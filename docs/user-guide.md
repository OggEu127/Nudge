# 用户指南

以下示例使用编译后的 `nudge` 命令；Windows 对应 `nudge.exe`。源码开发时可改为 `cargo run --locked -p launcher-cli -- <子命令>`。查看本版本准确参数使用 `nudge --help` 和 `nudge <子命令> --help`。

## 桌面工作流

从源码启动 `cargo run --locked -p launcher-gui`；若使用自行构建的 Windows 便携包，可运行其中的 `nudge-gui.exe`。正式签名分发包尚未发布，产物来源与校验方式见[验证记录](verification.md)和[发布清单](release.md)。应用界面、操作提示与错误摘要默认使用简体中文，调用系统字体显示中文；系统应安装可用的中文字体。Minecraft、Fabric、Java 等技术名称和上游游戏/依赖的原始诊断日志保留原文，便于定位问题。

| 页面 | 用途 |
| --- | --- |
| 游戏 | 实例卡片、状态与详情（含封面）、校验文件、修复/继续安装、启动及停止；顶部可进入安装或导入 |
| 模组 / 资源包 / 世界存档 | 扫描当前实例对应目录，显示真实文件并可打开文件夹 |
| 整合包 | 本地路径或文件拖放、检查安装计划、选择可选文件并确认安装 |
| 下载 | 从官方版本清单筛选并安装原版或精确 Fabric Loader |
| 设置 | Java 路径、初始/最大内存、下载并发、微软应用标识、GUI 安装后自动启动 |
| 更多 | 有界任务日志、本地配置与数据路径、打开数据目录 |

左下角账户按钮是唯一的登录入口：未登录时显示「登录账户」，登录后显示头像、玩家名称与菜单箭头。点击后向上展开玻璃弹窗，在同一处添加微软账户、添加离线账户、切换已保存账户或退出；界面不会同时显示两个独立的登录按钮。

将本地 `.mrpack` 拖入窗口后，先检查整合包名称、Minecraft/加载器版本、目的实例和可选项，再点击「确认方案并安装整合包」。拖放只触发检查，不执行包中的脚本，也不隐式启动游戏。也可以在「导入整合包」输入本地路径并点击「解析压缩包」。

底部显示当前阶段文件进度，可取消正在进行的任务；取消后等待后台保存状态。重新打开启动器后，在「游戏实例」选择未完成实例，点击「修复 / 继续安装」。请保留来源归档，覆盖文件恢复仍需要它。

默认一次执行一个前台操作，后台由核心下载引擎保持有界并发。启动游戏后持续收集输出，使用「取消任务 / 停止游戏」结束并回收；当前版本不是可以任意退出启动器仍保留后台游戏的驻留服务。

关闭 GUI 时会取消当前任务，最多等待十秒让后台保存恢复状态并回收直接子进程；超时会报告错误而不是假装全部结束。下次打开应校验/恢复受影响实例。Nudge 不负责关闭 Mod 自己另行创建的所有外部程序。

正常「启动游戏」需要已选择的微软或离线账户；官方试玩必须显式勾选「使用 Minecraft 官方试玩模式」，不会因登录失败自动回退。GUI 安装后自动启动默认关闭，打开后仍要求合法账号或显式试玩设置。CLI 的 `install` / `import` 只执行安装，不因该设置自动开始游戏，需另行执行 `launch`。

## 安装原版与 Fabric

```sh
nudge manifest --kind release --limit 10
nudge install 1.21.5 --instance vanilla
nudge install 1.21.5 --instance fabric --fabric 0.20.1
nudge instances
```

实例 ID 只使用 ASCII 字母、数字、点、连字符和下划线，长度 1–80，不能以点结尾，也不能使用 `CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9` 等 Windows 设备名。不要使用归档中的任意路径或包显示名称；已存在的其他游戏/整合包实例不会被自动覆盖。GUI 会在解析或安装前校验标识并禁用无效操作。

Nudge 下载官方版本元数据、客户端、Libraries、资源索引、资源对象、日志配置及加载器依赖。已有文件和缓存必须校验成功才复用；首次安装耗时和空间取决于版本与网络。

## 导入与恢复 MRPACK

