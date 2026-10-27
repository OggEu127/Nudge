//! Responsive native desktop frontend; installation and launch work lives in core.

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Nudge supports Windows x86-64 only");

mod app;
mod branding;
mod covers;
mod errors;
mod jobs;
mod logs;
mod theme;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context as _, Result};
use launcher_core::config::{AppPaths, ConfigStore};
use tokio_util::task::TaskTracker;

/// Native frontend startup paths and optional bounded diagnostic lifetime.
#[derive(Clone, Debug, Default)]
pub struct GuiOptions {
    /// Override the platform configuration path.
    pub config: Option<PathBuf>,
    /// Override the platform shared data directory.
    pub data_dir: Option<PathBuf>,
    /// Close after this interval, useful for display-server smoke tests.
    pub smoke_test_duration: Option<Duration>,
}

/// Returns a Chinese user-facing summary without exposing technical chains.
#[must_use]
pub fn error_summary(error: &anyhow::Error) -> String {
    errors::Failure::from_error(error).summary
}

/// Starts the native UI on the calling OS main thread.
///
/// # Errors
/// Returns configuration, runtime, display-server or rendering initialization errors.
pub fn run(options: GuiOptions) -> Result<()> {
    let paths = AppPaths::discover()?;
    let store = ConfigStore::new(options.config.unwrap_or(paths.config_file));
    let config = store.load_or_create()?;
    let data_dir = options.data_dir.unwrap_or(paths.data_dir);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("无法创建桌面后台运行环境")?;
    let handle = runtime.handle().clone();
    let tracker = TaskTracker::new();
    let app_tracker = tracker.clone();
    let window_icon = branding::window_icon()?;
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Nudge · Minecraft Java 版启动器")
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([920.0, 640.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_taskbar(true)
            .with_resizable(true)
            .with_drag_and_drop(true)
            .with_icon(window_icon),
        renderer: eframe::Renderer::Glow,
        centered: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Nudge",
        native,
        Box::new(move |context| {
            Ok(Box::new(app::NudgeApp::new(
                context,
                handle,
                app_tracker,
                store,
                config,
                data_dir,
                options.smoke_test_duration,
            )?))
        }),
    )
    .map_err(|error| anyhow::anyhow!("桌面界面初始化失败：{error}"));
    // Runtime shutdown cancels async tasks immediately. Keep it alive first so
    // cancellation can flush transfer journals and reap child processes.
    let drained = drain_background(&runtime, &tracker, Duration::from_secs(10));
    runtime.shutdown_timeout(Duration::from_secs(1));
    shutdown_result(result, drained)
}

fn shutdown_result(result: anyhow::Result<()>, drained: bool) -> anyhow::Result<()> {
    match (result, drained) {
        (Ok(()), true) => Ok(()),
        (Err(error), true) => Err(error),
        (Ok(()), false) => {
            tracing::warn!("后台任务未能在 10 秒宽限期内结束");
            Err(errors::FrontendError::ShutdownTimeout.into())
        }
        (Err(error), false) => {
            tracing::warn!(
                error = %error,
                "后台任务未能在 10 秒宽限期内结束"
            );
            Err(errors::FrontendError::ShutdownTimeout.into())
        }
    }
}

fn drain_background(
    runtime: &tokio::runtime::Runtime,
    tracker: &TaskTracker,
    grace: Duration,
) -> bool {
    tracker.close();
    runtime
        .block_on(async { tokio::time::timeout(grace, tracker.wait()).await })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::{drain_background, shutdown_result};
    use crate::errors::FrontendError;
    use std::time::Duration;
    use tokio_util::task::TaskTracker;

    #[test]
    fn shutdown_timeout_is_not_masked_by_an_earlier_loop_error() {
        let error = shutdown_result(Err(anyhow::anyhow!("window failure")), false)
            .expect_err("expired grace period must be reported");
        assert!(
            error
                .downcast_ref::<FrontendError>()
                .is_some_and(|error| matches!(error, FrontendError::ShutdownTimeout))
        );
        let original = shutdown_result(Err(anyhow::anyhow!("window failure")), true)
            .expect_err("loop error must survive a clean drain");
        assert_eq!(original.to_string(), "window failure");
    }

    #[test]
    fn uncooperative_background_task_has_a_bounded_shutdown() -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let tracker = TaskTracker::new();
        let worker = tracker.spawn_on(std::future::pending::<()>(), runtime.handle());
        assert!(!drain_background(
            &runtime,
            &tracker,
            Duration::from_millis(20)
        ));
        worker.abort();
        assert!(drain_background(&runtime, &tracker, Duration::from_secs(2)));
        runtime.shutdown_timeout(Duration::from_secs(1));
        Ok(())
    }
}
