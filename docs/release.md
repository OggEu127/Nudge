# Windows 构建与发布

本项目优先 Windows x86-64，架构保留 Linux/macOS 扩展。工作流存在、交叉编译通过、Windows 原生运行正常是三个不同结论，必须分别记录。

本仓库提供构建、打包与发布准备，不等于正式稳定发行已经完成。v0.1.0 首次推送已获维护者授权；自有 Microsoft 应用注册与真实账号、原生 Windows 桌面以及首次远端工作流结果仍需外部验收，正式签名和公开发布仍需单独审核。

## 自动构建

`.github/workflows/ci.yml` 对 push、pull request 或手动触发执行 Ubuntu/Windows Stable 质量检查，包括格式、编译、严格 Clippy 和测试；Linux 另验证 rustdoc。

`.github/workflows/windows-portable.yml` 在手动触发或推送 `v*` 标签时，在 Windows runner 原生构建 `x86_64-pc-windows-msvc` Release，先执行格式、全目标编译检查、Clippy、测试及 CLI 基础运行检查，再上传便携 ZIP 与 SHA-256。

工作流依赖固定为完整 commit SHA，仓库权限只有 `contents: read`。包构建不会推送代码、创建标签或 GitHub Release；需要仓库维护者另行审核和发布。手动触发和标签事件遵循 [GitHub Actions 触发规则](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)，产物使用官方 [upload-artifact](https://github.com/actions/upload-artifact) 保存，默认 14 天。

## 便携包内容

```text
Nudge-<version>-windows-x86_64/
├── nudge.exe
├── nudge-gui.exe
├── README.md
├── LICENSE
├── BUILD-INFO.json
├── SHA256SUMS
└── docs/
```

ZIP 外另提供 `.zip.sha256`。`BUILD-INFO.json` 记录版本、源 commit、目标、Rust 版本和工作流链接。包不包含 Java、游戏文件、第三方 Mod 或任何账号信息。当前工作流复制整个 `docs/` 以保持 README 中的相对链接可用，因此也包含开发与发布文档；这些内部页面不是运行便携包所必需的内容。

“便携”指免安装的二进制分发，不表示默认把用户数据写在 EXE 旁。默认数据进入平台用户目录。需要自包含数据时，显式运行：

```powershell
.\nudge-gui.exe --config .\userdata\config.json --data-dir .\userdata\data
.\nudge.exe --config .\userdata\config.json --data-dir .\userdata\data instances
```

系统凭据库仍属操作系统用户，不会随 `userdata` 目录转移。把目录复制到另一台机器后需要重新授权。

## 本机 Release 构建

准备 [开发工具链](development.md)，在 Windows PowerShell 中执行：

```powershell
cargo build --locked --release -p launcher-cli -p launcher-gui --target x86_64-pc-windows-msvc
.\target\x86_64-pc-windows-msvc\release\nudge.exe --version
.\target\x86_64-pc-windows-msvc\release\nudge-gui.exe
```

构建流程不打包 Visual C++ 系统运行库；需在干净 Windows 机器核查依赖，并在必要时明确提供微软官方运行库安装指引。当前没有数字签名，Windows SmartScreen 可能提示未知发布者；校验摘要不能替代代码签名或来源审核。

在 Linux 上可用 `x86_64-pc-windows-gnullvm` 加 `-Ctarget-feature=+crt-static` 做导入依赖诊断；已验证可消除 `libunwind.dll`，但产物仍依赖 Windows 系统 UCRT，且未在 Windows 执行，不能替代 MSVC runner 的正式构建。

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

当前没有承诺逐字节可复现构建、自动升级、安装程序或 Windows ARM64 支持。尚未执行的清单不能以 mock 测试、Linux 交叉编译或工作流配置代替。
