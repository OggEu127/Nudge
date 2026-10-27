//! Small async adapter between UI intentions and frontend-independent services.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use eframe::egui;
use launcher_core::{
    Launcher,
    auth::{Account, AuthService},
    config::LauncherConfig,
    instances::{InstallProgress, Instance, InstanceManager},
    modpack::ImportOptions,
};
use launcher_minecraft::{
    VersionEntry,
    launch::{LaunchIdentity, LaunchOptions, ProcessEvent},
};
use launcher_modpack::InstallPlan;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::errors::{Failure, FrontendError};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ContentKind {
    Mods,
    ResourcePacks,
    Worlds,
}

impl ContentKind {
    pub(crate) fn folder(self) -> &'static str {
        match self {
            Self::Mods => "mods",
            Self::ResourcePacks => "resourcepacks",
            Self::Worlds => "saves",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContentEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) size: Option<u64>,
    pub(crate) detail: String,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContentListing {
    pub(crate) instance: String,
    pub(crate) kind: ContentKind,
    pub(crate) folder: PathBuf,
    pub(crate) entries: Vec<ContentEntry>,
}

pub(crate) enum Action {
    Refresh,
    Versions,
    Inspect {
        archive: PathBuf,
        id: String,
    },
    Install {
        id: String,
        version: String,
        fabric: Option<String>,
    },
    Import {
        archive: PathBuf,
        id: String,
        optional: BTreeSet<PathBuf>,
        archive_sha256: String,
    },
    Resume(String),
    Verify(String),
    Launch {
        id: String,
        demo: bool,
    },
    Accounts,
    AddOffline(String),
    SignIn(String),
    SignOut(Uuid),
    Content {
        id: String,
        kind: ContentKind,
    },
    ImportCover {
        id: String,
        source: PathBuf,
    },
}

pub(crate) enum Outcome {
    Instances(Vec<Instance>),
    Versions(Vec<VersionEntry>),
    Plan {
        archive: PathBuf,
        id: String,
        plan: Box<InstallPlan>,
    },
    Installed(Box<Instance>),
    Message(String),
    Accounts(Vec<Account>),
    AccountCreated {
        account: Account,
        accounts: Vec<Account>,
    },
    SignedIn(Account),
    Content(ContentListing),
    CoverInstalled {
        id: String,
        path: PathBuf,
    },
}

pub(crate) enum Event {
    Progress(InstallProgress),
    Log(String),
    DeviceCode {
        verification_uri: String,
        user_code: String,
        expires_in: u64,
    },
    Finished(Result<Outcome, Failure>),
}

#[derive(Clone)]
pub(crate) struct EventSink {
    sender: mpsc::Sender<Event>,
    context: egui::Context,
}

impl EventSink {
    pub(crate) fn new(sender: mpsc::Sender<Event>, context: egui::Context) -> Self {
        Self { sender, context }
    }

    pub(crate) fn progress(&self, progress: InstallProgress) {
        // Intermediate progress is intentionally lossy under load. Terminal
        // events use awaited delivery and therefore cannot disappear.
        let _ = self.sender.try_send(Event::Progress(progress));
        self.context.request_repaint();
    }

