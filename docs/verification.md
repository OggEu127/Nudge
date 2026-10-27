# 验证记录与验收边界

本文件记录当前开发过程实际观察到的结果，不以工作流配置代替远端执行，也不以 mock 替代正版账号或 Windows 桌面验收。记录基准日为 **2026-10-24 UTC**；后续代码修改仍需重新运行质量检查。

## 2026-10-27 Windows x64 推送前检查

当前仅面向 Windows x86-64 交付。此次未提交源码在 Linux 主机上通过格式检查、全特性工作区测试（**155 项通过，0 失败**）、严格 Clippy、`RUSTDOCFLAGS='-D warnings'` 文档构建及 CI 测试日志脚本检查。使用本机 LLVM-MinGW 工具链，通过 `x86_64-pc-windows-gnullvm` 的全工作区、全目标、全特性交叉编译检查和严格 Clippy；Windows 专用测试仅编译，未在 Windows 执行。

这些结果不等于 Windows MSVC Release 构建或桌面验收。根目录现存 EXE/ZIP 是先前源码的构建，不对应当前待推送改动；真实 Microsoft 登录仍搁置，实例删除/重装/升级的上层协调器也尚未实现。推送后的 Windows 原生 CI 和新产物需另行核查。

## 2026-10-25 待办跟进

本轮只执行本地验证；Microsoft 真实账号验收按维护者要求搁置，Windows 暂无实机，以下交叉编译不等于 Windows 原生运行。

| 待办 | 本轮结果 |
| --- | --- |
| P1 下载总超时 | 已移除固定 300 秒限制，正文持续有进展可继续；保留连接/读停滞超时和元数据 30 秒总超时。手动虚拟时钟累计 320 秒的分块流单次成功，停滞测试触发重试后可取消 |
| P1 Microsoft 正版登录 | 搁置；未配置或借用他人应用 ID，未进行真实授权 |
| P2 下载记录生命周期 | 完成显式释放接口、互斥与事务回滚测试，以及[删除/重装/升级设计](download-lifecycle.md)；上层用户操作与共享依赖所有者索引尚未实现 |
| P2 文件持久化与路径 | 配置、实例记录、下载与封面接入统一提交；Unix 同步父目录，配置/记录新建目录同步目录项；故障注入与提交前链接替换测试通过。剩余 TOCTOU 窗口与 Windows 掉电保证仍有限制 |
| P2 Windows 验收 | 准备 [PowerShell 冒烟脚本与验收矩阵](windows-acceptance.md)、显式运行的合成凭据测试；脚本和实机矩阵尚未在 Windows 执行 |
| P3 GUI 拆分与重试 | 导航/对话框状态、读取请求状态、首页/内容、其他页面、下载页分模块；失败与加载分开显示，版本失败可主动重试，不自动逐帧重发 |

验证环境：Linux x86-64、Rust/Cargo 1.99.1；全工作区测试 **144 通过、0 失败**（其中网络模块 36 项）。格式检查、全目标编译、严格 Clippy、rustdoc 均通过；GUI 的无显示逐页布局测试通过，没有进行新的原生窗口/真实游戏冒烟。测试日志保存在本地 `target/followup-tests.log`。

额外通过 `cargo check --locked --offline --workspace --all-targets --target x86_64-pc-windows-gnullvm`，使用工作区已有 LLVM-MinGW 工具链；包括新增 Windows 凭据测试的编译检查。未运行 Windows EXE、系统凭据测试或 PowerShell 脚本，也没有重新生成 Windows 发行包。

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
- `target/windows-gnullvm-branding-build.log`
- `target/windows-gnullvm-branding-resources.txt`
- `target/windows-gnullvm-branding-sha256.txt`
- `target/x11-window-icon.txt`

联网夹具清单为 `crates/launcher-modpack/tests/fixtures/smoke-index.json`，归档生成器为 `crates/launcher-modpack/examples/smoke_pack.rs`。夹具不打包游戏或 Mod 二进制。`target/` 内的证据不随源代码分发。

## 桌面与跨目标构建

