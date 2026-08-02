# MoreLogin 2.52.0.0 能力与 Brows 差距分析

> 分析时间：2026-08-02。范围仅包括本机程序文件、签名、文件结构和运行中进程的只读元数据。
> 未调用 MoreLogin 私有接口，未连接其 gateway，未读取或记录账号、密码、2FA、Cookie、代理凭据和命令行 token。

## 1. 结论

MoreLogin 不是一个简单的 Electron 管理器。它实际采用多进程组合：

```text
Electron 管理端
  ├─ env-kit.exe                 环境/指纹核心协调
  ├─ envkit-net.exe × N         每个活动环境的网络侧车
  ├─ 定制 Chromium × N          每环境独立进程树
  ├─ zx-sync-sdk.exe            窗口/操作同步能力
  ├─ Selenium WebDriver         自动化入口
  └─ 内置扩展 V2 / V3          浏览器内能力与管理端通信
```

Brows v0.1 已经完成了“本地可信 Chrome + 独立 User Data Dir + 多开调度 + SQLite 元数据”这条安全基线，
但它还不是 MoreLogin 的完整替代品。当前最大差距不是 UI，而是指纹一致性内核、每环境网络侧车、代理/DNS/WebRTC
闭环、自动化和扩展管理。

## 2. 已确认的 MoreLogin 能力

### 2.1 程序与运行时

- 主程序是 Electron/Chromium 桌面应用，安装目录包含 Electron、V8、Chromium 和 GPU 运行库。
- 主程序 `MoreLogin.exe` 及 `env-kit.exe`、`envkit-net.exe`、`zx-sync-sdk.exe` 均有有效 Authenticode 签名，
  签名主体为 `EASYANT PTE. LTD.`。
- 有效签名只能证明发布者和文件完整性，不能单独证明没有后门或没有不必要的数据上传。
- 当前包内提供定制 Chromium 144；本地缓存还保留 132、133、144 三套核心。
- 本次观察到活动浏览器实际运行在 `chrome_64_133.3`，说明它支持环境绑定不同核心版本，而不是永远使用最新核心。

### 2.2 多环境进程模型

- 观察到 13 个定制 Chromium 子进程，以及 3 个独立 `envkit-net.exe` 进程。
- 这与“每个打开的环境有独立浏览器进程树和网络侧车”的模型一致。
- 管理端自身还有多个 Electron renderer/GPU/utility 子进程。
- 此前对启动参数的脱敏检查显示多个 Electron 子进程使用 `--no-sandbox`。这扩大了管理端内容被利用后的影响面，
  是 Brows 不应复制的做法。

### 2.3 指纹与浏览器核心

- 包内存在 `env-kit.exe`，并带有专用 Chromium 包、`analysis_sa.exe`、`webdriver.exe`。
- 定制包名称包含 `chromium_morelogin...`，说明其指纹能力主要在定制 Chromium/本地核心层，而不只是页面启动后注入 JavaScript。
- 本地 `env-kit/Core` 约 1.24 GB，符合多版本完整浏览器核心的体量。
- 可确认其具备指纹配置基础设施；具体 Canvas/WebGL/Audio/字体噪声算法、稳定种子和一致性规则仍未反编译验证，
  不能仅凭 UI 文案断言实现质量。

### 2.4 网络与代理

- 每个活动环境出现独立 `envkit-net.exe`，表明代理处理不是只有 `--proxy-server` 参数。
- 本地存在 `ipCheckList.json`，UI 资源包含代理和 IP 检查能力。
- 合理推断网络侧车负责部分代理协议适配、认证、连通性或出口检测；DNS/WebRTC 是否完全被同一路由约束仍需受控网络实验验证。
- 本次没有读取进程 token，也没有调用本地 HTTP server 或远程 gateway。

### 2.5 自动化、同步与扩展

- 程序包明确包含 Selenium WebDriver，并携带 Windows/macOS/Linux driver 管理代码。
- 包含 `zx-sync-sdk.exe` 及 Python 3.7 运行时，说明窗口同步/操作同步是独立能力，不只是前端按钮。
- 包含 `SBBackground.zip` 和 `SBBackgroundV3.zip`，说明同时兼容不同 Chrome 扩展清单代际。
- UI 静态资源中可确认存在自动化、Cookie、窗口同步、代理、团队、扩展、API、RPA、云手机等功能模块文案；
  这些模块是否全部对当前账号开放、是否完全本地实现，尚未验证。

### 2.6 数据与迁移

- 当前检查的 XLSX 工作表范围为 `A1:Q501`，但其中 499 行是预格式化空白行，实际只有 `P-21` 一条有效环境记录；
  不能用工作表范围推断导出数量。文件字段包含名称、平台、账号、密码、2FA、Cookie、代理、分组、标签、备注、UA 等。
- XLSX 不包含完整 Chromium User Data、LocalStorage、IndexedDB、Service Worker、扩展数据和部分加密登录材料。
- 因此仅靠 XLSX 不可能“完全无缝”恢复原登录环境；最多做到元数据、代理和部分 Cookie 的分阶段迁移。

## 3. Brows 当前差距

