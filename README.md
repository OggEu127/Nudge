<!-- markdownlint-disable MD033 MD041 -->
<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/branding/nudge-lockup-blue-violet-dark.png">
    <img src="assets/branding/nudge-lockup-blue-violet.png" alt="Nudge — Minecraft Java 版启动器" width="420" height="135">
  </picture>
</h1>

<p align="center">
  <a href="https://github.com/OggEu127/Nudge/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/OggEu127/Nudge/ci.yml?branch=main&label=CI&logo=github" alt="CI status"></a>
  <a href="https://github.com/OggEu127/Nudge/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2FOggEu127%2FNudge%2Fmain%2FCargo.toml&query=%24.workspace.package.version&label=version&color=blue" alt="Version"></a>
  <a href="https://github.com/OggEu127/Nudge/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--only-blue" alt="License: GPL-3.0-only"></a>
  <a href="https://github.com/OggEu127/Nudge/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/dynamic/toml?url=https%3A%2F%2Fraw.githubusercontent.com%2FOggEu127%2FNudge%2Fmain%2FCargo.toml&query=%24.workspace.package.rust-version&label=MSRV&color=orange&logo=rust&suffix=%2B" alt="Rust MSRV"></a>
  <a href="https://github.com/OggEu127/Nudge/blob/main/docs/verification.md"><img src="https://img.shields.io/badge/platform-Windows%20x86--64%20target%20%7C%20native%20QA%20pending-lightgrey" alt="Platform: Windows x86-64 target, native acceptance pending"></a>
  <a href="https://github.com/OggEu127/Nudge/blob/main/docs/verification.md"><img src="https://img.shields.io/badge/UI-%E7%AE%80%E4%BD%93%E4%B8%AD%E6%96%87-blue" alt="UI language: Simplified Chinese"></a>
</p>

用 Rust 写的 Windows x86-64 Minecraft Java 版启动器，支持原版、Fabric 和本地 MRPACK，提供 CLI 与简体中文桌面界面。源码和 CI 仅支持 Windows x86-64。

**v0.1.0 · 2026-10-24 首次公开推送 · 开发版。** 真实 Microsoft 账号登录、Windows 原生桌面和签名发行尚未验收；仓库没有内置 Microsoft 应用 ID。

旧版的历史验证记录见[验证记录](docs/verification.md)；当前 Windows 专用代码仍需 Windows 原生 CI 和桌面验收。

## 功能

- **原版与 Fabric**：读取 Mojang 版本清单和 Fabric Meta，安装版本、Libraries、Assets，生成启动参数。
- **本地 MRPACK**：识别客户端文件与可选文件，应用 Overrides，先检查安装计划再导入。
- **下载与恢复**：有界并发、校验缓存、备用源、重试、取消、SQLite 任务恢复和条件 Range 续传。
- **实例管理**：隔离目录、完整性校验、安装锁和断点恢复，并管理启动后的子进程。
- **Microsoft 登录**：设备码授权、令牌刷新、Java 权限与档案检查；刷新令牌只写入系统凭据库。真实账号测试需要自行配置应用 ID。
- **离线账户**：本地玩家名称生成确定性 UUID，不收集密码、不保存任何微软凭据，也不会伪装成正版授权。
- **桌面界面**：基于 egui/eframe 的纯白 Liquid Glass 风格，包含实例卡片、模组/资源包/存档浏览、整合包导入、版本下载、任务日志、设置与启动。

## 暂不支持

Forge、NeoForge、Quilt、CurseForge/HMCL 整合包、Java 自动安装、整合包升级和自动更新。

## 快速开始

在 Windows x86-64 上构建需要 Rust 1.99 或更新版本、Visual Studio C++ 构建工具与 Windows SDK；启动游戏还需要已安装且与游戏版本匹配的 Java。准备步骤见[开发指南](docs/development.md)。

```powershell
git clone https://github.com/OggEu127/Nudge.git
cd Nudge
cargo build --locked --release -p launcher-cli -p launcher-gui --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\release\nudge-gui.exe
```

常用 CLI 命令（完整参数见[用户指南](docs/user-guide.md)）：

```powershell
$nudge = '.\target\x86_64-pc-windows-msvc\release\nudge.exe'
& $nudge config init
& $nudge manifest --kind release --limit 10
& $nudge install 1.21.5 --instance vanilla
& $nudge install 1.21.5 --instance fabric --fabric 0.20.1
& $nudge inspect .\MyPack.mrpack --instance my-pack
& $nudge import .\MyPack.mrpack --instance my-pack
& $nudge launch my-pack --demo --java 'C:\Program Files\Java\jdk-21\bin\java.exe'
```

将示例 Java 路径替换为本机实际路径。导入前先看 `inspect` 输出的 `archive_sha256`，再用 `import --archive-sha256` 传入同一个摘要，导入就只接受你确认过的归档。安装中断后可用 `resume` 继续，`verify` 用于完整性检查。

## 验证

当前质量门在 Windows x86-64 runner 上执行格式、编译、严格 Clippy、工作区测试和 rustdoc；命令与已知限制见[开发指南](docs/development.md)。真实账号、Windows 桌面和音频/高 DPI 仍需人工验收。

## 参考与致谢

实现时参考了这些公开接口、格式或行为；具体代码为独立编写。

- [Mojang Launcher Meta](https://piston-meta.mojang.com/mc/game/version_manifest_v2.json)：原版版本、Libraries 和 Assets。
- [Fabric Meta](https://github.com/FabricMC/fabric-meta)：Loader profile 与依赖。
- [Modrinth MRPACK 格式](https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack)：整合包清单。
- [Prism Launcher](https://github.com/PrismLauncher/PrismLauncher)：launcher/entitlement 登录调用的行为参考；Prism Launcher 自身使用 GPL-3.0。
- [Microsoft 设备码授权](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)、[Xbox 认证](https://learn.microsoft.com/en-us/gaming/gdk/docs/services/fundamentals/s2s-auth-calls/service-authentication/live-website-authentication)和 [RFC 8628](https://www.rfc-editor.org/rfc/rfc8628)：登录流程。

技术栈：egui/eframe、tokio、reqwest/rustls、rusqlite、keyring 和 zip。

## 文档

- [用户指南](docs/user-guide.md)
- [架构与数据目录](docs/architecture.md)
- [核心 API 与扩展边界](docs/api.md)
- [安全模型与限制](docs/security.md)
- [Microsoft 认证与外部前置条件](docs/authentication.md)
- [开发、测试与故障定位](docs/development.md)
- [Windows 构建与发布](docs/release.md)
- [实际验证记录](docs/verification.md)
- [下载记录生命周期设计](docs/download-lifecycle.md)
- [Windows 原生验收工具与清单](docs/windows-acceptance.md)

## 许可证

本项目使用 [GPL-3.0-only](LICENSE)。Minecraft 游戏资源与第三方 Mod 不在本许可证范围内；发行包不包含游戏文件、Java 运行时或账号数据。

Nudge 是非官方项目，与 Microsoft、Mojang、Modrinth、CurseForge 没有隶属关系。
