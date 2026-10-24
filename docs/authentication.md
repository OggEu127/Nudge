# Microsoft 正版认证

## 外部前置条件

实现协议不等于获得服务访问许可。Nudge 发布者必须准备自己的 Microsoft 公共客户端注册，允许个人 Microsoft 账号及对应公共客户端流程，并确认该应用有权访问 Xbox/Minecraft 服务。客户端 ID 是公开标识，不是客户端密钥；桌面应用不应携带可保密的 client secret。

Microsoft 的[设备授权协议](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)要求有效应用注册；[桌面应用配置](https://learn.microsoft.com/en-us/entra/identity-platform/scenario-desktop-app-configuration)说明公共客户端设置。[Xbox 官方说明](https://learn.microsoft.com/en-us/gaming/gdk/docs/services/fundamentals/s2s-auth-calls/service-authentication/live-website-authentication)也将应用注册作为访问服务的前置步骤。

Minecraft 生态提供的[应用审核入口](https://aka.ms/mce-reviewappid)需由发布者自行核实适用性、提交资料并确认结果。能打开表单或调用一个匿名端点，不代表已获批准；本仓库不承诺审核时限，不内置未经许可借用的应用 ID。

当前设备授权、刷新、Xbox/Minecraft 权限与档案检查、系统凭据库接口及 mock 测试均已实现。发布前尚未注册/配置 Nudge 自有获准 client ID，当前安排是先完成代码、测试和文档；没有使用真实持有 Java 版的账号完成端到端验收。mock 只验证本地实现和失败处理，不能将本阶段标记为“真实正版登录已验收”。

## 使用方式

获得属于本项目的应用 ID 后，通过配置或登录命令提供它：

```sh
nudge config set --microsoft-client-id YOUR_NUDGE_APP_ID
nudge account login
# 也可以仅在本次登录显式指定：
nudge account login --client-id YOUR_NUDGE_APP_ID
```

界面/终端显示微软验证页面和短期用户码。只在微软官方页面输入码并完成本人授权；不要把密码交给 Nudge，不要授权别人发来的未知设备码。用户码与待授权会话也不应放到公开日志。

```sh
nudge account list
nudge account select MINECRAFT_PROFILE_UUID
nudge launch my-pack
# 临时选择另一个已授权账号：
nudge launch my-pack --account MINECRAFT_PROFILE_UUID
nudge account logout MINECRAFT_PROFILE_UUID
```

退出只删除本地刷新凭据与公开账号记录，不等同于撤销微软所有设备的会话。要全局管理授权，请使用微软账号的官方安全/应用授权页面。

## 离线账户

Nudge 也支持本地离线身份，用于没有 Microsoft 账号时的单人或局域网游戏。桌面界面左下角的账户按钮是唯一入口：弹窗中并列提供「添加微软」「添加离线」和已有账户切换，不会同时显示两个独立的登录按钮。

- 名称需要 1–16 位 ASCII 字母、数字或下划线；按 Java 的 `OfflinePlayer:<name>` 规则生成确定性 UUID。
- 不收集密码，不请求 Microsoft/Xbox/Minecraft 服务，也没有刷新令牌；离线账户只在本地 SQLite 中保存公开身份。
- 启动时标记为 `legacy` 身份，不会被描述为已通过正版认证，也不代表拥有 Minecraft Java 版。
- 同一名称在本地已存在其他账户时拒绝创建，避免身份混淆；删除账户不影响游戏文件。
- 多人服务器会按服务器自己的验证规则处理离线身份，Nudge 不承诺服务器接受该身份。

## 协议与检查顺序

```text
用户发起登录
  → Microsoft consumers 设备码授权 / 按间隔轮询
  → Microsoft access token
  → Xbox 用户令牌
  → XSTS 授权（校验用户 hash 一致）
  → Minecraft launcher token
  → Java 版权限检查
  → Minecraft profile UUID / 名称
  → 系统凭据库保存 refresh token；SQLite 保存公开资料
```

设备授权遵循服务器的过期时间和轮询间隔，处理 `authorization_pending`、`slow_down`、拒绝、过期、取消等分支；不会把非成功响应当作已登录。`slow_down` 增加轮询间隔，见 [RFC 8628](https://www.rfc-editor.org/rfc/rfc8628)。当前请求范围按 [Xbox 官方资源说明](https://learn.microsoft.com/en-us/gaming/gdk/docs/services/fundamentals/s2s-auth-calls/service-authentication/live-website-authentication?view=gdk-2604)使用 `XboxLive.signin XboxLive.offline_access`，不混用其他资源 scope，不使用账号密码或客户端密钥。

每次正常启动重新通过刷新凭据获取身份，再核对 Java 版权限与所选档案 UUID。微软流程不提供无有效凭据的正版“离线绕过”；离线账户是明确标注的本地身份，不参加上述检查，也不会被当作已授权账号。微软轮换刷新令牌后优先持久化新值，避免后续权限检查临时失败使已消耗的旧令牌无法恢复。

Minecraft launcher/entitlement/profile 的完整公开稳定契约并不与 OAuth 文档一起提供。适配器当前采用 `/launcher/login`、`/entitlements/license` 和 `/minecraft/profile`；行为研究参考 [Prism Launcher 的 launcher 登录调用](https://github.com/PrismLauncher/PrismLauncher/blob/develop/launcher/minecraft/auth/steps/LauncherLoginStep.cpp)及[权限调用](https://github.com/PrismLauncher/PrismLauncher/blob/develop/launcher/minecraft/auth/steps/EntitlementsStep.cpp)，独立实现，不将社区实现冒称微软官方格式规范。未识别的结构、无 Java 权限或档案变化均拒绝生成启动身份；接口变化需重新核验。

## 凭据存储

使用 [keyring](https://docs.rs/keyring/4.2.0/keyring/) 的平台系统凭据后端：Windows Credential Manager、macOS Keychain、Linux Secret Service。阻塞凭据操作进入后台 worker，不阻塞图形界面。

- 只有刷新令牌进入系统凭据库，没有明文文件回退。
- `accounts.sqlite3` 只存 UUID、档案名称和公开 client ID；配置只存应用 ID 与所选账号。
- 访问令牌、Xbox/XSTS 令牌与设备密钥保留在内存，不写入实例、缓存、任务数据库或日志。
- 敏感类型的 Debug 输出脱敏，认证传输禁止重定向并限制正文大小。
- 账号写入/刷新/退出串行化，避免并发刷新和退出相互覆盖。

Linux 无桌面 Secret Service、凭据库未解锁、沙盒不允许访问或系统拒绝写入时会明确报错。复制数据目录不会复制系统凭据；移动目录也可能改变凭据命名空间，应重新登录。

## 常见失败

| 情况 | 处理 |
| --- | --- |
| 未配置 client ID / ID 非法 | 先完成应用注册并设置属于 Nudge 的有效公开 ID |
| 应用被拒绝或 Minecraft 服务不允许访问 | 核对应用配置、服务访问许可与审核；不换用他人 ID 绕过 |
| 设备码过期或授权拒绝 | 由用户重新发起登录，不自动无限重试 |
| Xbox 账号/家庭策略限制 | 按官方账号设置处理；保留服务状态/数值错误码，不打印响应令牌 |
| 没有 Java 权限或档案不匹配 | 拒绝正版启动，确认账号与游戏拥有情况 |
| 刷新已撤销 | 再次本人授权 |
| 系统凭据库不可用 | 配置/解锁操作系统凭据服务，不改成明文存储 |

显式 `--demo` 只启动 Minecraft 自带的受限试玩模式；它不证明正版身份，也不是缺少 client ID、权限或凭据时的自动回退。
