//! Isolated instance installation and verification services.

use std::{
    io::Write as _,
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
};

use launcher_minecraft::{
    VersionMetadata,
    java::{discover_java, required_java_major},
    launch::{
        GameProcess, LaunchIdentity, LaunchOptions, ProcessEvent, ProcessExit, build_launch_spec,
        spawn_game_with_resource,
    },
    metadata::DownloadArtifact,
    resolver::{self, AssetIndex, ResolvedVersion},
    rules::Platform,
};
use launcher_net::{
    HttpClient as _, ReqwestHttpClient,
    engine::{DownloadEventKind, DownloadJob, DownloadManager},
    transfer::{FileSpec, Hashes, verify_file},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

const DEFAULT_CONCURRENCY: NonZeroUsize = NonZeroUsize::new(8).expect("eight is nonzero");

/// Persistent, portable description of an installed or resumable instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    /// Instance record schema version.
    pub schema_version: u32,
    /// Safe local instance ID.
    pub id: String,
    /// User-visible name.
    pub name: String,
    /// Mojang base version ID.
    pub minecraft_version: String,
    /// Effective merged version metadata.
    pub metadata: VersionMetadata,
    /// Whether every required installation operation has completed.
    pub installed: bool,
    /// Source pack identity and managed file integrity, if imported.
    #[serde(default)]
    pub pack: Option<crate::modpack::PackRecord>,
}

/// Incremental installation status for frontends.
#[derive(Clone, Debug)]
pub struct InstallProgress {
    /// Completed file count.
    pub completed: usize,
    /// Total required files.
    pub total: usize,
    /// Current relative file path.
    pub path: PathBuf,
    /// Whether the existing file passed integrity verification.
    pub reused: bool,
}

/// Application filesystem and installation coordinator.
#[derive(Clone, Debug)]
pub struct InstanceManager {
    pub(crate) root: PathBuf,
    pub(crate) http: ReqwestHttpClient,
    pub(crate) concurrency: NonZeroUsize,
    asset_source: Option<url::Url>,
}

/// A supervised game process retaining its private native-library directory.
pub struct RunningInstance {
    process: GameProcess,
}

impl RunningInstance {
    /// Receives process output and lifecycle events. Drain while the game runs.
    pub async fn next_event(&mut self) -> Option<ProcessEvent> {
        self.process.next_event().await
    }

    /// Requests termination of the game process.
    #[must_use]
    pub fn cancel(&self) -> bool {
        self.process.cancel()
    }

