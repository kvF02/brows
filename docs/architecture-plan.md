# Brows：安全、低耦合的多 Profile 浏览器架构计划

> 版本：v0.1 设计基线（Windows 11 x64）
> 状态：v0.1 实施中，所有实现和接口变更都必须先更新本文档或对应 ADR。
> 产品定位：本地优先的隐私隔离、跨地区 QA、指纹测量和浏览器环境研究工具。

当前已落地的垂直切片包括 Domain/Contracts/Application 核心、SQLite
ProfileRepository、验证过的 Chromium 运行时适配器和 Windows Credential
Manager SecretStore 适配器。SecretStore 只使用当前用户的 Generic Credential
命名空间；非 Windows 平台明确返回 `Unavailable`，不会静默降级到明文文件。

## 1. 目标与边界

### 1.1 目标

先交付一个 MoreLogin 的安全简化版，核心价值是“多个相互隔离的浏览器身份同时运行”：

- 每个环境有独立 Chromium User Data Directory、Cookie、LocalStorage、IndexedDB、缓存、Service Worker、历史记录和权限。
- 不同环境可以绑定不同的 HTTP、HTTPS 或 SOCKS5 代理，并独立检测出口网络。
- 最多保存 500 个环境，默认最多并发运行 10 个 Chromium；超出后进入可取消的 FIFO 队列。
- 配置采用版本化、可解释的一致性预设；同一环境重启后配置稳定，不在每次启动时随机变化。
- 管理器、运行时、凭据、备份和网络行为可审计，默认零遥测。
- 以模块化单体和清晰端口（Ports）设计，未来可以替换数据库、浏览器运行时、代理实现或 UI，而不修改领域核心。

### 1.2 非目标（v0.1 不做）

- 不承诺绕过网站风控、反滥用或验证码系统。
- 不通过脚本注入或临时随机值伪造 Canvas、WebGL、Audio、字体、CPU 等底层设备指纹。
- 不提供账号密码库、云同步、团队/RBAC、代理市场、RPA 或批量网站操作。
- 不提供默认开启的 Selenium/CDP 远程端口、本地 HTTP 管理 API 或第三方插件市场。
- 不修改 Chromium 内核；需要改变系统级指纹时，后续采用独立且可审计的 Chromium Runtime 或 VM 后端。

### 1.3 指纹语义（必须在产品中明确展示）

“环境隔离”不等于“每个环境都是一台完全不同的电脑”。v0.1 的差异边界如下：

| 属性 | Profile 间是否独立 | 说明 |
| --- | --- | --- |
| Cookie、LocalStorage、IndexedDB、缓存、历史、Service Worker | 是 | 每个 Profile 使用独立 User Data Dir |
| 登录状态、站点权限、扩展数据 | 是 | 不共享 Chromium 数据目录 |
| 代理、出口 IP、DNS 路径 | 可独立 | 取决于用户是否配置不同代理；同一代理会共享出口 |
| 启动页、标签、窗口大小、语言偏好 | 可独立 | 仅设置 Chromium 原生支持的配置 |
| Chromium 版本 | 可独立 | 由 BrowserBuild 策略决定 |
| Windows 时区、字体、GPU、屏幕、平台属性 | 默认共享 | 来自主机，v0.1 不伪造 |
| Canvas、WebGL、Audio、CPU 核数、TLS/HTTP2 特征 | 默认共享或由运行时决定 | 未实现原生 Persona 时必须标为“系统共享” |

UI 和诊断报告必须分别列出“隔离属性”“可配置属性”“主机共享属性”，不得将代理 IP 不同宣传成完整设备指纹不同。

## 2. 总体架构

采用“模块化单体 + 独立 Chromium 进程”。v0.1 不拆微服务，不启动网络监听服务；只有 Chromium 和按会话创建的代理 Broker 是独立进程。

```text
┌──────────────────────────────────────────┐
│ React + TypeScript 管理 UI                │
│ 环境 / 代理 / 诊断 / 备份 / 运行时设置      │
└────────────────────┬─────────────────────┘
                     │ 版本化 Commands / Events
                     ▼
┌──────────────────────────────────────────┐
│ Tauri 2 Desktop                          │
│ 本地资源、CSP、IPC 白名单、依赖装配         │
└────────────────────┬─────────────────────┘
                     ▼
┌──────────────────────────────────────────┐
│ Application Use Cases                     │
│ 创建、编译、启动、停止、诊断、备份、更新     │
└────────────────────┬─────────────────────┘
                     ▼
┌──────────────────────────────────────────┐
│ Domain + Configuration Compiler            │
│ Profile / Session / Proxy / Persona 规则   │
└────────────────────┬─────────────────────┘
                     │ Rust traits / Ports
       ┌─────────────┼──────────────┬───────────────┐
       ▼             ▼              ▼               ▼
   SQLite         SecretStore   BrowserRuntime   ProxyRuntime
   Adapter        (DPAPI/Vault) Adapter          Adapter/Broker
                                                     │
                         ┌───────────────────────────┼──────────┐
                         ▼                           ▼          ▼
                   Chromium Profile A         Profile B    Profile C
                   独立进程+目录               独立进程     独立进程
```

