//! Typed Chinese summaries, with original diagnostic chains kept separately.

use launcher_core::{Error as CoreError, auth};
use launcher_minecraft::Error as MinecraftError;
use launcher_net::Error as NetError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FrontendError {
    #[error("操作已取消。")]
    Cancelled,
    #[error("请先选择已授权的微软账号，或明确启用官方试玩模式。")]
    MissingAccount,
    #[error("保存的账号标识无效，请重新选择或登录账号。")]
    InvalidSelectedAccount,
    #[error("Minecraft 未能正常退出（退出代码：{0:?}），请查看游戏日志。")]
    GameExited(Option<i32>),
    #[error("后台任务未能在 10 秒内结束。下次启动时，请校验或继续安装受影响的实例。")]
    ShutdownTimeout,
}

#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub(crate) summary: String,
    pub(crate) details: String,
}

impl Failure {
    pub(crate) fn message(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            details: String::new(),
        }
    }

    pub(crate) fn from_error(error: &anyhow::Error) -> Self {
        let summary = if let Some(error) = error.downcast_ref::<FrontendError>() {
            error.to_string()
        } else if let Some(error) = error.downcast_ref::<CoreError>() {
            core_summary(error)
        } else if let Some(error) = error.downcast_ref::<auth::Error>() {
            auth_summary(error)
        } else if let Some(error) = error.downcast_ref::<MinecraftError>() {
            minecraft_summary(error)
        } else if let Some(error) = error.downcast_ref::<NetError>() {
            net_summary(error)
        } else {
            "操作未能完成，请查看技术详情与日志后重试。".to_owned()
        };
        Self {
            summary,
            details: format!("{error:#}"),
        }
    }
}

fn core_summary(error: &CoreError) -> String {
    match error {
        CoreError::Authentication(error) => auth_summary(error),
        CoreError::Network(error) => net_summary(error),
        CoreError::Minecraft(error) => minecraft_summary(error),
        CoreError::Modpack(_) => {
            "整合包无效、已被修改或不符合安全要求，请检查原始压缩包。".to_owned()
        }
        CoreError::InvalidInput(_) => {
            "输入或设置项无效，请检查版本、实例标识、整合包依赖以及账号与下载设置。".to_owned()
        }
        CoreError::InstanceJson(_) => "游戏实例记录无法读取，请查看技术详情。".to_owned(),
        CoreError::Io { .. } | CoreError::CreateDirectory { .. } => {
            "无法访问游戏文件，请检查目录权限与磁盘空间。".to_owned()
        }
        CoreError::ConfigDirectoryUnavailable
        | CoreError::ReadConfig { .. }
        | CoreError::ParseConfig { .. }
        | CoreError::UnsupportedConfigSchema { .. }
        | CoreError::SerializeConfig(_)
        | CoreError::WriteConfig { .. }
        | CoreError::PersistConfig { .. } => {
            "无法读取或保存设置，请检查配置文件、格式与目录权限。".to_owned()
        }
        CoreError::Logging(_) => "日志系统初始化失败，请查看技术详情。".to_owned(),
    }
}

fn auth_summary(error: &auth::Error) -> String {
    match error {
        auth::Error::Cancelled => "账号操作已取消。",
        auth::Error::ClientId => "请在设置中填写自行注册的有效微软公共客户端应用标识。",
        auth::Error::Expired => "微软登录代码已过期，请重新开始登录。",
        auth::Error::Declined => "微软账号授权已被拒绝。",
        auth::Error::ReauthenticationRequired => "账号授权已失效，请重新登录微软账号。",
        auth::Error::NoEntitlement => "该账号未通过 Minecraft Java 版游戏权益验证。",
        auth::Error::ProfileMismatch => "账号身份与已保存的游戏档案不一致，请重新登录。",
        auth::Error::ApplicationChanged => "该账号属于另一应用标识，请先退出账号再更换应用注册。",
        auth::Error::AccountMissing(_) => "所选账号已不存在，请重新选择或登录。",
        auth::Error::CredentialStore(_) => {
            "系统凭据管理器不可用或尚未解锁。请启用并解锁后重试，不会回退到明文存储。"
        }
        auth::Error::Busy(_) => "另一个 Nudge 进程正在使用账号存储，请稍后重试。",
        auth::Error::Database { .. } | auth::Error::Storage(_) | auth::Error::Worker => {
            "账号存储操作失败，请检查数据目录与权限。"
        }
        auth::Error::Network(_) => "无法连接账号认证服务，请检查网络后重试。",
        auth::Error::Rejected { .. } => {
            "微软或 Minecraft 服务拒绝了请求，请确认应用权限、审核状态与账号资格。"
        }
        auth::Error::InvalidResponse(_) => "账号认证服务返回了无法识别的响应，请查看技术详情。",
    }
    .to_owned()
}

fn minecraft_summary(error: &MinecraftError) -> String {
    match error {
        MinecraftError::Network(error) => return net_summary(error),
        MinecraftError::JavaNotFound { required_major } => {
            return format!(
                "未找到 Java {required_major}。请安装对应版本，并在设置中填写 Java 可执行文件路径。"
            );
        }
        MinecraftError::JavaInspection { .. } | MinecraftError::InvalidJavaVersion { .. } => {
            "无法识别所选 Java，请检查文件路径、运行权限与版本。"
        }
        MinecraftError::ProcessStart { .. } => "无法启动 Minecraft 进程，请检查 Java 与实例设置。",
        MinecraftError::Io { .. } => "无法访问游戏运行所需文件，请检查目录权限与磁盘空间。",
        MinecraftError::Zip { .. }
        | MinecraftError::UnsafeArchive { .. }
        | MinecraftError::NativeDestinationExists(_) => {
            "本地库压缩包无法安全解压，请校验并修复游戏文件。"
        }
        MinecraftError::InvalidEndpoint(_)
        | MinecraftError::Json(_)
        | MinecraftError::InvalidMetadata(_)
        | MinecraftError::InvalidRuleExpression { .. }
        | MinecraftError::UnresolvedPlaceholder(_) => {
            "游戏版本元数据或启动参数无效，请校验实例并查看技术详情。"
        }
    }
    .to_owned()
}

fn net_summary(error: &NetError) -> String {
    match error {
        NetError::Cancelled => "操作已取消，现有安装恢复状态仍保留。",
        NetError::Integrity { .. } => "文件完整性校验未通过，未提交损坏文件。请重试或修复实例。",
        NetError::UrlRejected { .. } => "下载地址未通过安全检查，已拒绝访问。",
        NetError::Sqlite(_) => "无法读取或保存下载恢复记录，请检查数据目录。",
        NetError::Io { .. } => "无法保存下载文件，请检查目录权限与磁盘空间。",
        NetError::InvalidSpec(_) => "下载任务缺少有效的完整性信息，无法安全安装。",
        NetError::ClientBuild(_) | NetError::Request { .. } | NetError::HttpStatus { .. } => {
            "网络请求失败，请检查连接后重试；已安装文件不会因此被破坏。"
        }
        NetError::ResponseTooLarge { .. } => "服务端响应超出安全大小限制，已停止处理。",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::Failure;

    #[test]
    fn summaries_are_chinese_and_original_details_remain_available() {
        let error = anyhow::Error::from(launcher_core::Error::Minecraft(
            launcher_minecraft::Error::JavaNotFound { required_major: 21 },
        ));
        let failure = Failure::from_error(&error);
        assert!(failure.summary.contains("未找到 Java 21"));
        assert!(failure.details.contains("no Java 21 runtime was found"));
    }
}