已编译 Linux 原生 `nudge-gui`，在 Xvfb 中实际显示窗口，读取并呈现三个既有实例。旧版英文布局截图保留在 `target/gui-home_000.png`；完成简体中文化后，已在同一 Xvfb 环境重新启动并截取 `target/gui-home-zh.png`，实际观察到中文导航、首页提示与状态栏。最终代码状态又以 `--smoke-test-seconds 6` 启动并正常退出（`target/gui-final-smoke.log`），确认当前构建仍能创建窗口。

完成纯白 Liquid Glass 界面重构后，再次以当前构建在 Xvfb 中逐页查看游戏、模组、资源包、世界存档、整合包、下载、设置、更多及账户弹窗，并在 920×640 物理窗口、`WINIT_X11_SCALE_FACTOR=1.25/1.5` 与宽逻辑布局下复核游戏、下载、模组、整合包页和账户弹窗。观察到：窄宽度下实例卡片改为单列，宽逻辑布局下为三列；下载筛选框与「仅正式版」复选框都完整可见，版本行不越过面板右缘；账户弹窗在已登录、未登录、离线表单和设备码状态下未被裁剪；日志区改为浅色凹槽后仍保持可读的等宽文本。截图保存在 `target/gui-redesign-home.png`、`target/gui-redesign-home-wide.png`、`target/gui-redesign-downloads.png`、`target/gui-redesign-log.png`、`target/gui-redesign-account-popup.png` 与 `target/gui-redesign-150.png`。这些截图只证明当前虚拟显示下的布局与字体，不代表完成全部交互、拖放、真实 Windows 窗口动画、系统菜单和无障碍验收。

在 Linux 上使用 llvm-mingw 与 `-Ctarget-feature=+crt-static` 真实交叉编译了 `x86_64-pc-windows-gnullvm` Release CLI 与 GUI。构建日志、PE 导入表和二进制 SHA-256 分别保存在 `target/windows-gnullvm-build.log`、`target/windows-gnullvm-imports.txt` 与 `target/windows-gnullvm-sha256.txt`；导入表已不含 `libunwind.dll`，剩余条目均为 Windows 系统 DLL 或 UCRT API set。这不表示产物无需任何系统运行库，也没有在 Windows 上执行过 EXE；正式便携包仍由 Windows 原生 MSVC 工作流构建。

替换品牌资源后，同一交叉目标重新构建了两个 EXE，并用 `llvm-readobj --coff-resources` 确认两者都包含 7 个尺寸的 `ICON`、一个 `GROUP_ICON` 和一个 `VERSIONINFO`；`VERSIONINFO` 中的 `ProductName` 为 Nudge，`OriginalFilename` 分别为 `nudge-gui.exe` 和 `nudge.exe`，版本为 0.1.0。日志与资源摘要保存在 `target/windows-gnullvm-branding-build.log`、`target/windows-gnullvm-branding-resources.txt` 与 `target/windows-gnullvm-branding-sha256.txt`。在 Xvfb 中启动当前 GUI 后，`xprop` 读到 128×128 的 `_NET_WM_ICON`，输出保存在 `target/x11-window-icon.txt`；这不等同于已验收 Windows 任务栏、资源管理器或安装后的图标缓存行为。

标题栏随后再次在 1240×820、920×640 和 150% 缩放下逐页复核，图标与字标没有压住居中导航、拖拽区或窗口控件；`target/app-screenshots/` 中的 15 张截图与总览已全部用当前构建重新生成。窗口图标与标题栏图集都改为预乘 alpha 缩放，避免透明边缘出现暗边；GUI 测试额外解析 ICO 目录（7 个尺寸、32 位深度、每帧 PNG 签名）并检查缩放后的边缘颜色。README 改用由主图裁切的浅色/深色标题图，不再依赖肉眼难以辨认的点阵副标题。这些仍是 Linux/Xvfb 下的观察，不是 Windows 原生视觉验收。

## 自动化覆盖

工作区测试覆盖 Manifest 与 HTTP 边界、配置存储、规则与继承、参数构建、Java 解析、原生库路径/限额、下载重试/缓存/恢复/校验、实例锁/安全复制，以及 MRPACK 元数据、恶意路径、Overrides 和离线恢复。认证使用 mock 服务与替代凭据存储，不接触真实账号。