### 2.1 依赖方向

- `domain`：只包含纯 Rust 领域类型、值对象、状态机和规则；禁止依赖 Tauri、SQLite、Windows API、网络库或具体 Chromium 实现。
- `contracts`：前后端共享的版本化 DTO、命令、事件和错误码；不包含数据库句柄或平台对象。
- `application`：编排用例，只依赖 `domain` 和 Ports；不直接读写文件或启动进程。
- `adapters`：实现 SQLite、Windows Credential Manager/DPAPI、Chromium、代理、诊断、备份和签名验证等外部细节。
- `desktop`：Tauri composition root，负责依赖装配、窗口生命周期、权限配置和命令注册；不承载业务规则。
- `ui`：只通过生成的 TypeScript 契约调用白名单命令和订阅事件，不能访问文件、数据库、Shell 或任意 HTTP。

建议 Cargo workspace：`domain`、`contracts`、`application`、`adapters`、`desktop`；React UI 独立于 `ui/`。CI 使用依赖方向检查，阻止 Adapter/Tauri 类型反向进入 Domain。

### 2.2 进程与信任边界

管理器渲染器只处理本地打包资源。网页内容永远在独立 Chromium 中打开，不嵌入管理器 WebView。Tauri IPC 是唯一管理 UI 到 Rust 的入口，每条命令都经过参数校验、当前用户授权、审计和错误映射。Chromium 网页不能直接触达管理器 IPC；v0.1 不开放 loopback 管理 API。

## 3. 核心领域模型

### 3.1 ProfileSpec

Profile 是声明式环境配置，不包含账号密码：

- `id: UUID`、`name`、`tags`、`archived_at`。
- `preset_ref: EnvironmentPresetRef`。
- `proxy_ref: Option<ProxyRef>`。
- `browser_build_ref: BrowserBuildRef` 或稳定通道策略。
- 启动页、语言、窗口、权限策略和 UI 偏好。
- `persona_ref: FingerprintPersonaRef`（v0.1 固定为 `SystemNative`）。
- `schema_version` 与单调递增 `revision`，用于乐观并发控制。

更新运行中的 Profile 不会修改当前会话；新 revision 只在下一次启动时生效。

### 3.2 EnvironmentPreset

预设必须可解释、可版本化，并在编译阶段检查一致性：

- `SystemNative`：使用真实 Windows 时区、字体、GPU、平台和浏览器 UA。
- `RegionalNetwork`：配置代理、语言和区域提示，验证出口区域一致性；不能声称改变系统时区或底层 GPU。
- `QARestricted`：强化权限提示、WebRTC 检查和网络泄漏告警。

每个设置记录来源、Chromium 是否原生支持、实际生效方式、检测值及不支持/冲突原因。禁止随机拼接互相矛盾的 UA、平台、GPU、字体、屏幕和语言。

### 3.3 FingerprintPersona 与 FingerprintRuntime

以端口隔离未来实现：

```rust
trait FingerprintRuntime {
    fn persona_kind(&self) -> PersonaKind;
    fn capabilities(&self, build: &BrowserBuild) -> CapabilitySet;
    fn compile(&self, spec: &ProfileSpec, build: &BrowserBuild)
        -> Result<PersonaPlan, CompileError>;
}
```

v0.1 仅实现 `SystemNativeFingerprintRuntime`，输出实际系统测量和共享标记，不注入脚本。后续的 `NativePersonaChromiumRuntime` 必须让同一 Profile 的 Persona 稳定、可审计、与 UA/平台/GPU 等保持一致，并独立构建和回归测试；Profile、Session、Proxy 和 UI 主流程不变。

### 3.4 ProxySpec

字段包括协议（HTTP/HTTPS/SOCKS5）、主机、端口、可选用户名、`secret_ref`、绕过列表、DNS 行为和超时。密码只进入 Windows Credential Manager 或当前用户 DPAPI；SQLite、命令行、环境变量和日志中不得出现明文。

