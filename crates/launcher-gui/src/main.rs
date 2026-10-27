//! Native desktop executable.

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Nudge supports Windows x86-64 only");

use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Parser;
use launcher_core::logging::{self, LogFormat};

#[derive(Parser)]
#[command(
    name = "nudge-gui",
    version,
    about = "Nudge 原生 Minecraft Java 版启动器"
)]
struct Arguments {
    /// 指定启动器配置文件路径。
    #[arg(long)]
    config: Option<PathBuf>,
    /// 指定共享应用数据目录。
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// 在指定秒数后关闭窗口，用于图形环境验收测试。
    #[arg(long, hide = true)]
    smoke_test_seconds: Option<u64>,
}

fn main() -> Result<()> {
    let result = run();
    if let Err(error) = &result {
        let _ = rfd::MessageDialog::new()
            .set_title("Nudge 遇到问题")
            .set_description(format!(
                "{}\n\n技术详情（原始错误信息）：\n{error:#}",
                launcher_gui::error_summary(error)
            ))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    result
}

fn run() -> Result<()> {
    let arguments = Arguments::parse();
    logging::init("info", LogFormat::Pretty)?;
    launcher_gui::run(launcher_gui::GuiOptions {
        config: arguments.config,
        data_dir: arguments.data_dir,
        smoke_test_duration: arguments.smoke_test_seconds.map(Duration::from_secs),
    })
}