    async fn send(&self, event: Event) {
        if self.sender.send(event).await.is_ok() {
            self.context.request_repaint();
        }
    }
}

pub(crate) fn start(
    runtime: &tokio::runtime::Handle,
    tracker: &TaskTracker,
    manager: InstanceManager,
    config: LauncherConfig,
    action: Action,
    cancel: CancellationToken,
    sink: EventSink,
) {
    let worker_sink = sink.clone();
    let worker = tracker.spawn_on(
        async move {
            Box::pin(execute(manager, config, action, cancel, &worker_sink))
                .await
                .map_err(|error| Failure::from_error(&error))
        },
        runtime,
    );
    tracker.spawn_on(
        async move {
            let result = worker.await.unwrap_or_else(|_| {
                Err(Failure::message(
                    "后台任务意外结束。现有安装恢复状态已保留，请稍后校验或继续安装。",
                ))
            });
            sink.send(Event::Finished(result)).await;
        },
        runtime,
    );
}

async fn execute(
    manager: InstanceManager,
    config: LauncherConfig,
    action: Action,
    cancel: CancellationToken,
    sink: &EventSink,
) -> Result<Outcome> {
    check_cancel(&cancel)?;
    let progress = |value| sink.progress(value);
    let result = match action {
        Action::Refresh => Outcome::Instances(tokio::select! {
            result = manager.list() => result?,
            () = cancel.cancelled() => bail!(FrontendError::Cancelled),
        }),
        Action::Versions => {
            let launcher = Launcher::new()?;
            let manifest = tokio::select! {
                result = launcher.version_manifest() => result?,
                () = cancel.cancelled() => bail!(FrontendError::Cancelled),
            };
            Outcome::Versions(manifest.versions)
        }
        Action::Inspect { archive, id } => {
            let plan = manager.inspect_modpack(&archive, &id).await?;
            check_cancel(&cancel)?;
            Outcome::Plan {
                archive,
                id,
                plan: Box::new(plan),
            }
        }
        Action::Install {
            id,
            version,
            fabric,
        } => install_game(&manager, &id, &version, fabric.as_deref(), cancel, sink).await?,
        Action::Import {
            archive,
            id,
            optional,
            archive_sha256,
        } => {
            let options = ImportOptions {
                include_all_optional: false,
                optional_files: optional,
                expected_archive_sha256: Some(archive_sha256),
            };
            Outcome::Installed(Box::new(
                manager
                    .import_modpack(&archive, &id, options, cancel, progress)
                    .await?,
            ))
        }
        Action::Resume(id) => Outcome::Installed(Box::new(
            manager.resume_instance(&id, cancel, progress).await?,
        )),
        Action::Verify(id) => {
            tokio::select! {
                result = async {
                    let instance = manager.load(&id).await?;
                    manager.verify(&instance).await
                } => { result?; },
                () = cancel.cancelled() => bail!(FrontendError::Cancelled),
            }
            Outcome::Message(format!("实例「{id}」的全部受管理文件已通过完整性校验。"))
        }
        Action::Launch { id, demo } => {
            return run_game(&manager, &id, demo, &config, &cancel, sink).await;
        }
        Action::Accounts => {
            // Account listing may initialize SQLite on a blocking worker. Let
            // that safe transaction finish before reporting cancellation.
            let accounts = AuthService::new(manager.root())?.accounts().await?;
            check_cancel(&cancel)?;
            Outcome::Accounts(accounts)
        }
        Action::AddOffline(name) => {
            let auth = AuthService::new(manager.root())?;
            let account = auth.add_offline(&name, &cancel).await?;
            let accounts = auth.accounts().await?;
            Outcome::AccountCreated { account, accounts }
        }
        Action::Content { id, kind } => {
            Outcome::Content(scan_content(&manager, &id, kind, &cancel).await?)
        }
        Action::ImportCover { id, source } => import_cover(&manager, id, &source, &cancel).await?,
        Action::SignIn(client_id) => {
            let auth = AuthService::new(manager.root())?;
            let authorization = auth.begin_login(&client_id, &cancel).await?;
            sink.send(Event::DeviceCode {
                verification_uri: authorization.verification_uri.to_string(),
                user_code: authorization.user_code.clone(),
                expires_in: authorization.expires_in,
            })
            .await;
            Outcome::SignedIn(auth.complete_login(authorization, &cancel).await?)
        }
        Action::SignOut(id) => {
            let auth = AuthService::new(manager.root())?;
            auth.logout(id).await?;
            Outcome::Accounts(auth.accounts().await?)
        }
    };
    Ok(result)
}

async fn import_cover(
    manager: &InstanceManager,
    id: String,
    source: &Path,
    cancel: &CancellationToken,
) -> Result<Outcome> {
    let path = crate::covers::install_custom_cover(manager.root(), &id, source, cancel).await?;
    Ok(Outcome::CoverInstalled { id, path })
}

async fn install_game(
    manager: &InstanceManager,
    id: &str,
    version: &str,
    fabric: Option<&str>,
    cancel: CancellationToken,
    sink: &EventSink,
) -> Result<Outcome> {
    let progress = |value| sink.progress(value);
    let instance = match fabric {
        Some(loader) => {
            manager
                .install_fabric(id, version, loader, cancel, progress)
                .await?
        }
        None => {
            manager
                .install_vanilla_controlled(id, version, cancel, progress)
                .await?
        }
    };
    Ok(Outcome::Installed(Box::new(instance)))
}

/// Lists instance-local content without following links or mutating files.
async fn scan_content(
    manager: &InstanceManager,
    id: &str,
    kind: ContentKind,
    cancel: &CancellationToken,
) -> Result<ContentListing> {
    let folder = manager.game_directory(id)?.join(kind.folder());
    launcher_core::instances::check_no_links(&folder).await?;
    let mut entries = Vec::new();
    let mut reader = match tokio::fs::read_dir(&folder).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ContentListing {
                instance: id.to_owned(),
                kind,
                folder,
                entries,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取目录 {}", folder.display()));
        }
    };
    while let Some(entry) = reader.next_entry().await? {
        check_cancel(cancel)?;
        let file_type = entry.file_type().await?;
        // A link inside a game directory could point outside the instance.
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Includes Windows junctions and other reparse points.
        if !safe_content_path(&path).await? {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let (include, size, detail, enabled) = match kind {
            ContentKind::Mods if file_type.is_file() => {
                if lower.ends_with(".jar.disabled") {
                    (
                        true,
                        Some(entry.metadata().await?.len()),
                        "已停用".to_owned(),
                        false,
                    )
                } else if has_extension(&lower, "jar") {
                    (
                        true,
                        Some(entry.metadata().await?.len()),
                        "Forge / Fabric 模组".to_owned(),
                        true,
                    )
                } else {
                    (false, None, String::new(), true)
                }
            }
            ContentKind::ResourcePacks if file_type.is_dir() => {
                (true, None, "资源包文件夹".to_owned(), true)
            }
            ContentKind::ResourcePacks if file_type.is_file() && has_extension(&lower, "zip") => (
                true,
                Some(entry.metadata().await?.len()),
                "ZIP 资源包".to_owned(),
                true,
            ),
            ContentKind::Worlds if file_type.is_dir() => {
                let level = path.join("level.dat");
                if !safe_content_path(&level).await? {
                    continue;
                }
                let include = tokio::fs::symlink_metadata(&level)
                    .await
                    .is_ok_and(|metadata| metadata.is_file());
                (include, None, "世界存档".to_owned(), true)
            }
            _ => (false, None, String::new(), true),
        };
        if include {
            entries.push(ContentEntry {
                name,
                path,
                size,
                detail,
                enabled,
            });
        }
    }
    entries.sort_by_key(|entry| entry.name.to_ascii_lowercase());
    launcher_core::instances::check_no_links(&folder).await?;
    Ok(ContentListing {
        instance: id.to_owned(),
        kind,
        folder,
        entries,
    })
}

async fn safe_content_path(path: &Path) -> Result<bool> {
    match launcher_core::instances::check_no_links(path).await {
        Ok(()) => Ok(true),
        Err(launcher_core::Error::InvalidInput(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!(FrontendError::Cancelled);
    }
    Ok(())
}

async fn run_game(
    manager: &InstanceManager,
    id: &str,
    demo: bool,
    config: &LauncherConfig,
    cancel: &CancellationToken,
    sink: &EventSink,
) -> Result<Outcome> {
    let options = LaunchOptions {
        demo,
        min_memory_mib: config.game.min_memory_mib,
        max_memory_mib: config.game.max_memory_mib,
        ..LaunchOptions::default()
    };
    let identity = if demo {
        LaunchIdentity::demo()
    } else {
        let id = config
            .auth
            .selected_account
            .as_deref()
            .context(FrontendError::MissingAccount)?;
        let id = Uuid::parse_str(id).context(FrontendError::InvalidSelectedAccount)?;
        AuthService::new(manager.root())?
            .identity(id, cancel)
            .await?
    };
    let mut running = manager
        .launch_controlled(
            id,
            &identity,
            options,
            config.game.java_executable.as_deref(),
            cancel,
        )
        .await?;
    let mut stopping = false;
    loop {
        tokio::select! {
            event = running.next_event() => match event {
                Some(ProcessEvent::Started { pid }) => sink.send(Event::Log(format!("Minecraft 已启动（进程 ID：{}）。", pid.map_or_else(|| "未知".to_owned(), |pid| pid.to_string())))).await,
                Some(ProcessEvent::Output { bytes, .. }) => sink.send(Event::Log(String::from_utf8_lossy(&bytes).into_owned())).await,
                Some(ProcessEvent::Exited(_)) | None => break,
            },
            () = cancel.cancelled(), if !stopping => {
                let _ = running.cancel();
                stopping = true;
            }
        }
    }
    let exit = running.wait().await?;
    if !exit.success && !exit.cancelled {
        bail!(FrontendError::GameExited(exit.code));
    }
    Ok(Outcome::Message(format!(
        "Minecraft 已停止（退出代码：{}，用户中止：{}）。",
        exit.code
            .map_or_else(|| "无".to_owned(), |code| code.to_string()),
        if exit.cancelled { "是" } else { "否" }
    )))
}

#[cfg(test)]
mod tests {
    use super::{ContentKind, Event, EventSink, Outcome, scan_content};
    use launcher_core::instances::{InstallProgress, InstanceManager};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn cover_import_is_a_tracked_background_action() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let manager = InstanceManager::new(root.path())?;
        tokio::fs::create_dir_all(root.path().join("instances/demo")).await?;
        let source = root.path().join("source.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255])).save(&source)?;
        let (sender, mut receiver) = mpsc::channel(4);
        let sink = EventSink::new(sender, eframe::egui::Context::default());
        let tracker = tokio_util::task::TaskTracker::new();
        super::start(
            &tokio::runtime::Handle::current(),
            &tracker,
            manager,
            launcher_core::config::LauncherConfig::default(),
            super::Action::ImportCover {
                id: "demo".into(),
                source,
            },
            CancellationToken::new(),
            sink,
        );
        let event =
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await?;
        assert!(
            matches!(event, Some(Event::Finished(Ok(Outcome::CoverInstalled { id, path }))) if id == "demo" && path.is_file())
        );
        tracker.close();
        tracker.wait().await;
        Ok(())
    }

    #[tokio::test]
    async fn saturated_progress_queue_never_loses_a_terminal_event() -> anyhow::Result<()> {
        let (sender, mut receiver) = mpsc::channel(1);
        let sink = EventSink::new(sender, eframe::egui::Context::default());
        for completed in 1..=2 {
            sink.progress(InstallProgress {
                completed,
                total: 2,
                path: "file".into(),
                reused: false,
            });
        }
        let worker = tokio::spawn(async move {
            sink.send(Event::Finished(Ok(Outcome::Message("done".to_owned()))))
                .await;
        });
        assert!(
            matches!(receiver.recv().await, Some(Event::Progress(value)) if value.completed == 1)
        );
        assert!(
            matches!(receiver.recv().await, Some(Event::Finished(Ok(Outcome::Message(value)))) if value == "done")
        );
        worker.await?;
        Ok(())
    }

    #[tokio::test]
    async fn content_scan_lists_real_supported_files_and_skips_hidden_or_unrelated_ones()
    -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let manager = InstanceManager::new(root.path())?;
        let game = root.path().join("instances").join("demo").join("game");
        for directory in ["mods", "resourcepacks", "saves"] {
            tokio::fs::create_dir_all(game.join(directory)).await?;
        }
        tokio::fs::write(game.join("mods").join("a.jar"), b"jar").await?;
        tokio::fs::write(game.join("mods").join("b.jar.disabled"), b"off").await?;
        tokio::fs::write(game.join("mods").join("notes.txt"), b"ignore").await?;
        tokio::fs::write(game.join("resourcepacks").join("pack.zip"), b"zip").await?;
        tokio::fs::create_dir_all(game.join("saves").join("world")).await?;
        tokio::fs::write(game.join("saves").join("world").join("level.dat"), b"level").await?;
        tokio::fs::create_dir_all(game.join("saves").join("empty")).await?;

        let cancel = CancellationToken::new();
        let mods = scan_content(&manager, "demo", ContentKind::Mods, &cancel).await?;
        assert_eq!(mods.entries.len(), 2);
        assert_eq!(mods.entries[0].name, "a.jar");
        assert!(mods.entries[0].enabled);
        assert_eq!(mods.entries[1].name, "b.jar.disabled");
        assert!(!mods.entries[1].enabled);
        let packs = scan_content(&manager, "demo", ContentKind::ResourcePacks, &cancel).await?;
        assert_eq!(packs.entries.len(), 1);
        assert_eq!(packs.entries[0].name, "pack.zip");
        let worlds = scan_content(&manager, "demo", ContentKind::Worlds, &cancel).await?;
        assert_eq!(worlds.entries.len(), 1);
        assert_eq!(worlds.entries[0].name, "world");
        assert!(worlds.folder.ends_with("saves"));
        Ok(())
    }
}
