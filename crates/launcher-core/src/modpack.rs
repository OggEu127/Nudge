//! Executes inspected MRPACK plans without moving business logic into frontends.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use launcher_modpack::{InstallPlan, LoaderKind, TaskKind};
use launcher_net::{
    engine::{DownloadEventKind, DownloadJob, DownloadManager},
    transfer::{Hashes, verify_content},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    instances::{
        InstallProgress, Instance, InstanceManager, check_no_links, confined_path,
        copy_content_verified, create_directory, validate_id,
    },
};

/// User selection for optional client files.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Bind execution to the exact archive the user reviewed, when supplied.
    pub expected_archive_sha256: Option<String>,
    /// Select all optional client files.
    pub include_all_optional: bool,
    /// Individually selected game-relative paths.
    pub optional_files: BTreeSet<PathBuf>,
}

/// Integrity of a final managed file, after applying override precedence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManagedPackFile {
    /// Path relative to this instance's game directory.
    pub path: PathBuf,
    /// Exact installed length.
    pub size: u64,
    /// Every expected digest is verified.
    pub hashes: Hashes,
    /// Mods and packaged assets are immutable; user configuration remains editable.
    pub verify_on_launch: bool,
}

/// Pack provenance retained in the persistent instance record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackRecord {
    /// Original local archive, retained so interrupted installs can be resumed.
    #[serde(default)]
    pub source_archive: Option<PathBuf>,
    /// SHA-256 of the original inspected archive.
    pub archive_sha256: String,
    /// Human-readable pack name.
    pub name: String,
    /// Pack release ID.
    pub version: String,
    /// Stable optional selection, required for a safe resume.
    pub selected_optional: BTreeSet<PathBuf>,
    /// Final managed contents, including winning overrides.
    pub files: Vec<ManagedPackFile>,
}

impl InstanceManager {
    /// Inspects an archive on a blocking worker without downloading or installing.
    ///
    /// # Errors
    /// Rejects invalid instance IDs or unsafe/unsupported archives.
    pub async fn inspect_modpack(&self, path: &Path, id: &str) -> Result<InstallPlan> {
        let source = path.to_owned();
        let target = self.game_directory(id)?;
        let plan = tokio::task::spawn_blocking(move || launcher_modpack::inspect(&source, target))
            .await
            .map_err(|e| Error::InvalidInput(format!("pack inspection worker failed: {e}")))??;
        validate_loader(&plan)?;
        Ok(plan)
    }

    /// Imports a client MRPACK into a new instance, or resumes the identical pack.
    ///
    /// # Errors
    /// Rejects unsupported dependencies, unrelated existing instances, changed
    /// archives/selections, cancellation, or any integrity/installation failure.
    pub async fn import_modpack(
        &self,
        path: &Path,
        id: &str,
        options: ImportOptions,
        cancel: CancellationToken,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        if cancel.is_cancelled() {
            return Err(launcher_net::Error::Cancelled.into());
        }
        validate_id(id)?;
        let plan = self.inspect_modpack(path, id).await?;
        if options
            .expected_archive_sha256
            .as_ref()
            .is_some_and(|hash| !hash.eq_ignore_ascii_case(&plan.archive_sha256))
        {
            return Err(Error::InvalidInput(
                "the modpack archive changed after inspection; inspect and confirm it again".into(),
            ));
        }
        validate_supported(&plan)?;
        let mut record = pack_record(&plan, &options)?;
        record.source_archive = Some(std::path::absolute(path).map_err(|source| Error::Io {
            operation: "resolve pack archive",
            path: path.to_owned(),
            source,
        })?);
        let _lease = self.acquire_instance(id).await?;
        let mut cached_metadata = self.resume_pack_metadata(id, &record).await?;
        let mut runtime = None;
        for task in plan.ordered_tasks()? {
            if cancel.is_cancelled() {
                return Err(launcher_net::Error::Cancelled.into());
            }
            match task {
                TaskKind::Runtime => {
                    let metadata = if let Some(metadata) = cached_metadata.take() {
                        metadata
                    } else {
                        let base = self.vanilla_metadata(&plan.minecraft_version).await?;
                        if let Some(loader) = &plan.loader {
                            launcher_minecraft::fabric::resolve_profile(
                                &self.http,
                                &base,
                                &loader.version,
                            )
                            .await?
                        } else {
                            base
                        }
                    };
                    let initial = Instance {
                        schema_version: 1,
                        id: id.to_owned(),
                        name: plan.name.clone(),
                        minecraft_version: plan.minecraft_version.clone(),
                        metadata: metadata.clone(),
                        installed: false,
                        pack: Some(record.clone()),
                    };
                    self.save(&initial).await?;
                    runtime = Some(
                        self.prepare_profile(
                            id,
                            &plan.minecraft_version,
                            metadata,
                            cancel.clone(),
                            &progress,
                        )
                        .await?,
                    );
                }
                TaskKind::PackFiles => {
                    self.download_pack(&plan, id, &options, cancel.clone(), &progress)
                        .await?;
                }
                TaskKind::Overrides => self.apply_pack_overrides(path, &plan, id).await?,
                TaskKind::Verify => {
                    let instance = runtime.as_ref().ok_or_else(|| {
                        Error::InvalidInput(
                            "runtime task must finish before verification".to_owned(),
                        )
                    })?;
                    self.verify_runtime(instance).await?;
                    self.verify_pack(instance, true).await?;
                }
                TaskKind::Commit => {
                    let instance = runtime.as_mut().ok_or_else(|| {
                        Error::InvalidInput("runtime task must finish before commit".to_owned())
                    })?;
                    instance.installed = true;
                    self.save(instance).await?;
                }
            }
        }
        runtime
            .filter(|instance| instance.installed)
            .ok_or_else(|| {
                Error::InvalidInput("installation plan did not commit an instance".to_owned())
            })
    }

