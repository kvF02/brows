# 签名运行时与指纹验收

当前生产登记支持已安装的 Google Chrome 和 Microsoft Edge Stable，分别验证
Google LLC 与 Microsoft Corporation 的 Authenticode 签名，并在每次启动前重新
核对登记时的可执行文件 SHA-256。两者的目录由核心发现，前端不能添加任意执行路径。
安装来源使用 [Google Chrome](https://www.google.com/chrome/) 或
[Microsoft Edge](https://www.microsoft.com/edge/download) 官方渠道。

Chrome 继续作为既有 channel-based 配置的默认运行时；只有 Edge 时可用 Edge。
在环境编辑器选择 Microsoft Edge，再在设置运行“检测指纹能力”。签名可信并不自动
授予 Supported；必须通过同一个隔离的生产 MAIN-world 能力探针。浏览器自动更新后
摘要不匹配会阻止启动，需重启应用重新验签、登记并检测。

## 实际安装验证发现的边界

本次在 Windows runner 尝试官方当前 Stable Chrome for Testing，包未通过现有
Authenticode 准入要求，因而没有执行。没有为 CfT 增加 unsigned 例外，也没有将
下载来源或本机生成清单当作发行者签名。生产兼容路径转向签名的 Edge，并以对应
真实探针结果判断扩展是否可用。此前系统 Chrome 的负向结果不能推断 Edge 的结果。

## 实测报告

```powershell
npm run observe:fingerprint -- "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" target/fingerprint-observation.json
# 将跨上下文差异也作为命令失败条件：
npm run observe:fingerprint -- "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" target/fingerprint-observation.json --require-consistent
```

报告使用生产 hook 模板、实际内核主版本派生的 UA、语言 Preferences 和独立临时目录。
它比较页面首段脚本、同源 iframe、Dedicated Worker、请求 UA/语言/Client Hints，
并检查 Canvas 重复读取以及同一配置重新启动后的稳定性。只记录允许的请求头，
不保存 Cookie、凭据或用户网页内容；不读取现有会话，也不是用户当前 session 的观测。
报告只写到显式指定的输出路径。

结果包括 `hooksObserved`、`repeatStable`、`consistent`、`complete` 和逐项检查。
未收到的字段是 `not_checked`，不能算通过。默认命令对扩展没有生效、重启不稳定或
执行错误返回非零；`--require-consistent` 还要求纳入比较的上下文信号一致。
`complete` 当前始终为 false，因为尚未覆盖下面的项目。该独立 Node 工具只加载扩展，
不经过桌面核心的私有会话控制器；它保留扩展单独运行时的差异证据。

## 私有会话控制与当前会话观测

SeedableNoise 的生产启动计划增加 typed 会话策略。Windows 核心通过仅继承两个指定
句柄的 CDP pipe 控制新进程，不开放调试 TCP 端口；Job Object 在控制失败或核心退出时
终止整棵进程树。新标签先停在 about:blank，完成策略后才导航到启动地址。新 page、
iframe、Dedicated/Shared/Service Worker 采用暂停、配置、恢复的顺序；不支持策略的
目标会使会话失败，不静默跳过。

策略安装完成并恢复目标后，控制器先在独立、共享的 8 秒预算内完成自有 about:blank
文档的导航及固定只读就绪检查，再发出真实启动 URL。内部空白文档必须没有导航错误、
已经加载完成；失败会拒绝会话，不重试外网站点。该初始化不依赖外网响应，也不增加
等待网站正文的预算。扣住首个 HTTP 响应的原生测试同时核对空白文档可响应且只有一条历史记录。

首次外网导航在上述初始化完成后发起。控制器在原有 8 秒指令预算内接受导航回复，或匹配
`Page.frameStartedNavigating` 的会话、主 frame 和规范化后的完整启动 URL；不会把子 frame、
其他 URL 或初始 about:blank 的事件当作确认。确认的是浏览器已开始导航，不是站点加载
成功。网页等待响应、网络错误及下载不能换算成代理或网站验收通过，后续真实页面证明
仍须单独读取。匹配状态随首次导航等待结束删除；网址不写入控制取证日志。
导航回复的 `errorText`/`isDownload` 是网页结果；主 frame 确认后浏览器可继续运行并呈现
该结果。协议错误、策略错误或缺失/错误的主 frame 仍失败关闭，不把文档错误冒充控制错误。

`Windows navigation lifecycle regression` 在独立 runner 连续执行六轮：扣住本地页面的
HTTP 响应，要求受控会话先返回且保持存活，再释放响应并核对六类上下文的首脚本信号；
随后执行 Worker 停止/唤醒/更新、renderer 崩溃恢复和九类经典/模块上下文检查。
任一轮失败即停止，没有用成功重试覆盖失败。测试与 debug 依赖构建在失败时输出最近
128 条协议方法和匿名目标标识，不输出参数、URL、脚本、正文或凭据；控制管道断开与
实际回复超时分开报告，并保留既有策略失败原因。panic 清理前的页面分类查询共享
750ms 预算；release 安装包不包含这组取证代码。

启动偶发缺失首段脚本的历史失败继续在 [#28](https://github.com/kvF02/brows/issues/28)
保留。曾实际捕获到已发出导航开始事件、但自有服务器 8 秒内未收到请求，且页面状态
查询未应答；尚不能证明其与此前两次零证明样本有相同根因。空白文档初始化是补齐
启动就绪约束，后续样本通过不能单独证明该偶发问题已根治。

控制器统一 UA、UA-CH、平台、CPU、Memory 和语言。时区使用原生 Emulation 覆盖；
多个目标共享进程时可能已存在覆盖，协议未重新应用成功时必须实际读到规范化后的
期望时区，否则关闭会话。页面和 Worker 共用完整渲染 hook，覆盖 Worker 的
OffscreenCanvas 2D 读取/导出和 WebGL vendor/renderer 声明。字体、真实 GPU 像素、
实时 Audio 和网络栈仍依赖主机。浏览器 API 在不安全上下文中本来缺失时，
不会凭空添加 Device Memory。

控制器为受控页面和 iframe 显式启用 Inspector 生命周期通知（Worker 不支持该 enable 命令）。
target 注册表独立跟踪连接、停止和重载的代次。停止的 Worker 从观测中移除；
重载重新排队配置，同一个 session 串行安装，旧任务或旧观测不能重新发布结果。
丢弃的协议请求会释放有界 pending 槽位；单个已失效 target 的超时不直接终止整个浏览器，
当前 target 的策略安装失败仍会关闭会话。原生停止/重启/更新测试与纯注册表测试分别运行。
Service Worker 版本进入 redundant 时按版本/目标映射移除旧连接和观测，并阻止迟到的
旧任务重新发布；停止状态保留重启入口，版本跟踪与退役记录均有上限。
Service Worker 初次就绪和重复就绪通知不会重复排队配置；只有已停止的代次收到就绪
通知才重新配置。协议对已结束目标返回的精确生命周期错误会使该 Worker 失效并保留
重启元数据，未知协议错误仍然关闭会话。此处理依据上游
[Worker 就绪/停止通知](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/content/browser/devtools/service_worker_devtools_agent_host.cc)
与[待处理命令的结束错误](https://chromium.googlesource.com/chromium/src/+/lkgr/content/browser/devtools/devtools_session.cc)。

诊断页的“读取当前会话设备信号”只允许固定字段，最多读取 32 个 target；IPC 不接受
脚本、URL、Cookie 或页面正文参数。所有 target 共用 8 秒观测预算，单次固定读取最多等待 1.5 秒，协议等待与每次
求值都受剩余预算约束，预算耗尽明确报错。返回前在同一次注册表核对中移除已停止、
重载或断开的 target，避免早先读完的信号在后续读取期间失效后仍被返回。
前端在设备观测和公网检查完成后重新核对活动会话；如果会话已停止或已换成新 ID，
旧结果不再发布，状态重新读取失败也不能算检查成功。
会话列表定期核对进程状态，控制器退出后释放代理
与调度槽位，再启动排队环境。此实现的真实内核通过范围以 Windows acceptance 结果为准。

可选公网探测通过独立测试标签运行，必须先确认展示的端点；配置方法和证据边界见
[公网 DNS / ICE 服务](network-probe-service.md)。未配置服务时保持未检查。

## 尚未完成的能力

- 更多站点、动态 import、顶层 await、Worker 专属进程强杀、OOM 和并发导航场景，以及字体、
  GPU 像素和网络栈的完整观测。当前验收是固定的隔离样本，不是所有网站与 API 的保证。
- 自建公网服务部署后的 DNS/HTTP/ICE 关联验收。仓库中的 loopback 协议测试不能将
  HTTP 出口检查或没有收到 STUN 包换算成公网无泄漏。

不得将本变更描述为完整指纹浏览器交付，也不承诺外部检测站通过率。

## 自动化验证

`.github/workflows/acceptance.yml` 在 Windows 执行 JavaScript 测试、完整 UI 构建、
Rust workspace 测试和 clippy，并验证 Edge 签名、生产登记/能力探针、两个真实 SeedableNoise 会话的启动/停止及跨上下文观测。
生产样本包括页面、同源 iframe、跨源 OOPIF，以及经典/模块 Dedicated、Shared 和
Service Worker，共九类上下文。模块样本在导入依赖的顶层读取，早于入口模块正文，
并与对应经典 Worker 的信号比较。
生产会话验收连续执行六轮，每轮均须通过；任一轮失败即停止，不将重试成功记作本轮通过。
能力探针与验收 HTTP 服务均在接受连接后显式恢复阻塞 I/O，再设置有界读写超时。
延迟首个请求字节的 socket 回归用例防止 Windows 继承非阻塞模式导致预连接被提前关闭。
新增 `private_worker_lifecycle_acceptance` 使用同一个 loopback fixture，验证 Service Worker
停止后不被诊断读取唤醒、消息唤醒后的首段脚本、版本更新后的首段脚本，以及硬件、
时区、Canvas 重复读取和 WebGL 声明的一致性。生产会话验收同时比较六种上下文的
Canvas 与页面是否一致，并在基线支持 WebGL 时要求该上下文仍支持且声明一致。
`private_module_worker_acceptance` 不加载扩展，保留生产 UA/语言启动参数，核对九类
上下文的 UA、平台、完整语言列表、CPU/Memory、时区/Date 偏移、Canvas 和 WebGL 声明。
`private_renderer_crash_acceptance` 仅崩溃隔离测试页面的 renderer；要求旧页面离开观测，
浏览器主进程存活，重载后首段脚本与页面观测恢复策略。它不等于全浏览器或独立 Worker
进程崩溃恢复。两个独立 JSON 报告分别为 `fingerprint-module-worker-acceptance.json`
与 `fingerprint-renderer-crash-acceptance.json`，仍标记 `complete: false`。
JSON 报告作为 Actions artifact 保存；Rust 工具链与仓库的 1.97.1 保持一致。

观测返回竞态的回归用例覆盖：已完成读取的 Worker 在另一读取期间重载、复用旧
target/session ID、共享截止时间已过但回复已入队，以及检测完成前会话停止或更换。
前两类和截止时间用例由 Rust 单元测试验证；前端会话切换用例使用真实 Tauri adapter
与可控的异步回复。新用例的 Windows 执行结果应以对应提交的 CI 为准。

[本批验收记录与 CI 链接](https://github.com/kvF02/brows/pull/7) 汇总实际执行结果。
样本包括页面、同源 iframe、跨源 OOPIF、经典/模块 Dedicated、Shared、Service Worker 的首段脚本。
两组会话分别使用 CPU 3 / Memory 0.5GB / Asia/Tokyo 与 CPU 6 / Memory 2GB /
US/Eastern，核对 UA、低熵 UA-CH、完整语言列表、平台、硬件字段、原生 Intl 时区，
以及固定冬夏日期的 Date 偏移；别名规范化和无效时区失败关闭也纳入断言。

同版本未修改的 Edge 基线不会在这些 Worker fetch 中发送 UA-CH 请求头，因此这些
缺失头保留 `not_checked`，不凭空补头或记作通过。页面/iframe 已发送的头仍必须与
设备声明一致。`fingerprint-session-acceptance.json` 记录这些受限证据，并保持
`complete: false`。

本地行为回归是 `npm test`。真实生产验收显式执行：

```powershell
$env:BROWS_ACCEPTANCE_DATA_ROOT = Join-Path $env:TEMP 'brows-acceptance'
cargo test -p brows-desktop signed_runtime_acceptance -- --ignored --nocapture
$env:BROWS_ACCEPTANCE_BROWSER = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'
$version = [Diagnostics.FileVersionInfo]::GetVersionInfo($env:BROWS_ACCEPTANCE_BROWSER)
$env:BROWS_ACCEPTANCE_VERSION = "$($version.ProductMajorPart).$($version.ProductMinorPart).$($version.ProductBuildPart).$($version.ProductPrivatePart)"
cargo test -p brows-adapters-runtime private_worker_lifecycle_acceptance -- --ignored --nocapture
```

只使用新的 `brows-acceptance` 测试目录，不指定真实用户的数据目录。

## 托管快照的独立控制路径

本机安装项保留 MV3 扩展探针。托管快照改用私有管道首脚本探针：在全新临时目录、
唯一受控页面中暂停目标并安装生产策略，再读取页面首段脚本已捕获的固定字段与 nonce。
读取表达式固定在适配器内部，不提供通用执行 IPC；失败、超时或字段不一致仍拒绝启动。
托管启动不传入 `--load-extension`，仍校验登记表支持状态和全部资源完整性。

[PR #8 的检查入口](https://github.com/kvF02/brows/pull/8/checks) 同时包含安装包、
托管快照、备份闭环、原有九上下文和短时十开验收。只有对应提交的工作流成功且报告存在，
才算本次通过；短时容量报告不代表多小时长稳或完整 UI 操作验收。
