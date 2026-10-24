//! Small async adapter between UI intentions and frontend-independent services.

use std::{collections::BTreeSet, path::PathBuf};

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
    SignIn(String),
    SignOut(Uuid),
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
    SignedIn(Account),
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
    use super::{Event, EventSink, Outcome};
    use launcher_core::instances::InstallProgress;
    use tokio::sync::mpsc;

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
}