    /// Resumes a persisted incomplete installation with its original metadata.
    ///
    /// # Errors
    /// Requires the original archive for pack overrides, and reports normal
    /// integrity, cancellation and network errors without bypassing checks.
    pub async fn resume_instance(
        &self,
        id: &str,
        cancel: CancellationToken,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        let existing = self.load(id).await?;
        if let Some(pack) = &existing.pack {
            let archive = pack.source_archive.as_deref().ok_or_else(|| {
                Error::InvalidInput(
                    "original archive path is unavailable; re-import the same archive to resume"
                        .to_owned(),
                )
            })?;
            let options = ImportOptions {
                expected_archive_sha256: Some(pack.archive_sha256.clone()),
                include_all_optional: false,
                optional_files: pack.selected_optional.clone(),
            };
            self.import_modpack(archive, id, options, cancel, progress)
                .await
        } else {
            self.install_profile_controlled(
                id,
                &existing.minecraft_version,
                existing.metadata,
                cancel,
                progress,
            )
            .await
        }
    }

    async fn resume_pack_metadata(
        &self,
        id: &str,
        record: &PackRecord,
    ) -> Result<Option<launcher_minecraft::VersionMetadata>> {
        let record_path = self.record_path(id)?;
        if tokio::fs::try_exists(&record_path)
            .await
            .map_err(|source| Error::Io {
                operation: "inspect import destination",
                path: record_path,
                source,
            })?
        {
            let existing = self.load(id).await?;
            let previous = existing.pack.as_ref().ok_or_else(|| {
                Error::InvalidInput(
                    "choose an unused instance ID; an unrelated instance already exists".to_owned(),
                )
            })?;
            if previous.archive_sha256 != record.archive_sha256
                || previous.selected_optional != record.selected_optional
            {
                return Err(Error::InvalidInput("this instance belongs to another archive or optional selection; choose a new instance ID".to_owned()));
            }
            return Ok(Some(existing.metadata));
        }
        let game = self.game_directory(id)?;
        check_no_links(&game).await?;
        match tokio::fs::read_dir(&game).await {
            Ok(mut directory) => {
                if directory
                    .next_entry()
                    .await
                    .map_err(|source| Error::Io {
                        operation: "inspect unmanaged game directory",
                        path: game.clone(),
                        source,
                    })?
                    .is_some()
                {
                    return Err(Error::InvalidInput(
                        "target game directory contains unmanaged files; choose a new instance ID"
                            .to_owned(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    operation: "inspect unmanaged game directory",
                    path: game,
                    source,
                });
            }
        }
        Ok(None)
    }

    async fn download_pack(
        &self,
        plan: &InstallPlan,
        id: &str,
        options: &ImportOptions,
        cancel: CancellationToken,
        progress: &impl Fn(InstallProgress),
    ) -> Result<()> {
        let selected = selected_files(plan, options)?;
        let shadowed: BTreeSet<_> = plan.overrides.iter().map(|file| &file.path).collect();
        let mut jobs = Vec::new();
        let mut paths = BTreeMap::new();
        for file in selected {
            if shadowed.contains(&file.path) {
                continue;
            }
            let destination = PathBuf::from("instances")
                .join(id)
                .join("game")
                .join(&file.path);
            confined_path(&self.root, &destination).await?;
            let task_id = format!(
                "pack:{id}:{}",
                hex::encode(Sha1::digest(file.path.to_string_lossy().as_bytes()))
            );
            paths.insert(task_id.clone(), file.path.clone());
            jobs.push(DownloadJob {
                id: task_id,
                spec: file.spec.clone(),
                mirrors: file.mirrors.clone(),
                destination,
                priority: 1,
            });
        }
        let total = jobs.len();
        let engine =
            DownloadManager::with_http(self.root.clone(), self.concurrency, self.http.clone())?;
        let (sender, mut receiver) = tokio::sync::broadcast::channel(512);
        let run = engine.run(jobs, cancel, sender);
        tokio::pin!(run);
        let mut completed = 0;
        loop {
            tokio::select! {
                result=&mut run=>{result?;break;},
                event=receiver.recv()=>if let Ok(event)=event && let DownloadEventKind::Completed{reused}=event.kind {
                    completed+=1;progress(InstallProgress{completed,total,path:paths.get(&event.id).cloned().unwrap_or_default(),reused});
                },
            }
        }
        progress(InstallProgress {
            completed: total,
            total,
            path: PathBuf::new(),
            reused: false,
        });
        Ok(())
    }

    async fn apply_pack_overrides(&self, path: &Path, plan: &InstallPlan, id: &str) -> Result<()> {
        let game = self.game_directory(id)?;
        create_directory(&game).await?;
        let stage = tempfile::tempdir_in(
            game.parent()
                .ok_or_else(|| Error::InvalidInput("instance has no parent".to_owned()))?,
        )
        .map_err(|source| Error::Io {
            operation: "create override stage",
            path: game.clone(),
            source,
        })?;
        let archive = path.to_owned();
        let plan_copy = plan.clone();
        let stage_path = stage.path().to_owned();
        tokio::task::spawn_blocking(move || {
            launcher_modpack::extract_overrides(&archive, &plan_copy, &stage_path)
        })
        .await
        .map_err(|e| Error::InvalidInput(format!("override worker failed: {e}")))??;
        for file in &plan.overrides {
            let target = confined_path(&game, &file.path).await?;
            if verify_content(&target, Some(file.size), &file.hashes).await? {
                continue;
            }
            copy_content_verified(
                &stage.path().join(&file.path),
                &target,
                file.size,
                &file.hashes,
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn verify_pack(&self, instance: &Instance, all: bool) -> Result<()> {
        if let Some(pack) = &instance.pack {
            let game = self.game_directory(&instance.id)?;
            check_no_links(&game).await?;
            for file in &pack.files {
                if !all && !file.verify_on_launch {
                    continue;
                }
                let path = confined_path(&game, &file.path).await?;
                if !verify_content(&path, Some(file.size), &file.hashes).await? {
                    return Err(Error::InvalidInput(format!(
                        "missing or corrupt managed pack file `{}`; resume import with the original archive to repair",
                        path.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_loader(plan: &InstallPlan) -> Result<()> {
    if plan
        .loader
        .as_ref()
        .is_some_and(|loader| loader.kind != LoaderKind::Fabric)
    {
        return Err(Error::InvalidInput("this loader is recognized but not supported yet; this build installs vanilla and Fabric packs".to_owned()));
    }
    Ok(())
}

fn validate_supported(plan: &InstallPlan) -> Result<()> {
    if !plan.unsupported_dependencies.is_empty() {
        return Err(Error::InvalidInput(format!(
            "unsupported pack dependency keys: {}",
            plan.unsupported_dependencies
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    validate_loader(plan)
}

fn selected_files<'a>(
    plan: &'a InstallPlan,
    options: &ImportOptions,
) -> Result<Vec<&'a launcher_modpack::PlannedFile>> {
    for selected in &options.optional_files {
        if !plan
            .optional_files
            .iter()
            .any(|file| &file.path == selected)
        {
            return Err(Error::InvalidInput(format!(
                "unknown optional file `{}`",
                selected.display()
            )));
        }
    }
    Ok(plan
        .required_files
        .iter()
        .chain(plan.optional_files.iter().filter(|file| {
            options.include_all_optional || options.optional_files.contains(&file.path)
        }))
        .collect())
}

fn pack_record(plan: &InstallPlan, options: &ImportOptions) -> Result<PackRecord> {
    let mut files = BTreeMap::new();
    for file in selected_files(plan, options)? {
        files.insert(
            file.path.clone(),
            ManagedPackFile {
                path: file.path.clone(),
                size: file.spec.size.ok_or_else(|| {
                    Error::InvalidInput("MRPACK file has no byte length".to_owned())
                })?,
                hashes: file.spec.hashes.clone(),
                verify_on_launch: immutable(&file.path),
            },
        );
    }
    for file in &plan.overrides {
        files.insert(
            file.path.clone(),
            ManagedPackFile {
                path: file.path.clone(),
                size: file.size,
                hashes: file.hashes.clone(),
                verify_on_launch: immutable(&file.path),
            },
        );
    }
    let selected_optional = plan
        .optional_files
        .iter()
        .filter(|file| options.include_all_optional || options.optional_files.contains(&file.path))
        .map(|file| file.path.clone())
        .collect();
    Ok(PackRecord {
        source_archive: None,
        archive_sha256: plan.archive_sha256.clone(),
        name: plan.name.clone(),
        version: plan.version.clone(),
        selected_optional,
        files: files.into_values().collect(),
    })
}

fn immutable(path: &Path) -> bool {
    path.starts_with("mods")
        || path.starts_with("resourcepacks")
        || path.starts_with("shaderpacks")
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
}
