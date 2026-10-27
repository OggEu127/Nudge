# Windows 构建与发布

本项目只支持 Windows x86-64。源码、CI、构建和发布任务均以该平台为目标；Windows 原生运行验收仍须单独记录。

本仓库支持构建、打包和发布开发版，但不代表稳定版已经验收。微软应用注册、真实账号登录和 Windows 桌面运行仍需检查。发布前要先确认版本号、说明和对应提交。

## 自动构建

`.github/workflows/ci.yml` 对 push、pull request 或手动触发在 Windows x86-64 runner 执行 Stable 质量检查，包括格式、编译、严格 Clippy、测试和 rustdoc。

`.github/workflows/windows-portable.yml` 在手动触发或推送 `v*` 标签时，在 Windows runner 原生构建 `x86_64-pc-windows-msvc` Release，先执行格式、全目标编译检查、Clippy、测试及 CLI 基础运行检查，再打包便携 ZIP 与 SHA-256。

两种触发方式的区别：

- 手动运行（包括选择已有标签运行）：只上传 Actions 构建产物，保留 14 天，不创建或修改 Release。
- 推送版本标签：构建成功后，单独的发布任务下载本次产物，核对文件名和 SHA-256，再把 ZIP 和 `.zip.sha256` 上传到同名 GitHub Release。Release 附件不受 Actions 的 14 天保留期影响。

构建任务只有 `contents: read`；发布任务才有 `contents: write`，使用 GitHub 自动提供的短期 `GITHUB_TOKEN`，不用额外配置个人令牌。Action 依赖固定为完整 commit SHA。发布流程使用官方 [download-artifact](https://github.com/actions/download-artifact) 和 [GitHub CLI](https://cli.github.com/manual/gh_release_create)。

## 发布一个版本

1. 确认工作区版本号、发布说明和对应提交，先把代码推送到远程。
2. 经维护者确认后，创建并推送相同版本的标签，例如 Cargo 版本 `0.1.0` 对应 `v0.1.0`。标签和 Cargo 版本不一致会阻止发布。
3. 等待 Windows 构建、测试和发布任务通过，在仓库的 Releases 页面下载便携包和校验文件。

普通 `main` 分支推送不会触发发布；工作流也不会替维护者创建或推送标签。当前公开版本统一标记为预发布，不设为 Latest，说明中注明未签名和待验收事项。检查通过后才上传附件，上传完成后才公开草稿，避免用户看到缺少应用的版本。

若同名 Release 已存在，任务会失败，不会覆盖已有附件或改写发布说明。上传或公开失败时可能留下草稿，需要检查失败原因和草稿内容；清理或修改远程草稿仍须维护者确认，不能直接强制重跑覆盖已有版本。

这里只配置流程；文件修改或代码推送不表示已有新版本发布。实际打标签和公开发布需另行确认。

## 便携包内容

```text
Nudge-<version>-windows-x86_64/
├── nudge.exe
├── nudge-gui.exe
├── README.md
├── LICENSE
├── BUILD-INFO.json
├── SHA256SUMS
├── docs/
├── scripts/windows-smoke.ps1
└── assets/
```

ZIP 外另提供 `.zip.sha256`。`BUILD-INFO.json` 记录版本、源 commit、目标、Rust 版本和工作流链接。包不包含 Java、游戏文件、第三方 Mod 或任何账号信息。当前工作流复制整个 `docs/` 以保持 README 中的相对链接可用，因此也包含开发与发布文档；这些内部页面不是运行便携包所必需的内容。工作流还会复制 `scripts/`（Windows 冒烟工具）与 `assets/`，让 README 顶部的标志可以显示；应用图标已经内嵌进两个 EXE，运行时不依赖这些外部文件。

“便携”指免安装的二进制分发，不表示默认把用户数据写在 EXE 旁。默认数据进入平台用户目录。需要自包含数据时，显式运行：

```powershell
.\nudge-gui.exe --config .\userdata\config.json --data-dir .\userdata\data
.\nudge.exe --config .\userdata\config.json --data-dir .\userdata\data instances
```

系统凭据库仍属操作系统用户，不会随 `userdata` 目录转移。把目录复制到另一台机器后需要重新授权。

## Windows 原生构建

准备 [开发工具链](development.md)，在 Windows PowerShell 中执行：

```powershell
cargo build --locked --release -p launcher-cli -p launcher-gui --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\release\nudge.exe --version
.\target\x86_64-pc-windows-msvc\release\nudge-gui.exe
```

构建流程不打包 Visual C++ 系统运行库；需在干净 Windows 机器核查依赖，并在必要时明确提供微软官方运行库安装指引。当前没有数字签名，Windows SmartScreen 可能提示未知发布者；校验摘要不能替代代码签名或来源审核。

Windows 实机脚本、凭据库测试和逐项结果模板见[Windows 原生验收](windows-acceptance.md)。脚本未执行和矩阵未签收时不得标为 Windows 已验收。

## 正式发布前清单

- [ ] 固定并审阅发布 commit，完整记录 `fmt/check/clippy/test/doc` 结果。
- [ ] Windows 原生 Release 构建和 CLI 基础检查通过；下载 artifact 并核对 ZIP 与二进制摘要。
- [ ] 在干净 Windows x86-64 用户环境运行 GUI，检查中文路径、拖放、缩放、日志和退出。
- [ ] 安装并启动原版、Fabric 与真实 MRPACK；断网/取消/重启后恢复，损坏文件重新校验。
- [ ] 使用 Nudge 自有、允许对应服务访问的客户端注册完成真实微软账号授权、刷新、退出及 Java 版权限验证。
- [ ] 检查系统凭据库可用/锁定/不可用情况，不出现明文回退或令牌日志。
- [ ] 对选定版本核查 Java 要求、图形驱动和平台运行库，注明受测版本，不概括为“全部版本验证”。
- [ ] 检查依赖许可证与安全公告，审查隐私说明、错误信息和默认配置。
- [ ] 随二进制提供 GPLv3 许可证、对应源代码和可复现的构建说明；不夹带无权再分发内容。
- [ ] 开通可用的私密漏洞报告渠道，确认已知限制和未完成验收在发行说明中可见。
- [ ] 由维护者明确批准版本号、发布说明和公开发布；构建 artifact 不自动等同于已发布。

当前没有承诺逐字节可复现构建、自动升级、安装程序或 Windows ARM64 支持。尚未执行的清单不能以 mock 测试、交叉编译或工作流配置代替。