认证代理使用每会话 loopback Broker：仅监听 `127.0.0.1` 的随机端口，上游秘密只驻留内存，Chromium 只接收本地代理地址，会话停止立即关闭。代理失败不得静默回退到系统直连。

### 3.5 BrowserBuild

描述受信任的浏览器运行时：Provider、版本、通道、安装目录、SHA-256、签名发布者、安装时间和能力集。前端不能传入任意可执行路径。首个 Provider 采用固定版本、官方签名的 Chrome for Testing 稳定版（接口不绑定供应商，后续可增加纯 Chromium/可复现构建）。多个版本并排安装，运行中的版本不原地替换；启动前校验签名、哈希和发布者。

### 3.6 ResolvedLaunchPlan

Profile 不直接生成命令行，必须先编译成不可变计划：

- 已验证 Chromium 路径和版本。
- 核心分配的唯一 User Data Dir。
- 白名单参数与 Preferences。
- Persona capability/警告结果。
- 代理 Broker 句柄。
- Profile revision、计划哈希和审计摘要。

启动器只接受该计划，不接受 UI 的任意参数、路径或 Shell 字符串。

### 3.7 Session 状态机与调度

```text
Stopped → Queued → Starting → Running → Stopping → Stopped
                         └──────────────→ Failed
```

- 同一 Profile 只能存在一个活动 Session。
- 并发上限默认 10；第 11 个请求进入 FIFO 队列且可取消。
- 用数据库租约、Profile 文件锁和 Windows 进程句柄三重识别活动会话。
- Chromium 进程树加入 Windows Job Object；管理器崩溃后回收子进程并清理陈旧锁。
- 关闭管理器默认提示并停止所有会话；后续可增加独立监督进程，但不进入 v0.1。

## 4. 组件与接口

### 4.1 Ports

```text
ProfileRepository
PresetCatalog
SecretStore
BrowserRuntime
ProxyRuntime
FingerprintRuntime
DiagnosticRunner
BackupService
AuditSink
Clock
IdGenerator
```

所有 Port 都以领域类型和可序列化结果交互，禁止泄漏 SQLite connection、Windows HANDLE、Tauri `Window` 或第三方库类型。每个 Port 必须有 adapter contract test。

### 4.2 v1 IPC Commands

```text
profile.list/create/get/update/clone/archive/restore
session.launch/stop/list/cancel_queued
proxy.save/test/remove
diagnostic.run/get/list
runtime.list/install/activate/remove
backup.inspect/export/import
settings.get/update
```

事件：`SessionStateChanged`、`LaunchQueued`、`DiagnosticProgress`、`RuntimeInstallProgress`、`RuntimeUpdateAvailable`、`BackupProgress`。Rust DTO 自动生成 TypeScript 类型，CI 检测前后端契约漂移。

稳定错误码：`VALIDATION_FAILED`、`REVISION_CONFLICT`、`INVALID_PROFILE_STATE`、`PROFILE_LOCKED`、`CAPABILITY_UNSUPPORTED`、`PROXY_CONNECT_FAILED`、`PROXY_AUTH_FAILED`、`RUNTIME_UNTRUSTED`、`RUNTIME_IN_USE`、`BACKUP_WRONG_PASSWORD`、`BACKUP_TAMPERED`、`BACKUP_INCOMPATIBLE`。

严禁通用 `execute`、`invoke`、`readFile`、`writeFile`、`request`、任意命令行参数、通用 JSON 配置袋或动态 DLL 加载。

### 4.3 未来插件边界

插件若在 v0.3 以后引入，必须是进程外 Named Pipe 协议，带版本化 capability manifest、签名/哈希校验、权限清单和资源限制。插件不能直接读 SQLite、Profile 目录或 DPAPI 密文；MVP 完全不加载第三方插件。

## 5. 数据与存储

### 5.1 SQLite 元数据

保存 Profiles/revision、Presets、Proxy 元数据和 secret reference、BrowserBuild、Session 历史、Diagnostic 报告、Audit 事件及 schema migrations。运行状态以进程、租约和锁为事实来源，数据库中的 `running` 仅作历史/展示缓存。

### 5.2 Profile 目录

```text
%LOCALAPPDATA%\Brows\
  app.sqlite
  profiles\<profile-uuid>\userdata\
  runtimes\<provider>\<version>\
  backups\quarantine\
  logs\
```

核心统一创建并验证路径，拒绝目录穿越、junction/reparse point 和非预期符号链接；用户数据目录 ACL 限制为当前用户。删除先归档到回收站，默认保留 7 天，永久删除必须停止会话并二次确认。

### 5.3 加密完整备份

