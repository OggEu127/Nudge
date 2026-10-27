# 架构

Nudge 将外部格式解析、安装计划、网络传输、实例协调与界面适配分开。CLI 和 GUI 共享核心服务，不各自实现一套下载或安装逻辑。

## Cargo Workspace

```text
Nudge/
├── crates/
│   ├── launcher-core/       实例、配置、认证与安装协调
│   ├── launcher-minecraft/  Mojang/Fabric 元数据、Java、参数与进程
│   ├── launcher-modpack/    MRPACK 检查、计划、受限 Overrides 提取
│   ├── launcher-net/        HTTP 策略、校验、下载队列与恢复
│   ├── launcher-cli/        nudge 命令行适配器
│   └── launcher-gui/        egui/eframe 桌面适配器
├── docs/
└── .github/workflows/
```

```text
launcher-cli ────┐
                ├──► launcher-core ──► launcher-minecraft ──┐
launcher-gui ────┘          │                              ├──► launcher-net
                           └─────────► launcher-modpack ───┘
```

依赖只向下流动。`launcher-core` 协调领域模块，领域模块不依赖核心层；共享协议由最低层的实际所有者定义，不增加泛化的“通用工具 crate”。

| 模块 | 接口边界 |
| --- | --- |
| `launcher-core` | `InstanceManager`、`Launcher`、配置存储、安装进度、认证服务与统一应用错误 |
| `launcher-minecraft` | 类型化元数据、纯规则/继承解析、文件解析计划、Java 检测、参数构建、原生库提取、受监督进程 |
| `launcher-modpack` | 无网络的 `inspect` → `InstallPlan`；不直接安装或启动游戏 |
| `launcher-net` | `HttpClient`、`FileSpec`/`Hashes`、`DownloadManager`、认证专用的受限 API 传输 |
| 前端 | 收集输入、调用服务、消费事件、呈现错误；不得自行绕过路径、身份和下载校验 |

## 原版与 Fabric

官方 Manifest v2 提供版本 ID、元数据 URL 与可用摘要；`launcher-core` 在下载元数据后、解析前检查摘要，解析后验证版本标识。继承解析有深度上限和环检测，合并 Libraries 与现代/旧式启动参数，保留父版本客户端 JAR。

规则求值使用当前 OS、架构、系统版本与 feature 集合，遵循顺序规则。解析器产出 classpath、客户端、Libraries、原生库、Asset Index/Objects 与日志配置清单，不依赖某个固定版本的命令模板。

