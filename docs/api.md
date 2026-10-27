# 核心 API 与扩展边界

工作区尚未发布稳定的 Rust crate API；0.1 开发阶段允许经审阅的接口调整。以下接口均对应实际代码，不是远程 HTTP API。完整参数、错误与类型以源码 rustdoc 为准：

```sh
cargo doc --locked --workspace --no-deps --open
```

## 应用层

`launcher_core::Launcher` 是轻量版本发现入口；`Launcher::with_http` 为 HTTP 测试注入边界，`version_manifest` 取得并验证官方清单。

`launcher_core::instances::InstanceManager` 持有显式数据根目录、生产 HTTP 客户端与下载并发设置：

| 方法 | 语义 |
| --- | --- |
| `new` / `with_http` | 创建生产服务 / 显式注入受限测试传输 |
| `with_concurrency` | 设置并发偏好；下载引擎只接受 1–64，超过上限会在首次构造引擎时明确报错 |
| `game_directory`、`load`、`list` | 验证实例 ID，定位隔离目录或读取记录 |
| `install_vanilla_controlled` | 原版安装与可恢复取消，回调 `InstallProgress` |
| `install_fabric` | 精确 Fabric Loader 版本安装 |
| `inspect_modpack` | 在阻塞 worker 检查本地归档，返回计划，无下载/目标写入 |
| `import_modpack` | 按 `ImportOptions` 执行计划，验证后提交实例 |
| `resume_instance` | 使用已保存元数据/相同归档和选择恢复；不升级整合包 |
| `verify` | 验证运行时及受管理的不可变整合包文件 |
| `launch` / `launch_controlled` | 校验、匹配 Java、提取原生库并返回 `RunningInstance`；后者接受准备阶段取消 |

安装、导入、恢复、启动准备和认证阶段接收 `CancellationToken`。`InstanceManager::verify` 与 `inspect_modpack` 不接收取消令牌：它们会在当前校验/解析步骤完成后返回，前端不能把“发出取消”立即渲染成“已成功结束”。前端取消后应等待结果或进程退出。安装进度是阶段内完成文件数，恢复/缓存复用同样发出完成状态；底层下载另提供字节事件。

`Instance` 的 `installed` 是核心在全部验证通过后写入磁盘的状态；前端修改内存副本不会改变持久记录，启动与校验会重新读取磁盘状态，因此不能据此绕过就绪检查。保存/提交细节不作为前端绕过接口。`ImportOptions` 支持全部可选文件或 `BTreeSet<PathBuf>` 的逐项选择；预览后执行的前端应设置 `expected_archive_sha256`，绑定用户实际审核过的归档内容。

`list` 会跳过单个损坏的实例记录并记录警告；直接按 ID `load` 仍返回该记录的错误。大小写仅不同的实例 ID 会在创建时明确拒绝，避免在大小写不敏感的文件系统上指向同一目录。

`RunningInstance::next_event` 返回 `Started`、`Output`、`Exited`；`cancel` 请求结束，`wait` 排空输出并回收进程。输出通道有界，调用者应持续消费。不要丢弃句柄后假定游戏会在后台无限运行；实例锁和原生库由 supervisor 持有到直接子进程回收之后，不随消费者句柄提前释放。取消启动准备时，不可中止的原生库提取 worker 会安全完成并保留资源所有权，再返回取消结果。

## 认证层

`launcher_core::auth::AuthService` 提供 `accounts`、`begin_login`、`complete_login`、`identity`、`logout`。`begin_login` 只发起用户授权，不代表成功；只有权限和档案检查完成后才保存账号。`identity` 的结果用于正常游戏启动，不能用显示名称拼装“已认证身份”。

`accounts` 会跳过单条损坏的账户元数据并记录警告；对损坏账户执行操作仍明确报错。若记录的标识或名称无法读取，新增账户会停止，以免误判为不存在并覆盖原记录。