格式 `.browsbak`，包含版本化头、加密 manifest、Profile 元数据和 Chromium User Data：

- 使用 libsodium Argon2id 从恢复口令派生密钥。
- 使用 XChaCha20-Poly1305 secretstream 认证加密。
- 导出代理凭据需单独明确确认，并单独标记风险。
- 导入先解密到受限临时目录，完成哈希、路径、容量和 schema 检查后原子安装。
- 默认生成新 UUID，禁止覆盖已有 Profile；错误口令、篡改、空间不足或中断不得留下半安装数据。
- Chromium DPAPI/App-Bound Encryption 可能导致 Cookie/登录态无法跨 Windows 设备迁移；产品只报告该限制，不破解或绕过系统保护。

## 6. 启动、代理和诊断流程

### 6.1 启动流程

1. 读取 Profile revision，验证未归档且无冲突。
2. 解析 BrowserBuild，校验签名、哈希、能力和目录权限。
3. 将预设、Persona、代理和运行时能力编译为 `ResolvedLaunchPlan`。
4. 检查 User Data Dir、锁、junction/reparse point 和残留进程。
5. 获取并发配额；不足则排队。
6. 启动并握手代理 Broker（若配置）。
7. 使用参数数组创建 Chromium，禁止 Shell；强制保持 sandbox。
8. 将进程树加入 Job Object，发布状态事件和脱敏审计事件。
9. 停止时先请求正常退出，超时后回收 Job Object；释放租约和 Broker。

### 6.2 诊断

本地检查：运行时签名/沙箱参数、目录隔离、代理配置、语言/时区一致性、主机共享属性。主动网络检查（出口 IP、DNS、WebRTC）只在用户点击后执行，并在 UI 显示即将访问的端点。诊断使用临时无登录 Profile 和继承式 DevTools pipe，完成即销毁；正式 Profile 不开放 remote debugging TCP 端口、不注入诊断脚本。

## 7. 安全基线

- Tauri 只加载本地打包资源；严格 CSP，禁用远程导航、内联脚本和 `eval`。
- 默认 `sandbox`、`contextIsolation` 开启，`nodeIntegration` 关闭；不安装 Shell、任意文件或通用 HTTP 权限。
- 管理 UI 与网页完全分离；不提供 localhost 管理 API。
- 禁止 `--no-sandbox`、任意用户参数和可执行路径；不使用 `webSecurity:false`、`sandbox:false`、`webviewTag` 等宽松设置。
- 安装目录采用 `Program Files` 标准 ACL，普通用户不可修改程序资源；用户数据只允许当前用户访问。
- 日志脱敏，不记录 Cookie、Token、密码、完整代理 URL 或启动令牌；设置容量和保留周期。
- 默认零遥测；所有管理端外连目标、原因、开关和结果可审计。
- 更新元数据使用 Ed25519 签名、SHA-256、AuthentiCode 和防降级检查；运行时同时校验项目签名目录、哈希和预期发布者。
- CI 生成 SBOM、许可证清单、依赖漏洞报告和构建来源证明。
- 威胁模型覆盖恶意网页、被攻破的管理渲染器、备份篡改、运行时替换、代理凭据泄漏和本地普通用户攻击；不承诺防御管理员权限、内核恶意软件或上游 Chromium 未知漏洞。

## 8. MoreLogin 简化版 UI

首版只提供六个一级入口：

1. **环境列表**：名称、标签、运行状态、运行时版本、代理、诊断摘要、启动/停止/克隆/归档。
2. **新建/编辑向导**：基础信息 → 运行时 → 代理 → 一致性预设 → 最终检查。
3. **代理库**：协议、地址、凭据引用、连通性、出口 IP、延迟和失败原因。
4. **诊断报告**：实际值、预期值、支持状态、冲突、共享/独立分类和建议。
5. **备份与恢复**：检查、导出、导入预览、冲突处理和进度。
6. **设置**：运行时版本、并发上限、网络行为、日志保留、更新和安全状态。

所有界面只能调用 typed commands；网络检测端点由后端固定 allowlist 管理，不能由 UI 注入任意 URL。

## 9. 分阶段路线

### v0.1：安全隔离多开 MVP

- 工程骨架、Domain/Contracts/Application/Adapters 分层。
- Profile 创建、编辑、克隆、归档/恢复。
- 独立 Chromium 目录、Cookie、缓存、登录态和进程树。
- HTTP/HTTPS/SOCKS5 代理与认证 Broker。
- `SystemNative` 一致性预设和共享指纹诊断。
- 10 并发调度、崩溃回收、审计日志、签名运行时管理。
- 加密完整备份/恢复；默认零遥测。