    /// Drains output and waits for child termination.
    ///
    /// # Errors
    /// Returns a process management error if supervision failed.
    pub async fn wait(self) -> Result<ProcessExit> {
        Ok(self.process.wait().await?)
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

impl InstanceManager {
    /// Constructs a service with an explicitly configured HTTP policy.
    #[must_use]
    pub fn with_http(root: impl Into<PathBuf>, http: ReqwestHttpClient) -> Self {
        Self {
            root: root.into(),
            http,
            concurrency: DEFAULT_CONCURRENCY,
            asset_source: None,
        }
    }

    /// Applies a bounded download concurrency preference.
    #[must_use]
    pub fn with_concurrency(mut self, concurrency: NonZeroUsize) -> Self {
        self.concurrency = concurrency;
        self
    }

    /// Selects an explicit resource mirror while retaining Mojang asset hashes.
    ///
    /// The HTTP policy still validates every request. This also provides an
    /// offline HTTP test seam without permitting public access from test clients.
    #[must_use]
    pub fn with_asset_source(mut self, mut source: url::Url) -> Self {
        if !source.path().ends_with('/') {
            source.set_path(&format!("{}/", source.path()));
        }
        self.asset_source = Some(source);
        self
    }
    /// Verifies, selects Java, prepares private natives, and starts one instance.
    ///
    /// # Errors
    /// Returns an error for incomplete installation, bad files, Java mismatch,
    /// unsafe native archives, invalid identity, or child process failure.
    pub async fn launch(
        &self,
        id: &str,
        identity: &LaunchIdentity,
        options: LaunchOptions,
        explicit_java: Option<&Path>,
    ) -> Result<RunningInstance> {
        self.launch_controlled(
            id,
            identity,
            options,
            explicit_java,
            &CancellationToken::new(),
        )
        .await
    }

    /// Prepares a game with cancellation without releasing ownership underneath
    /// a still-running native extraction worker.
    ///
    /// Blocking extraction finishes safely before cancellation returns. After
    /// spawning, the supervisor retains the native directory and instance lease
    /// even if a frontend drops the returned handle.
    ///
    /// # Errors
    /// Returns cancellation or the same validation/runtime errors as [`Self::launch`].
    pub async fn launch_controlled(
        &self,
        id: &str,
        identity: &LaunchIdentity,
        mut options: LaunchOptions,
        explicit_java: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<RunningInstance> {
        let prepare = async {
            // A missing or damaged record must fail before lock preparation
            // creates a new instance directory.
            self.load(id).await?;
            let lease = self.acquire_instance(id).await?;
            let instance = self.load(id).await?;
            let plan = self.verify(&instance).await?;
            let runtime =
                discover_java(required_java_major(&instance.metadata), explicit_java).await?;
            Ok::<_, Error>((lease, instance, plan, runtime))
        };
        let (lease, instance, plan, runtime) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(launcher_net::Error::Cancelled.into()),
            result = prepare => result?,
        };
        options.java = runtime.executable;
        let game = self.game_directory(id)?;
        let runtime_directory = game
            .parent()
            .ok_or_else(|| Error::InvalidInput("game has no instance directory".to_owned()))?
            .join("runtime");
        create_directory(&runtime_directory).await?;
        let temporary = tempfile::tempdir_in(&runtime_directory)
            .map_err(|e| io_error("create private natives", &runtime_directory, e))?;
        let natives = temporary.path().join("natives");
        let root = self.root.clone();
        let archives = plan.natives.clone();
        let output = natives.clone();
        // Keep ownership in the non-abortable worker, not its waiting caller.
        let (temporary, lease) = tokio::task::spawn_blocking(move || {
            launcher_minecraft::natives::extract_natives(&root, &archives, &output)?;
            Ok::<_, launcher_minecraft::Error>((temporary, lease))
        })
        .await
        .map_err(|e| Error::InvalidInput(format!("native extraction task failed: {e}")))??;
        if cancel.is_cancelled() {
            return Err(launcher_net::Error::Cancelled.into());
        }
        let root = std::fs::canonicalize(&self.root)
            .map_err(|e| io_error("resolve shared directory", &self.root, e))?;
        let game = std::fs::canonicalize(&game)
            .map_err(|e| io_error("resolve game directory", &game, e))?;
        let natives = std::fs::canonicalize(&natives)
            .map_err(|e| io_error("resolve natives directory", &natives, e))?;
        let spec = build_launch_spec(
            &instance.metadata,
            &plan,
            &root,
            &game,
            &natives,
            identity,
            &options,
        )?;
        let process = spawn_game_with_resource(spec, (temporary, lease))?;
        if cancel.is_cancelled() {
            let _ = process.cancel();
            process.wait().await?;
            return Err(launcher_net::Error::Cancelled.into());
        }
        Ok(RunningInstance { process })
    }
    /// Constructs an instance service with shared objects under `root`.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            root: root.into(),
            http: ReqwestHttpClient::new(
                concat!("Nudge/", env!("CARGO_PKG_VERSION")),
                launcher_net::DEFAULT_MAX_RESPONSE_BYTES,
            )?,
            concurrency: DEFAULT_CONCURRENCY,
            asset_source: None,
        })
    }

    /// Returns the shared application data root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns an isolated game directory after validating the instance ID.
    ///
    /// # Errors
    /// Rejects unsafe instance IDs.
    pub fn game_directory(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join("instances").join(id).join("game"))
    }

    /// Reads one persistent instance record.
    ///
    /// # Errors
    /// Returns an error for missing, malformed, or unsupported records.
    pub async fn load(&self, id: &str) -> Result<Instance> {
        let path = self.record_path(id)?;
        check_no_links(&path).await?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| io_error("read instance record", &path, e))?;
        let record: Instance = serde_json::from_slice(&bytes)?;
        if record.schema_version != 1 || record.id != id {
            return Err(Error::InvalidInput(format!(
                "invalid instance record `{id}`"
            )));
        }
        Ok(record)
    }

    /// Lists instance records in deterministic order.
    ///
    /// # Errors
    /// Returns an error if the instance directory cannot be enumerated.
    /// Unreadable or invalid individual records are omitted with a warning.
    pub async fn list(&self) -> Result<Vec<Instance>> {
        let directory = self.root.join("instances");
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error("list instances", &directory, error)),
        };
        let mut instances = Vec::new();
        let mut skipped = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| io_error("list instances", &directory, e))?
        {
            let directory = entry.path();
            let Ok(kind) = entry.file_type().await else {
                skipped += 1;
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let Ok(id) = entry.file_name().into_string() else {
                skipped += 1;
                continue;
            };
            match tokio::fs::try_exists(directory.join("instance.json")).await {
                Ok(true) => match self.load(&id).await {
                    Ok(instance) => instances.push(instance),
                    Err(_) => skipped += 1,
                },
                Ok(false) => {}
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            tracing::warn!(
                skipped,
                "unreadable or invalid instance records omitted from list"
            );
        }
        instances.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(instances)
    }

    /// Installs a vanilla client and retains an incomplete record on failure.
    ///
    /// # Errors
    /// Returns an error on metadata, filesystem, or verified transfer failures.
    pub async fn install_vanilla(
        &self,
        id: &str,
        version: &str,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        self.install_vanilla_controlled(id, version, CancellationToken::new(), progress)
            .await
    }

    /// Installs vanilla with cooperative cancellation and persisted downloads.
    ///
    /// # Errors
    /// Returns contextual metadata, download, cancellation, or filesystem errors.
    pub async fn install_vanilla_controlled(
        &self,
        id: &str,
        version: &str,
        cancel: CancellationToken,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        validate_id(id)?;
        let metadata = self.vanilla_metadata(version).await?;
        self.install_profile_controlled(id, version, metadata, cancel, progress)
            .await
    }

    pub(crate) async fn vanilla_metadata(&self, version: &str) -> Result<VersionMetadata> {
        let manifest = launcher_minecraft::fetch_version_manifest(&self.http).await?;
        let entry = manifest
            .find(version)
            .ok_or_else(|| Error::InvalidInput(format!("Mojang has no version `{version}`")))?;
        let bytes = self.http.get_bytes(&entry.url).await?;
        if let Some(hash) = &entry.sha1 {
            let actual = hex::encode(Sha1::digest(&bytes));
            if !hash.eq_ignore_ascii_case(&actual) {
                return Err(Error::InvalidInput(format!(
                    "version metadata hash mismatch for `{version}`"
                )));
            }
        }
        let metadata = launcher_minecraft::parse_version_metadata(&bytes)?;
        if metadata.id != version {
            return Err(Error::InvalidInput(
                "version metadata ID mismatch".to_owned(),
            ));
        }
        Ok(metadata)
    }

    /// Installs an exact Fabric Loader version into an isolated instance.
    ///
    /// # Errors
    /// Propagates official metadata, checksum, download and installation errors.
    pub async fn install_fabric(
        &self,
        id: &str,
        game: &str,
        loader: &str,
        cancel: CancellationToken,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        validate_id(id)?;
        let base = self.vanilla_metadata(game).await?;
        let metadata =
            launcher_minecraft::fabric::resolve_profile(&self.http, &base, loader).await?;
        self.install_profile_controlled(id, game, metadata, cancel, progress)
            .await
    }

    /// Installs an already resolved profile through the common artifact pipeline.
    ///
    /// # Errors
    /// Rejects collisions with a different profile and propagates installation failures.
    pub async fn install_profile(
        &self,
        id: &str,
        base_version: &str,
        metadata: VersionMetadata,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        self.install_profile_controlled(
            id,
            base_version,
            metadata,
            CancellationToken::new(),
            progress,
        )
        .await
    }

    /// Installs effective metadata with cancellable durable transfers.
    ///
    /// # Errors
    /// Returns an error on collisions, cancellation, or failed installation operations.
    pub async fn install_profile_controlled(
        &self,
        id: &str,
        base_version: &str,
        metadata: VersionMetadata,
        cancel: CancellationToken,
        progress: impl Fn(InstallProgress),
    ) -> Result<Instance> {
        validate_id(id)?;
        let _lease = self.acquire_instance(id).await?;
        if tokio::fs::try_exists(self.record_path(id)?)
            .await
            .map_err(|e| io_error("inspect instance", &self.root, e))?
            && self.load(id).await?.pack.is_some()
        {
            return Err(Error::InvalidInput(
                "this is a modpack instance; resume it using its original archive".to_owned(),
            ));
        }
        let mut instance = self
            .prepare_profile(id, base_version, metadata, cancel, &progress)
            .await?;
        instance.installed = true;
        self.save(&instance).await?;
        Ok(instance)
    }

    pub(crate) async fn prepare_profile(
        &self,
        id: &str,
        base_version: &str,
        metadata: VersionMetadata,
        cancel: CancellationToken,
        progress: &impl Fn(InstallProgress),
    ) -> Result<Instance> {
        let path = self.record_path(id)?;
        let mut pack = None;
        let mut name = id.to_owned();
        if tokio::fs::try_exists(&path)
            .await
            .map_err(|e| io_error("inspect instance", &path, e))?
        {
            let existing = self.load(id).await?;
            if existing.metadata.id != metadata.id || existing.minecraft_version != base_version {
                return Err(Error::InvalidInput(format!(
                    "instance `{id}` already belongs to another profile"
                )));
            }
            pack = existing.pack;
            name = existing.name;
        }
        let game = self.game_directory(id)?;
        create_directory(&game).await?;
        let instance = Instance {
            schema_version: 1,
            id: id.to_owned(),
            name,
            minecraft_version: base_version.to_owned(),
            metadata,
            installed: false,
            pack,
        };
        self.save(&instance).await?;
        let index = self.asset_index(&instance.metadata, true).await?;
        let plan = self.resolve_plan(&instance.metadata, &index)?;
        self.download_plan(&plan, cancel, progress).await?;
        self.materialize_assets(&index, &plan.asset_index_id, &game)
            .await?;
        Ok(instance)
    }

    async fn download_plan(
        &self,
        plan: &ResolvedVersion,
        cancel: CancellationToken,
        progress: &impl Fn(InstallProgress),
    ) -> Result<()> {
        let engine =
            DownloadManager::with_http(self.root.clone(), self.concurrency, self.http.clone())?;
        let mut paths = std::collections::BTreeMap::new();
        let mut jobs = Vec::with_capacity(plan.files.len());
        for file in &plan.files {
            confined_path(&self.root, &file.path).await?;
            let id = hex::encode(Sha1::digest(file.path.to_string_lossy().as_bytes()));
            paths.insert(id.clone(), file.path.clone());
            jobs.push(DownloadJob {
                id,
                spec: artifact_spec(&file.artifact)?,
                mirrors: Vec::new(),
                destination: file.path.clone(),
                priority: 0,
            });
        }
        let (events, mut receiver) = tokio::sync::broadcast::channel(512);
        let run = engine.run(jobs, cancel, events);
        tokio::pin!(run);
        let mut completed = 0;
        loop {
            tokio::select! {
                result = &mut run => { result?; break; },
                event = receiver.recv() => {
                    if let Ok(event) = event && let DownloadEventKind::Completed { reused } = event.kind {
                        completed += 1;
                        if let Some(path) = paths.get(&event.id) {
                            progress(InstallProgress { completed, total:plan.files.len(), path:path.clone(), reused });
                        }
                    }
                }
            }
        }
        while let Ok(event) = receiver.try_recv() {
            if let DownloadEventKind::Completed { reused } = event.kind {
                completed += 1;
                if let Some(path) = paths.get(&event.id) {
                    progress(InstallProgress {
                        completed,
                        total: plan.files.len(),
                        path: path.clone(),
                        reused,
                    });
                }
            }
        }
        if completed < plan.files.len() {
            progress(InstallProgress {
                completed: plan.files.len(),
                total: plan.files.len(),
                path: PathBuf::new(),
                reused: false,
            });
        }
        Ok(())
    }

    /// Verifies every required file before returning the current platform plan.
    ///
    /// # Errors
    /// Returns an error naming the first missing or corrupted required file.
    pub async fn verify(&self, instance: &Instance) -> Result<ResolvedVersion> {
        if !instance.installed {
            return Err(Error::InvalidInput(format!(
                "instance `{}` has an incomplete installation; reinstall to resume",
                instance.id
            )));
        }
        let plan = self.verify_runtime(instance).await?;
        self.verify_pack(instance, false).await?;
        Ok(plan)
    }

    pub(crate) async fn verify_runtime(&self, instance: &Instance) -> Result<ResolvedVersion> {
        let index = self.asset_index(&instance.metadata, false).await?;
        let plan = self.resolve_plan(&instance.metadata, &index)?;
        for file in &plan.files {
            let path = confined_path(&self.root, &file.path).await?;
            if !verify_file(&path, &artifact_spec(&file.artifact)?).await? {
                return Err(Error::InvalidInput(format!(
                    "missing or corrupt required file `{}`; reinstall to repair",
                    path.display()
                )));
            }
        }
        self.materialize_assets(
            &index,
            &plan.asset_index_id,
            &self.game_directory(&instance.id)?,
        )
        .await?;
        Ok(plan)
    }

    fn resolve_plan(
        &self,
        metadata: &VersionMetadata,
        index: &AssetIndex,
    ) -> Result<ResolvedVersion> {
        let mut plan = resolver::resolve(metadata, index, &Platform::host())?;
        if let Some(source) = &self.asset_source {
            for file in &mut plan.files {
                if file.path.starts_with("assets/objects") {
                    let hash = file
                        .artifact
                        .sha1
                        .as_deref()
                        .ok_or_else(|| Error::InvalidInput("asset has no hash".to_owned()))?;
                    file.artifact.url =
                        source
                            .join(&format!("{}/{hash}", &hash[..2]))
                            .map_err(|error| {
                                Error::InvalidInput(format!("invalid asset mirror: {error}"))
                            })?;
                }
            }
        }
        Ok(plan)
    }

    async fn asset_index(&self, metadata: &VersionMetadata, download: bool) -> Result<AssetIndex> {
        let index = metadata
            .asset_index
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("profile has no asset index".to_owned()))?;
        let relative = resolver::safe_relative_path(&format!("assets/indexes/{}.json", index.id))?;
        let path = confined_path(&self.root, &relative).await?;
        let spec = FileSpec {
            url: index.url.clone(),
            size: Some(index.size),
            hashes: Hashes {
                sha1: Some(index.sha1.clone()),
                ..Hashes::default()
            },
        };
        if download {
            self.http.download_verified(&spec, &path).await?;
        } else if !verify_file(&path, &spec).await? {
            return Err(Error::InvalidInput(format!(
                "asset index `{}` is missing or corrupt",
                index.id
            )));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| io_error("read asset index", &path, e))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn materialize_assets(
        &self,
        index: &AssetIndex,
        index_id: &str,
        game: &Path,
    ) -> Result<()> {
        if !index.virtual_assets && !index.map_to_resources {
            return Ok(());
        }
        for (name, object) in &index.objects {
            let hash = object.hash.to_ascii_lowercase();
            let source = self
                .root
                .join("assets/objects")
                .join(&hash[..2])
                .join(&hash);
            let relative = resolver::safe_relative_path(name)?;
            let mut destinations = Vec::new();
            if index.virtual_assets {
                destinations.push(
                    self.root
                        .join("assets/virtual")
                        .join(index_id)
                        .join(&relative),
                );
            }
            if index.map_to_resources {
                destinations.push(game.join("resources").join(&relative));
            }
            for target in destinations {
                check_no_links(&target).await?;
                if let Some(parent) = target.parent() {
                    create_directory(parent).await?;
                }
                let spec = FileSpec {
                    url: url::Url::parse("https://resources.download.minecraft.net/")
                        .map_err(|e| Error::InvalidInput(e.to_string()))?,
                    size: Some(object.size),
                    hashes: Hashes {
                        sha1: Some(object.hash.clone()),
                        ..Hashes::default()
                    },
                };
                if !verify_file(&target, &spec).await? {
                    atomic_copy_verified(&source, &target, &spec).await?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn record_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join("instances").join(id).join("instance.json"))
    }

    pub(crate) async fn acquire_instance(&self, id: &str) -> Result<std::fs::File> {
        validate_id(id)?;
        let instances = self.root.join("instances");
        let mut entries = match tokio::fs::read_dir(&instances).await {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error("inspect instance IDs", &instances, error)),
        };
        if let Some(entries) = &mut entries {
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|error| io_error("inspect instance IDs", &instances, error))?
            {
                let Some(existing) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if existing != id && existing.eq_ignore_ascii_case(id) {
                    return Err(Error::InvalidInput(format!(
                        "instance ID `{id}` conflicts with existing `{existing}` on case-insensitive filesystems"
                    )));
                }
            }
        }
        let directory = instances.join(id);
        create_directory(&directory).await?;
        let path = directory.join("instance.lock");
        check_no_links(&path).await?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io_error("open instance lock", &path, e))?;
        file.try_lock().map_err(|e| {
            Error::InvalidInput(format!("instance `{id}` is busy or cannot be locked: {e}"))
        })?;
        Ok(file)
    }

    /// Atomically persists a checked instance record.
    ///
    /// # Errors
    /// Returns an error on invalid IDs or filesystem failure.
    pub(crate) async fn save(&self, instance: &Instance) -> Result<()> {
        let path = self.record_path(&instance.id)?;
        atomic_json(&path, instance).await
    }
}

