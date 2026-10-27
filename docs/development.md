# 开发指南

## 工具链

工作区只支持 Windows x86-64，使用 Rust 2024 Edition、Cargo resolver 3；声明的 MSRV 为 1.99，`rust-toolchain.toml` 跟随 stable，并要求 rustfmt/Clippy。CI 目前安装浮动 stable，尚未单独锁定并验证 1.99；最低版本声明应视为待自动化确认的目标。`Cargo.lock` 随仓库分发，CI 和发布命令使用 `--locked`。

仓库的 `.gitattributes` 要求文本文件以 LF 检出，避免 Windows runner 上的格式检查受 CRLF 转换影响。

使用 `x86_64-pc-windows-msvc`，安装 Visual Studio Build Tools 的 C++ 桌面工作负载与 Windows SDK。SQLite 使用 bundled 编译，无需安装独立数据库服务。GUI 运行需要 Windows 桌面和可用图形驱动；一次 `cargo build` 不能证明窗口正常。

## 必须执行的检查

```powershell
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
$env:RUSTDOCFLAGS = '-D warnings'
cargo doc --locked --workspace --no-deps
```

质量检查失败时修复原因，不删除测试或放宽错误来制造通过结果。

普通 CI 和 Windows 发布构建都在 Windows runner 上通过 Git Bash 运行 `.github/scripts/run-tests.sh`。测试失败时保留 Cargo 的退出码，在页面显示包含 panic 详情的错误摘要，并将完整的 `test-output.log` 作为 Actions 附件保留 14 天。没有进入测试步骤时不生成日志；日志写入失败也会使任务失败。

[`cargo doc`](https://doc.rust-lang.org/cargo/commands/cargo-doc.html) 生成的入口在 `target/doc/launcher_core/index.html`，其他领域 crate 也有独立文档。

## 测试层次

- 纯单元测试：Manifest/元数据、规则、继承、参数、Java 版本、路径、MRPACK 计划和恶意归档。
- 边界测试：`wiremock` 提供本地 HTTP，覆盖非成功响应、超时/中断、摘要、备用源、续传、缓存和取消。
- 核心集成测试：临时数据目录、实例锁、原子复制、恢复和整合包执行；不下载完整 Minecraft。
- 手动联网/桌面验收：官方依赖、实际 Java、原版/Fabric/MRPACK 子进程、真实登录和 Windows 体验，单独记录环境与结果。

测试通常使用显式 loopback-only 客户端，不给生产网络策略添加测试例外。集成测试 `pack_resume_uses_only_verified_local_content_applies_overrides_and_checks_mods` 是一个刻意例外：它使用生产 HTTPS 策略和不可解析的运行时 URL，以证明完整缓存命中时不会发起网络请求，并用 mock 请求历史断言验证。认证测试使用替代凭据存储，不需要真实微软账号，也不会写入用户凭据库。

## 本地隔离与诊断

```powershell
cargo run --locked -p launcher-cli -- `
  --config target/dev-config.json --data-dir target/dev-data config init
cargo run --locked -p launcher-cli -- `
  --config target/dev-config.json --data-dir target/dev-data manifest --limit 5
```

默认日志写 stderr，可用全局 `--json-logs` 生成结构化日志，用 `RUST_LOG=debug` 调整追踪级别。CLI 的业务输出保留在 stdout，便于管道处理。不要为了诊断打印令牌、认证响应、完整启动参数或环境变量清单。

## 联网 MRPACK 冒烟夹具

仓库提供固定的清单与 Rust 归档生成器，不分发第三方 Mod JAR：

```powershell
cargo run --locked -p launcher-modpack --example smoke_pack -- target/smoke.mrpack
cargo run --locked -p launcher-cli -- `
  --config target/dev-config.json --data-dir target/dev-data `
  import target/smoke.mrpack --instance smoke
```

生成器对已存在的输出报错，不静默覆盖。该夹具引用的 Mod 版本可在上游删除或改变可用性；网络错误不应被测试伪装成成功。后续启动需要匹配的 Java 和图形环境，demo 冒烟不替代正版验收。

## 贡献约定

修改前检查现有工作树与模块说明，保留无关改动。新格式解析器先产出计划，下载必须复用统一引擎；GUI 不处理安装内部细节。对公开接口补充 rustdoc，对边界行为补回归测试。

新增依赖先检查维护状态、当前稳定 API、MSRV 与许可证；依赖生态变化需要同步更新 lockfile。避免无理由 `unwrap`/`expect`、忽略重要 `Result`、无限并发和未约束内存分配。

## 本次新增边界验证

下载慢速流测试使用 Tokio 手动时间推进，分块间隔 20 秒、累计超过 300 秒，验证单次请求不中断；另测停滞 31 秒触发重试及取消。`test-util` 仅作为网络 crate 的开发特性启用。持久化故障注入验证各提交边界的旧/新完整内容、临时文件清理及重试；不代替电源故障实验。

Windows 原生验收按 [工具与清单](windows-acceptance.md) 单独执行；Microsoft 真实账号流程当前搁置，继续标记未验收。