| 能力 | MoreLogin 观察结果 | Brows 当前状态 | 决策 |
|---|---|---|---|
| 多开与存储隔离 | 每环境独立进程树 | 已完成独立 User Data Dir、重复启动保护、并发调度 | 保持并强化崩溃恢复 |
| 浏览器可信链 | 定制核心，发布者签名有效 | 官方 Google Chrome，签名和 SHA-256 已验证 | Brows 当前安全透明度更好 |
| 多版本核心 | 132/133/144 并存 | 单一受信 Chrome 150 | 增加版本目录与生命周期管理 |
| 指纹 Persona | env-kit + 定制 Chromium | 仅系统原生指纹，明确标记主机共享 | 高优先级，但必须做一致性编译与实测 |
| 环境网络侧车 | 每活动环境一个 envkit-net | 主要依赖 Chrome 代理能力 | 增加独立 ProxySession 适配器 |
| DNS/WebRTC/出口闭环 | 有 IP 检查与网络侧车 | 诊断骨架，尚无完整主动检查 | 先实现可解释诊断再加策略 |
| 代理凭据 | 支持导出字段 | 已设计 Windows Credential Manager，UI 尚未闭环 | 优先完成，SQLite 只存 secret ref |
| 自动化/API | Selenium/WebDriver/RPA 模块 | 未实现 | 使用进程外、按能力授权的自动化服务 |
| 窗口同步 | 独立 sync SDK | 未实现 | v0.4 后实现，不放入核心进程 |
| 扩展管理 | 内置 V2/V3 扩展 | 未实现 | 白名单、哈希固定、权限审计后再开放 |
| 团队/云同步 | UI 有团队与云模块 | 本地单机、零遥测 | 保持默认本地；云同步必须端到端加密且可选 |
| MoreLogin 导入 | 原生数据源 | 已实现 XLSX 安全预览和非敏感字段导入 | 后续分层导入代理/Cookie |
| 沙箱安全 | Electron 子进程使用 `--no-sandbox` | Tauri 管理端，未开放生产调试端口 | 不复制 `--no-sandbox` 和本地 token API |

## 4. 推荐实施顺序

### 阶段 A：安全简化版收尾（当前）

1. 完成 UI 可读性、500+ 环境分页和批量操作。
2. MoreLogin XLSX 只读预览；首批只导入名称、分组、标签、平台分类。
3. 导入报告按行给出失败原因，绝不把敏感单元格写进日志或错误消息。
4. 完成停止、崩溃恢复、陈旧锁清理和实际 10 开压力测试。

### 阶段 B：每环境网络闭环

1. 新增 `ProxySession` 端口，具体协议放在进程外网络侧车适配器。
2. 代理密码只进入 Windows Credential Manager；数据库保存 opaque secret reference。
3. DNS、WebRTC、出口 IP、语言、时区形成一份启动前静态检查和用户触发的主动诊断报告。
4. 禁止静默直连回退；代理失败即停止该环境并显示可解释错误。

### 阶段 C：一致性 Persona 内核

1. Persona 是版本化声明，不允许 UI 直接拼 Chromium 参数或注入任意脚本。
2. `PersonaCompiler` 根据浏览器版本生成不可变启动计划，覆盖 UA/Client Hints、平台、语言、时区、屏幕、
   Canvas、WebGL、Audio、字体和设备能力之间的一致性约束。
3. 同一环境的种子稳定；克隆默认生成新种子；升级核心前必须跑回归指纹矩阵。
4. 先用于隐私隔离和 QA 测试，不承诺绕过站点风控，也不提供“随机到不可检测”的误导选项。

### 阶段 D：自动化与扩展

1. 自动化作为独立服务，通过版本化 Named Pipe/JSON-RPC 接口通信。
2. 每个任务声明可访问的环境、域名、下载目录和超时；默认禁止读取其他环境数据。
3. 扩展必须白名单、固定哈希、显示权限差异；禁止任意扩展直接访问 Brows 数据库或保险库。
4. 窗口同步复用同一事件协议，但与环境启动器解耦。

### 阶段 E：可选团队能力

1. 保持本地优先和零遥测默认值。
2. 若增加同步，服务端只保存端到端加密密文；设备密钥、成员撤销和审计事件独立设计。
3. 不上传 Chromium Profile 明文，不把代理凭据与环境归档使用同一密钥。

## 5. 不应复制的部分

- 不直接复用、注入或修改 MoreLogin 的 `env-kit`、定制 Chromium、扩展或私有接口。
- 不依赖其本地 HTTP token、远程 gateway 或当前进程状态完成迁移。
- 不使用 `--no-sandbox`，不在生产环境开放 remote-debugging TCP 端口。
- 不把密码、2FA、Cookie、代理凭据放入 SQLite、前端状态、审计记录或普通日志。
- 不把“不同 User Data Dir”宣传成“每个环境都有完全不同的硬件指纹”。

## 6. 仍需验证的项目

- 在受控测试代理下抓取 DNS/WebRTC/HTTP 出口，确认 `envkit-net` 的实际边界。
- 对两个 MoreLogin 环境运行同一离线指纹页，比较 Persona 稳定性和字段一致性。
- 检查定制 Chromium 主程序及浏览器内加载扩展的签名/哈希链。
- 评估关闭 MoreLogin 云端连接后，本地环境能否完整启动，以及哪些能力强依赖服务端。
- 验证其 Cookie 导入对 Chromium App-Bound Encryption 的实际成功率。