在当前代码状态（2026-10-24 UTC），已重新完整执行格式检查、全目标 `cargo check`、全特性严格 Clippy、全特性工作区测试，以及 `RUSTDOCFLAGS="-D warnings"` 的 rustdoc，全部退出 0。工作区共 135 项测试通过：`launcher-cli` 4、`launcher-core` 32（22 项单元测试 + 10 项实例集成测试）、`launcher-gui` 22、`launcher-minecraft` 43、`launcher-modpack` 5、`launcher-net` 29；0 失败、0 忽略，另有 5 个文档测试套件各 0 项。覆盖包括 CLI 归档摘要绑定与逐项可选文件、Windows 设备名与路径别名、符号链接父目录在创建目录前被拒绝、不支持的加载器在检查阶段被拒绝、登录元数据保存失败后清理新写入的刷新令牌、离线账户名称校验与确定性 UUID、系统凭据库不回退明文、ZIP64 条目元数据边界、Java 架构不匹配、并发测试中瞬态 `ETXTBSY` 的有界重试、GUI 账户弹窗在各状态下内容高度不超出玻璃面板，以及内置品牌图标的解码尺寸、ICO 目录结构、标题栏 UV 边界与预乘 alpha 缩放边缘。该数量是当前开发快照，不是最终发布承诺；后续修改仍应重新执行[完整质量检查](development.md)。

本轮额外覆盖：12 个独立 SQLite 连接同步升级旧账号数据库；封面失败/取消保留旧文件、原子替换不修改外部硬链接、拒绝祖先及目标符号链接；封面通过受跟踪后台任务提交；内容扫描拒绝链接根目录并跳过链接子项。封面导入不再在 UI 线程执行解码、缩放、编码和磁盘提交。

仓库状态说明：v0.1.0 首次公开推送已将本记录对应的工程文件发布到 [OggEu127/Nudge](https://github.com/OggEu127/Nudge)。`.github/workflows/` 随首次推送生效；远端 CI 的实际结果需要以 GitHub Actions 页面为准。在观察到完整运行结果前，不把工作流配置当作已通过的远端验收。

### 2026-10-25（韩国时间）：Windows 路径检查修复

[CI 40134472108](https://github.com/OggEu127/Nudge/actions/runs/40134472108) 对应提交 `9bcaf0a`：Ubuntu 通过，Windows 测试失败。新增的错误摘要确认，多项实例和下载测试在读取 `\\?\C:` 元数据时收到 `Incorrect function`。问题是逐段检查路径时，盘符前缀尚未与根分隔符合并，就被当成目录读取。

下载队列、单文件传输和实例路径检查现在保留前缀，等根分隔符拼接完成后才读取元数据；根目录及后续目录的符号链接和重解析点检查没有取消。Rust 对两种组件的定义见 [`Component::Prefix` 与 `RootDir`](https://doc.rust-lang.org/std/path/enum.Component.html)。新增三项通用回归测试覆盖原始路径、规范化路径、根目录、中文与空格、缺失子目录和完整性检查；另有一项 Windows 专用测试，用目录联接检查三个入口仍拒绝路径逃逸。

本轮 Linux 上 138 项 Rust 测试全部通过（原 135 项加三项通用回归测试），0 失败、0 忽略；6 项 CI 脚本测试、格式检查、全目标编译、严格 Clippy、rustdoc 和工作流检查也通过。另已对 `x86_64-pc-windows-gnullvm` 执行全目标、全特性编译检查和严格 Clippy，包含 Windows 专用测试代码。这只是交叉编译检查，没有在本机执行 Windows 测试；修复后的 Windows 原生 CI 仍需提交后验证。

## 仍需外部/平台验收

- Nudge 发布者拥有并获准使用的 Microsoft 应用 ID、真实持有 Minecraft Java 版的账号，以及真实授权/刷新/权限检查/退出。
- Windows runner 原生 CI、干净 Windows 桌面中的拖放、路径、凭据库与游戏启动；Linux 交叉编译不能替代。
- GUI 整体交互与长时间体验、真实显示/音频设备、高 DPI 和错误恢复。
- Windows 便携包的实际远端构建、人工下载校验、数字签名及公开发布审核。
- Linux/macOS 扩展支持与更多历史游戏版本的兼容性矩阵。

当前没有因为“代码已写”“请求能返回 401”“demo 能启动”就将这些验收项记为完成。正式发行请逐项完成[发布清单](release.md)。