/// Converts a metadata artifact to a mandatory hash-verified transfer spec.
///
/// # Errors
/// Returns an error when no expected digest is published.
pub fn artifact_spec(artifact: &DownloadArtifact) -> Result<FileSpec> {
    let hashes = Hashes {
        sha1: artifact.sha1.clone(),
        ..Hashes::default()
    };
    hashes.validate()?;
    Ok(FileSpec {
        url: artifact.url.clone(),
        size: artifact.size,
        hashes,
    })
}

/// Validates an instance identifier across supported filesystem conventions.
///
/// # Errors
/// Returns an error for path separators, device names, and unsafe identifiers.
pub fn validate_id(id: &str) -> Result<()> {
    let path = resolver::safe_relative_path(id)?;
    if id.len() > 80
        || path.components().count() != 1
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err(Error::InvalidInput(
            "instance ID must be 1-80 ASCII letters, numbers, dots, hyphens or underscores"
                .to_owned(),
        ));
    }
    Ok(())
}

pub(crate) async fn create_directory(path: &Path) -> Result<()> {
    check_no_links(path).await?;
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| io_error("create directory", path, e))?;
    check_no_links(path).await
}

/// Rejects symbolic links and Windows reparse points in an entire path.
///
/// Missing components are allowed. This is a static check, not a filesystem
/// sandbox: managed directories must not be concurrently writable by hostile
/// processes. Callers committing files must recheck and use atomic replacement.
///
/// # Errors
/// Returns a contextual filesystem error or rejects an unsafe path component.
pub async fn check_no_links(path: &Path) -> Result<()> {
    let absolute = std::path::absolute(path).map_err(|e| io_error("resolve path", path, e))?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        // A bare Windows prefix is not a directory. Keep it for the next
        // RootDir component, which must still undergo the normal link check.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => {
                return Err(Error::InvalidInput(format!(
                    "link in installation path `{}`",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("inspect path", &current, error)),
        }
    }
    Ok(())
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x400 != 0
}