Fabric 适配器从[官方 Fabric Meta API](https://github.com/FabricMC/fabric-meta)取得精确 Loader profile，再与基础游戏合并。依赖没有内嵌摘要时读取仓库公开的 `.sha1`；无法建立预期摘要时拒绝无校验安装。

安装记录保存有效合并元数据。恢复已开始的实例可使用保存的元数据，避免重启后仅因重新发现版本而丢失恢复能力。

## MRPACK：检查与执行

依据[Modrinth 官方格式定义](https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack)检查根目录 `modrinth.index.json`，不凭文件扩展名决定格式。客户端安装不会下载 `unsupported` 文件，把 `optional` 保留给用户选择；服务端专用条目仍须通过清单、来源与大小校验，不合规时整个包会被拒绝。

```text
本地归档
   │ 内容识别、路径/限额/CRC/摘要检查
   ▼
InstallPlan + 归档 SHA-256 + 用户可选文件选择
   │ 核心执行器持有目标实例锁
   ▼
Runtime → PackFiles → Overrides → Verify → Commit
   │                                      │
   └── 失败保留 incomplete + 下载状态       └── installed = true
```

计划包含目标目录、Minecraft/加载器版本、必需/可选/排除文件、主/备用来源、摘要、Overrides 和任务依赖图。核心只执行支持的加载器，对未知依赖明确报错。

覆盖优先级为普通 `overrides/` 后应用 `client-overrides/`；忽略客户端不应安装的 `server-overrides/`。计划绑定归档 SHA-256，提取时再次核对，防止预览后来源被替换。

覆盖内容先进入私有暂存目录，验证后按文件原子提交。整个实例不宣称支持文件系统级事务回滚：中途失败留下未完成记录和可恢复文件，只有所有阶段成功后才标记就绪。

相同实例只允许恢复相同归档及相同可选文件集合。整合包升级是另一种需要冲突/删除策略的操作，尚未实现，不冒充为“恢复”。

## 下载与持久化

`DownloadManager` 在应用数据根目录下运行，任务目的地必须是安全相对路径。SQLite 存储完整任务规格与状态；非阻塞文件锁保证同根目录同时只有一个引擎实例运行，并发启动会明确失败而不是排队。程序退出留下的 running 任务仅在取得锁后转回 pending。

调度器维护有界并发和优先级，不为所有文件创建无界 Tokio 任务，也不把游戏文件整体读入内存。已存在目标和内容缓存都重新检查期望长度及全部已提供摘要。相同内容有独立内容锁以避免重复传输。

续传需要已保存的资源验证标识，发送 Range/If-Range，并检查服务器响应与资源一致性；不支持续传、返回完整内容或标识改变时安全重头下载。最终摘要始终决定能否提交。网络重试与备用源不降低 HTTPS、路径或哈希要求。

## 数据布局

默认配置/数据路径由 `directories::ProjectDirs` 在 Windows 用户目录下发现，CLI 可用 `--config` 和 `--data-dir` 显式隔离：

```text
<config>/config.json
<data>/
├── versions/<version>/<version>.jar
├── libraries/<Maven 路径>
├── assets/{indexes,objects,virtual}/
├── logging/
├── .nudge-downloads/
│   ├── tasks.sqlite3
│   ├── engine.lock
│   ├── cache/
│   └── partials/
└── instances/<安全实例 ID>/
    ├── instance.json
    ├── instance.lock
    ├── game/               配置、Mods、存档等实例私有内容
    └── runtime/            每次启动的私有原生库暂存目录
```

共享的游戏依赖以校验和确认完整性，游戏目录相互隔离。实例 ID 来自用户确认，不从包名称直接拼接路径。安装与运行持有同一实例租约，防止运行中的实例被并发改写。

配置、实例记录、下载提交与封面使用同目录临时文件、文件刷盘、路径复查和原子替换。Windows 安全 API 不提供目录刷盘保证；替换后的路径复查若失败，新内容可能已经可见，重试前必须重新读取。无效配置不会静默重置。凭据不写入配置、实例记录或下载数据库，详见[认证说明](authentication.md)。

下载正文不设固定总时长上限，保留连接超时和 30 秒读停滞检测；元数据请求保留 30 秒总超时。目的地退役接口与删除、重装和升级的设计见[下载生命周期](download-lifecycle.md)。

GUI 的导航/对话框状态、读取请求状态、首页与内容、详情/整合包/设置、下载页分别位于 `app/` 子模块；后台执行仍由 `jobs` 调用核心服务。读取失败保持明确状态，主动刷新后重试，不在每一帧自动重发。

## 启动生命周期

启动先校验就绪状态和关键内容，再检测符合版本要求/架构的 Java。原生库进入每次运行独有的受限目录；参数通过直接 `Command` 传递，不经过 shell。

`RunningInstance` 提供启动、标准输出/错误输出和退出事件，以及取消/等待接口。进程 supervisor 持有原生库目录与实例锁，直到直接子进程回收且输出读取结束后才释放；前端句柄被丢弃会请求取消，但不能提前解锁仍在运行的实例。

前端必须持续消费有界输出。直接子进程结束后最多再排空输出两秒，避免其他后代进程继承管道导致无限等待；当前不负责终止整个后代进程树。GUI 关闭时先取消任务，并为后台保存状态/回收提供有限等待，超时明确报错。JVM 注入环境变量不从启动器环境盲目继承。

正常游戏身份来自用户明确选择的微软账户或离线账户：前者经过刷新凭据、Java 版权限与档案检查；后者按 Java `OfflinePlayer:<name>` 规则生成确定性 UUID，并标记为 `legacy`，不冒充正版。显式 demo 模式仅使用游戏自带限制，不伪造拥有游戏的账号。安装完成和开始游戏是两个不同操作；GUI 的安装后自动启动偏好默认关闭，CLI 安装/导入命令始终只安装，启动由单独命令执行。
