# Windows 原生验收

状态：待 Windows 实机执行。交叉编译和单元测试不代表本清单通过。真实 Microsoft 登录当前按维护者要求搁置。

## 自动冒烟工具

使用 Windows 10/11 x86-64 的交互桌面，核对二进制来自待验收源码。项目根目录中的现有 EXE 是旧构建；源码修改后必须重新构建或下载对应提交的产物。

```powershell
cargo build --locked --release -p launcher-cli -p launcher-gui --target x86_64-pc-windows-msvc
powershell -NoProfile -File .\scripts\windows-smoke.ps1 `
  -BinaryDirectory .\target\x86_64-pc-windows-msvc\release `
  -JavaExecutable 'C:\Program Files\Eclipse Adoptium\jdk-21\bin\java.exe'
```

便携包也携带 `scripts/windows-smoke.ps1`，可在解压根目录直接运行；不指定 `BinaryDirectory` 时使用脚本所在目录的上一级。凭据库测试仍需要源码和 Rust 工具链。

脚本只接受尚不存在的输出目录，默认在 TEMP 创建独立目录，强制覆盖中文与空格路径。检查版本、配置 JSON、空实例/账号、可选 Java 检测、GUI 打开与三秒后正常退出；GUI 超过 30 秒则终止本次进程并报错。保留 EXE SHA-256、UTC 时间、退出码、日志和 `report.json`。未提供 Java 时明确标记 SKIPPED。脚本不登录账号，不自动安装游戏，不证明视觉和交互正常，也不证明游戏启动。运行异常时检查原生错误对话框。

系统凭据库单独使用显式选择的测试；它只写一个随机名称的合成条目，测试写入、读取、覆盖、删除、重复删除，失败也尝试清理，不使用真实账号或令牌：

```powershell
cargo test --locked -p launcher-core --test windows_credentials `
  windows_system_credentials_roundtrip -- --ignored --exact
```

普通 CI 不会执行这个需要用户凭据库的测试。失败要保留错误与 OS 环境；系统禁止访问时应明确失败，不能改为文件存储。凭据库的锁定/受限用户场景仍需人工验证。

## 人工验收矩阵

每行记录 PASS / FAIL / SKIPPED、具体步骤、UTC 时间和证据文件。记录 Windows 版本、CPU/GPU/驱动、屏幕分辨率/缩放、Java 版本、源码提交及两个 EXE 摘要。不要记录密码、设备码、刷新令牌或完整游戏启动参数。

| 项目 | 操作与通过条件 |
| --- | --- |
| 图形与 DPI | 100%/150%/200% 缩放、跨显示器移动、最小尺寸/最大化/还原；中文、玻璃背景、阴影、弹窗及滚动内容无裁切、黑屏和明显错位；保存截图 |
| 窗口控制 | 拖动标题栏、调整四边/四角、最小化、任务栏恢复、最大化、关闭，行为一致 |
| 拖放 | 中文/空格路径 MRPACK 与封面；有效包显示检查方案，损坏 ZIP、错误扩展名、多个文件及链接/重解析目录给出错误，不误导为安装完成 |
| 网络失败与重试 | 下载页断网后显示失败与“重试获取版本”，每帧不重复请求；恢复网络后主动重试成功；模组目录权限失败后刷新可恢复 |
| 凭据库 | 执行上面的合成条目测试；受限/不可用场景明确失败，无明文回退。真实正版账号流程搁置，单列待验收 |
| Java 与路径 | 支持的 Java 主版本、中文/空格路径；缺失、错误版本与架构报错；检测无窗口卡死 |
| 实际启动 | 逐项安装原版、Fabric、MRPACK，完整性校验后用官方 demo 启动；确认真实游戏窗口、日志、关闭游戏和退出码。demo 不证明正版授权 |
| 退出与回收 | 游戏运行中关闭启动器、下载中关闭、空闲关闭；直接 Java 子进程回收、实例锁可再次取得、下载状态能恢复；无未处理异常。后代进程树当前不保证整体回收 |
| 恢复与完整性 | 中途取消、断网、重启后恢复；损坏缓存/目的地重新校验；有效备用来源成功；不损坏存档 |

## 结果模板

```text
源码提交 / 二进制 SHA-256：
Windows / GPU / 驱动 / 屏幕 DPI：
Java 路径 / 版本 / 架构：
自动脚本 report.json 位置：
凭据库测试结果：
矩阵逐项结果与截图：
未通过项、复现步骤：
真实 Microsoft 登录：搁置（未验收）
验收人 / UTC 时间：
```