pub(crate) async fn confined_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let text = relative
        .components()
        .map(|component| {
            if let std::path::Component::Normal(name) = component {
                name.to_str()
                    .ok_or_else(|| Error::InvalidInput("non-Unicode relative path".to_owned()))
            } else {
                Err(Error::InvalidInput("unsafe relative path".to_owned()))
            }
        })
        .collect::<Result<Vec<_>>>()?
        .join("/");
    let relative = resolver::safe_relative_path(&text)?;
    create_directory(root).await?;
    check_no_links(root).await?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(meta) if meta.file_type().is_symlink() || is_reparse_point(&meta) => {
                return Err(Error::InvalidInput(format!(
                    "symlink in installation path `{}`",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error("inspect installation path", &current, e)),
        }
    }
    Ok(current)
}

pub(crate) async fn atomic_copy_verified(
    source: &Path,
    target: &Path,
    spec: &FileSpec,
) -> Result<()> {
    let size = spec
        .size
        .ok_or_else(|| Error::InvalidInput("copy has no expected size".to_owned()))?;
    copy_content_verified(source, target, size, &spec.hashes).await
}

pub(crate) async fn copy_content_verified(
    source: &Path,
    target: &Path,
    expected_size: u64,
    hashes: &Hashes,
) -> Result<()> {
    use tokio::io::AsyncReadExt as _;

    check_no_links(source).await?;
    check_no_links(target).await?;
    let metadata = tokio::fs::symlink_metadata(source)
        .await
        .map_err(|error| io_error("inspect source asset", source, error))?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput(format!(
            "source asset `{}` is not an ordinary file",
            source.display()
        )));
    }
    let parent = target
        .parent()
        .ok_or_else(|| Error::InvalidInput("asset destination has no parent".to_owned()))?;
    create_directory(parent).await?;
    let temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create temporary asset copy", parent, error))?;
    let mut output = tokio::fs::File::from_std(
        temporary
            .reopen()
            .map_err(|error| io_error("open temporary asset copy", temporary.path(), error))?,
    );
    let input = tokio::fs::File::open(source)
        .await
        .map_err(|error| io_error("open source asset", source, error))?;
    let mut bounded_input = input.take(expected_size.saturating_add(1));
    let copied = tokio::io::copy(&mut bounded_input, &mut output)
        .await
        .map_err(|error| io_error("copy asset into temporary file", target, error))?;
    output
        .sync_all()
        .await
        .map_err(|error| io_error("flush temporary asset copy", temporary.path(), error))?;
    drop(output);
    if copied != expected_size
        || !launcher_net::transfer::verify_content(temporary.path(), Some(expected_size), hashes)
            .await?
    {
        return Err(launcher_net::Error::Integrity {
            path: target.to_path_buf(),
            reason: "asset copy does not match expected size or hash".to_owned(),
        }
        .into());
    }
    check_no_links(target).await?;
    // Replace the directory entry rather than truncating an existing inode:
    // user-created hard links must not let derived assets modify another file.
    launcher_net::persistence::persist(temporary, target)
        .map_err(|error| io_error("commit verified asset copy", target, error))?;
    Ok(())
}

async fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    check_no_links(path).await?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let parent = path
            .parent()
            .ok_or_else(|| Error::InvalidInput("record has no parent".to_owned()))?;
        launcher_net::persistence::create_dir_all(parent)
            .map_err(|e| io_error("create record directory", parent, e))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| io_error("create record temporary file", parent, e))?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|e| io_error("write record", &path, e))?;
        launcher_net::persistence::persist(temporary, &path)
            .map_err(|e| io_error("commit record", &path, e))?;
        Ok(())
    })
    .await
    .map_err(|e| Error::InvalidInput(format!("record persistence task failed: {e}")))?
}