`CredentialStore` 是可替换的异步凭据边界，生产为 `SystemCredentials`，测试为内存替代。`AuthService::with_parts` 和 `Endpoints` 用于显式测试注入；归档、加载器元数据与 GUI 普通用户输入不得重设生产认证端点。

`DeviceAuthorization` 中只将验证页面和短期用户码交给界面；私有设备密钥不可复制到日志或持久化。正常启动身份和原始启动参数同样属于敏感数据。

## Minecraft 层

- `parse_version_manifest` / `parse_version_metadata`：类型化解析与语义验证。
- `resolver::resolve_inheritance` / `merge`：纯继承合并，环检测、深度限制及 Library 覆盖。
- `rules::Platform` / 规则求值：显式平台和 feature 条件，不依赖前端字符串拼接判断。
- `resolver::resolve`：产出 `ResolvedVersion`、`ResolvedFile`、`NativeArchive`，路径相对于共享根。
- `java::discover_java` / `inspect_java`：有界子进程检测，不安装 Java。
- `launch::build_launch_spec`：无 shell 参数构造；未知占位符报错。
- `launch::spawn_game_with_resource`：把实例租约/暂存目录等资源交给进程 supervisor，直至直接子进程回收后释放；后代继承管道的排空等待有上限。
- `natives::extract_natives`：有限额和路径检查的提取，目标必须为新的私有目录。
- `fabric::list_loaders` / `resolve_profile`：官方适配器，网络依赖通过 `HttpClient` 传入。

`LaunchIdentity::new` 仅验证数据结构，不执行授权。调用方必须先经过认证服务；`LaunchIdentity::demo` 只能与显式 demo 选项配合。`LaunchSpec::arguments` 会暴露敏感 token，禁止直接记录。

## MRPACK 层

`launcher_modpack::inspect` / `inspect_with_limits` 返回 `InstallPlan`。计划包括来源指纹、目标、版本、加载器、必需/可选文件、排除项、Overrides 和依赖图，`ordered_tasks` 检查拓扑顺序。

`extract_overrides` 只向调用方提供的空私有暂存目录提取，不代替核心的目标实例锁和最终提交。外部调用者仍需遵循[路径与归档安全要求](security.md)。

添加 CurseForge/HMCL 等格式应新增格式检测/解析器并映射到统一计划，不在解析器内下载、执行脚本、改写全局实例或调用 GUI。新增加载器则在 Minecraft 层实现元数据适配，并由核心显式允许该加载器。

## 网络层

`HttpClient::get_bytes` 是有正文上限的小型元数据接口。`ReqwestHttpClient` 的生产策略是公网 HTTPS，测试须显式选择 `LoopbackOnly`。

`FileSpec` 包含 URL、可选长度、必需有效的 `Hashes`。`transfer::verify_file` / `verify_content` 流式检查所有已声明摘要。不得将无预期哈希的文件交给安装器并只检查存在性。

`DownloadManager::run` 接收 `DownloadJob` 集合、取消令牌与广播 sender；任务带稳定 ID、安全相对目的地、优先级和备用来源。`resume_pending` 重载持久队列。前端应处理进度广播落后，不把最佳努力的字节进度当作提交结果；方法返回值与实例提交状态是最终依据。

认证专用 `api::ApiClient` 提供有界 JSON/form/bearer 请求，禁止重定向，错误不回传响应正文。不要将普通带重定向的下载客户端用于传递登录凭据。

`DownloadManager::release_destinations` 在实例锁保护下接受可信旧任务，原子释放精确目的地及相应任务记录；保留文件与共享缓存，拒绝摘要已变化的旧计划。它与运行/恢复互斥；共享依赖需要上层证明没有其他所有者。上层删除/重装/升级协调器尚未实现，约束见[下载生命周期](download-lifecycle.md)。

`launcher_net::persistence` 为配置及受管理内容提供文件刷盘、提交前后路径复查与 Windows 原子替换。替换后的复查失败可能发生于新内容已经可见时，调用者应重新读回。静态路径复查不等于消除 TOCTOU。
