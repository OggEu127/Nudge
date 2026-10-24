# 验证记录与验收边界

本文件记录当前开发过程实际观察到的结果，不以工作流配置代替远端执行，也不以 mock 替代正版账号或 Windows 桌面验收。记录基准日为 **2026-10-24 UTC**；后续代码修改仍需重新运行质量检查。

## 已执行的真实游戏冒烟

环境：Linux x86-64、Rust Stable 1.99.1、显式指定的 Adoptium Java 21.0.13 JRE、Xvfb 虚拟显示与软件 OpenGL。Java 来自公开发行元数据并核对 SHA-256，仅为本次测试准备，不是产品的 Java 自动安装功能。最终代码状态重新下载并校验了同一版本 JRE，然后用当前构建重跑三类 demo 启动。

| 测试 | 实际观察 |
| --- | --- |
| Mojang Manifest | 成功从官方接口读取并解析版本列表 |
| 原版 1.21.5 | 安装并校验客户端、Libraries、资源等；启动后创建图形资源 atlas。最终代码状态又用当前 `target/debug/nudge` 重跑一次 demo 启动（20 秒诊断上限），正常创建 atlas 后受控取消并返回 0 |
| Fabric 0.20.1 / 1.21.5 | 合并官方 profile 与依赖，日志确认 Loader/Mixin 初始化及图形资源加载；最终代码状态以当前构建重跑（20 秒诊断上限），同样创建 atlas 后受控取消并返回 0 |
| MRPACK / 1.21.5 | 由仓库 Rust 夹具生成器生成归档，下载 Fabric API 0.128.2+1.21.5、应用 Overrides，日志确认 52 个 Mod 项及图形资源加载；最终代码状态以当前构建重跑（45 秒诊断上限，覆盖 52 个 Mod 的资源加载），同样创建 atlas 后受控取消并返回 0 |
| 子进程回收 | 诊断时间上限触发受控取消，输出报告 `cancelled: true`，未将取消记为自然成功退出 |

所有游戏冒烟显式使用 Minecraft 官方受限 demo 模式。虚拟机没有正常音频设备/朗读依赖，出现非致命音频诊断；观察到纹理/GUI atlas 初始化，不宣称验证了声卡、实际游玩、多人、所有 Mod 行为或全部 Minecraft 版本。

开发工作区的证据文件（`target/` 被 Git 忽略，不随源代码分发）：

- `target/vanilla-install.log`
- `target/vanilla-launch-xvfb.log`
- `target/vanilla-smoke-launch-final.log`（最终代码状态的 demo 重启）
- `target/fabric-install.log`
- `target/fabric-launch.log`
- `target/fabric-smoke-launch-final.log`（最终代码状态的 demo 重启）
- `target/mrpack-import.log`
- `target/mrpack-launch.log`
- `target/mrpack-smoke-launch-final.log`（最终代码状态的 demo 重启）
- `target/gui-home_000.png`（汉化前英文布局）
- `target/gui-home-zh.png`（汉化后中文界面）
- `target/gui-zh-smoke.log`
- `target/gui-final-smoke.log`（最终代码状态的 6 秒启动/关闭冒烟）
- `target/windows-gnullvm-build.log`
- `target/windows-gnullvm-imports.txt`
- `target/windows-gnullvm-sha256.txt`

联网夹具清单为 `crates/launcher-modpack/tests/fixtures/smoke-index.json`，归档生成器为 `crates/launcher-modpack/examples/smoke_pack.rs`。夹具不打包游戏或 Mod 二进制。`target/` 内的证据不随源代码分发。

## 桌面与跨目标构建

已编译 Linux 原生 `nudge-gui`，在 Xvfb 中实际显示窗口，读取并呈现三个既有实例。旧版英文布局截图保留在 `target/gui-home_000.png`；完成简体中文化后，已在同一 Xvfb 环境重新启动并截取 `target/gui-home-zh.png`，实际观察到中文导航、首页提示与状态栏。最终代码状态又以 `--smoke-test-seconds 6` 启动并正常退出（`target/gui-final-smoke.log`），确认当前构建仍能创建窗口。截图只证明窗口、字体与数据显示，不代表完成全部交互、拖放、缩放和无障碍验收。

在 Linux 上使用 llvm-mingw 与 `-Ctarget-feature=+crt-static` 真实交叉编译了 `x86_64-pc-windows-gnullvm` Release CLI 与 GUI。构建日志、PE 导入表和二进制 SHA-256 分别保存在 `target/windows-gnullvm-build.log`、`target/windows-gnullvm-imports.txt` 与 `target/windows-gnullvm-sha256.txt`；导入表已不含 `libunwind.dll`，剩余条目均为 Windows 系统 DLL 或 UCRT API set。这不表示产物无需任何系统运行库，也没有在 Windows 上执行过 EXE；正式便携包仍由 Windows 原生 MSVC 工作流构建。

## 自动化覆盖

工作区测试覆盖 Manifest 与 HTTP 边界、配置存储、规则与继承、参数构建、Java 解析、原生库路径/限额、下载重试/缓存/恢复/校验、实例锁/安全复制，以及 MRPACK 元数据、恶意路径、Overrides 和离线恢复。认证使用 mock 服务与替代凭据存储，不接触真实账号。

在当前代码状态（2026-10-24 UTC），已重新完整执行格式检查、全目标 `cargo check`、全特性严格 Clippy、全特性工作区测试，以及 `RUSTDOCFLAGS="-D warnings"` 的 rustdoc，全部退出 0。工作区共 118 项测试通过：`launcher-cli` 4、`launcher-core` 29、`launcher-gui` 9、`launcher-minecraft` 42、`launcher-modpack` 5、`launcher-net` 29；0 失败、0 忽略，另有 5 个文档测试套件各 0 项。新增覆盖包括 CLI 归档摘要绑定与逐项可选文件、Windows 设备名与路径别名、符号链接父目录在创建目录前被拒绝、不支持的加载器在检查阶段被拒绝、登录元数据保存失败后清理新写入的刷新令牌、ZIP64 条目元数据边界、Java 架构不匹配，以及并发测试中瞬态 `ETXTBSY` 的有界重试。该数量是当前开发快照，不是最终发布承诺；后续修改仍应重新执行[完整质量检查](development.md)。

仓库状态说明：v0.1.0 首次公开推送已将本记录对应的工程文件发布到 [OggEu127/Nudge](https://github.com/OggEu127/Nudge)。`.github/workflows/` 随首次推送生效；远端 CI 的实际结果需要以 GitHub Actions 页面为准。在观察到完整运行结果前，不把工作流配置当作已通过的远端验收。

## 仍需外部/平台验收

- Nudge 发布者拥有并获准使用的 Microsoft 应用 ID、真实持有 Minecraft Java 版的账号，以及真实授权/刷新/权限检查/退出。
- Windows runner 原生 CI、干净 Windows 桌面中的拖放、路径、凭据库与游戏启动；Linux 交叉编译不能替代。
- GUI 整体交互与长时间体验、真实显示/音频设备、高 DPI 和错误恢复。
- Windows 便携包的实际远端构建、人工下载校验、数字签名及公开发布审核。
- Linux/macOS 扩展支持与更多历史游戏版本的兼容性矩阵。

当前没有因为“代码已写”“请求能返回 401”“demo 能启动”就将这些验收项记为完成。正式发行请逐项完成[发布清单](release.md)。