### v0.2：效率增强

- 分组、批量标签、批量启动队列和资源自适应调度。
- Profile 模板与版本迁移预览。
- 诊断历史对比。
- 经过审核的 MV3 扩展安装（权限清单和签名校验）。

### v0.3：可替换扩展平台

- 独立 `browsd` 监督进程，使 UI 退出后可选择保持会话。
- 默认关闭、当前用户 ACL 保护的 Named Pipe CLI/API，短期令牌鉴权。
- 进程外插件协议、版本和 capability manifest、资源限制。
- 研究用途的 `NativePersonaChromiumRuntime`；每个 Persona 固定 seed、可重复、可审计，禁止临时脚本注入。

### v1.0：更强系统隔离（可选）

- Windows Sandbox/VM Backend，提供独立时区、字体、GPU 和系统网络环境。
- 可选端到端加密同步、团队/RBAC、审批和中央审计。
- 云同步、团队能力和自动化必须建立在 v0.1 安全边界稳定之后。

## 10. 测试与验收标准

### 功能、多开与性能

- 可保存和分页检索 500 个 Profile，查询 P95 < 200ms。
- 10 个不同 Profile 可并发运行，第 11 个请求进入可取消 FIFO 队列，UI 不冻结。
- 管理器空闲工作集 ≤ 250MB（不含 Chromium）。
- 同一 Profile 重复启动被拒绝；停止、崩溃恢复和陈旧锁清理有自动化测试。

### 隔离与代理

- 不同 Profile 的 Cookie、LocalStorage、IndexedDB、缓存、Service Worker、历史和权限互不可见。
- 每个 Profile 使用唯一目录、代理会话和进程树；一个 Profile 崩溃不影响其他环境。
- HTTP、HTTPS CONNECT、SOCKS5、认证失败和超时均有明确错误；代理断线不会回退系统直连。
- 凭据不出现在 SQLite、命令行、环境变量、崩溃转储或日志。

### 指纹与诊断

- UI 明确标记隔离、可配置和主机共享属性。
- Canvas、WebGL、GPU、Audio、字体、CPU 等未修改项目显示“系统共享”。
- 诊断能比较两个 Profile 的实际差异，并对代理/DNS/WebRTC/语言/时区矛盾给出可解释报告。
- 所有主动外部检测均由用户明确触发并展示端点。

### 安全、更新与备份

- 恶意 IPC、命令注入、路径穿越、junction 跳转、任意导航、更新/运行时篡改测试全部失败。
- Chromium sandbox 保持开启，调试 TCP 端口未监听，管理端无默认遥测外连。
- 签名、SHA-256、发布者或防降级校验失败时拒绝安装/启动。
- 正确口令可恢复；错误口令、篡改、空间不足和中断不会覆盖已有 Profile 或留下半恢复目录。
- 跨机器测试区分“配置恢复成功”和“登录态因 DPAPI/App-Bound Encryption 不可迁移”。

### 架构与可替换性

- Domain 单元测试不启动 Tauri、SQLite 或 Chromium。
- 每个 Port 有 contract test；SQLite、Browser Provider 和 UI 可替换而不改 Domain。
- CI 检查 crate 依赖方向和 Rust/TypeScript 契约漂移。

## 11. 实施顺序与变更纪律

1. 先落盘本架构文档和 ADR，再初始化 workspace/UI 骨架。
2. 先完成 Domain、Contracts 和单元测试，再接 SQLite、凭据和进程 Adapter。
3. 每个阶段必须有可运行的垂直切片和验收测试，不跨阶段预埋未验证的“指纹伪装”能力。
4. 任何新增权限、网络端点、IPC 命令、数据库字段或 Runtime capability 都要更新契约、威胁模型、审计事件和测试。
5. 遵循语义版本化；破坏性契约变更增加 schema version 和迁移，不静默兼容。

## 12. 明确假设

- 基准机器为 Windows 11 x64、8 核 CPU、16GB 内存、NVMe SSD。
- Chromium 进程使用官方签名的固定版本运行时；运行时接口不绑定具体供应商。
- 管理器退出默认停止所有 Chromium。
- 用户自行提供代理；首版不代理采购或共享代理池。
- Profile 只保存登录状态和站点数据，不保存账号密码。
- 开源核心、默认零遥测；所有外连必须可解释、可关闭、可审计。
- 后续若增加“不同设备指纹”，必须通过原生 Runtime 或 VM 实现，并保持 Persona 稳定和一致性，不能以 UI 脚本堆叠替代架构能力。