```sh
nudge inspect MyPack.mrpack --instance my-pack
# 将上面 JSON 中的 archive_sha256 传给导入，绑定审核过的字节：
nudge import MyPack.mrpack --instance my-pack --archive-sha256 <sha256>
# 逐项选择客户端可选文件（可重复），或选中全部：
nudge import MyPack.mrpack --instance with-optionals --optional config/example.toml
nudge import MyPack.mrpack --instance with-all-optionals --include-optional
nudge resume my-pack
nudge verify my-pack
```

`inspect` 输出 JSON 计划（含 `archive_sha256`），不进行下载或改写目标。`--include-optional` 与逐项 `--optional` 不能同时使用。格式按内部清单识别，即使扩展名不同，也必须有有效 MRPACK 内容；任意普通 ZIP、CurseForge/HMCL 包或不支持的加载器会明确报错。

恢复检查来源 SHA-256、可选文件选择与已有依赖。来源移动时可以重新 `import` 同一个字节内容、同一实例和同一可选集合；不允许把另一个包或版本冒充恢复。尚不支持原地整合包升级，请使用新的实例 ID。

正常启动时校验 Mod 和打包的不可变资源；用户配置可以修改，不会每次启动强制还原。显式重新导入相同包会再次应用其 Overrides，因此先备份希望保留的配置修改。存档不会因为下载恢复被自动删除。

## Java、内存与启动

```sh
nudge java --major 21
nudge config set --java /path/to/java --min-memory 512 --max-memory 4096 --concurrency 8
nudge launch my-pack
# 临时指定 Java 和官方受限试玩：
nudge launch my-pack --java /path/to/java --demo
```

Windows Java 路径指向 `java.exe`。自动发现会检查 `JAVA_HOME`、PATH 和常见运行时位置；运行时报告架构时同时验证主版本与架构，未报告 `os.arch` 的供应商运行时只能验证主版本。不能只把任意更高 Java 当作兼容版本。具体要求来自版本 JSON，缺少相应字段的旧版本使用 Java 8 规则。选择与版本要求相符的运行时，而不是把示例的 Java 21 套用到所有版本。

内存单位 MiB，满足 `128 <= min <= max`，还应为操作系统和其他程序留足内存。下载并发为 1–64，默认 8；过高不一定更快。配置可用 `nudge config show` 检查，`nudge config set --auto-launch true` 可修改 GUI 使用的安装后自动启动偏好；它不会让 CLI 的安装命令自动启动游戏。

微软应用注册、账号登录、授权刷新、离线身份和退出见[认证说明](authentication.md)。

## 位置与备份

默认使用 Windows 用户配置/数据目录，GUI「更多」页面显示实际数据路径；`config init` 显示配置路径。需要隔离时对 CLI 和 GUI 使用相同参数：

```sh
nudge --config userdata/config.json --data-dir userdata/data instances
nudge-gui --config userdata/config.json --data-dir userdata/data
```

重要存档和配置在 `<data>/instances/<id>/game/`。备份前停止游戏及安装任务，保存实例目录和原始整合包；完整迁移还需要共享依赖目录。系统凭据不包含在文件备份中，另一机器需要重新登录。不要手工修改任务 SQLite 或将数据目录放在其他用户可写的位置。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 下载中断 | 恢复网络后 `resume`；保留 partial、数据库和原始归档 |
| 哈希错误 | 不跳过校验；重试可信来源，确认缓存/磁盘及上游文件是否改变 |
| URL/路径被拒绝 | 核对来源是否符合安全策略；不关闭检查导入不可信包 |
| 同目录任务正在运行 | 等待另一个操作/进程结束；不要删除仍被持有的锁来强行并发 |
| Java 未找到/不匹配 | 指定真正的 Java 可执行文件，检查主版本和架构 |
| 凭据库不可用/没有权限 | 见认证文档，配置系统凭据库和应用授权，不使用明文回退 |
| GUI/游戏无法创建图形窗口 | 检查桌面会话、OpenGL/驱动和显示服务；无显示服务器不等于启动参数错误 |
| 服务端专属/其他加载器包 | 当前仅客户端原版/Fabric MRPACK；不把识别成功等同安装支持 |

向维护者报告问题时附版本、系统/架构、实例类型和脱敏错误链。不要上传 access/refresh token、设备码、完整启动参数或未经检查的账号日志。
