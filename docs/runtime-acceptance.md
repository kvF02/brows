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

控制器统一 UA、UA-CH、平台、CPU、Memory 和语言。时区使用原生 Emulation 覆盖；
多个目标共享进程时可能已存在覆盖，协议未重新应用成功时必须实际读到规范化后的
期望时区，否则关闭会话。页面和 Worker 共用完整渲染 hook，覆盖 Worker 的
OffscreenCanvas 2D 读取/导出和 WebGL vendor/renderer 声明。字体、真实 GPU 像素、
实时 Audio 和网络栈仍依赖主机。浏览器 API 在不安全上下文中本来缺失时，
不会凭空添加 Device Memory。

target 注册表独立跟踪连接、停止和重载的代次。停止的 Worker 从观测中移除；
重载重新排队配置，同一个 session 串行安装，旧任务或旧观测不能重新发布结果。
丢弃的协议请求会释放有界 pending 槽位；单个已失效 target 的超时不直接终止整个浏览器，
当前 target 的策略安装失败仍会关闭会话。原生停止/重启/更新测试与纯注册表测试分别运行。

诊断页的“读取当前会话设备信号”只允许固定字段，最多读取 32 个 target；IPC 不接受
脚本、URL、Cookie 或页面正文参数。会话列表定期核对进程状态，控制器退出后释放代理
与调度槽位，再启动排队环境。此实现的真实内核通过范围以 Windows acceptance 结果为准。

可选公网探测通过独立测试标签运行，必须先确认展示的端点；配置方法和证据边界见
[公网 DNS / ICE 服务](network-probe-service.md)。未配置服务时保持未检查。

## 尚未完成的能力

- 更多站点、模块 Worker、强制进程崩溃和并发生命周期场景，以及字体、
  GPU 像素和网络栈的完整观测。当前验收是固定的隔离样本，不是所有网站与 API 的保证。
- 自建公网服务部署后的 DNS/HTTP/ICE 关联验收。仓库中的 loopback 协议测试不能将
  HTTP 出口检查或没有收到 STUN 包换算成公网无泄漏。

不得将本变更描述为完整指纹浏览器交付，也不承诺外部检测站通过率。

## 自动化验证

`.github/workflows/acceptance.yml` 在 Windows 执行 JavaScript 测试、完整 UI 构建、
Rust workspace 测试和 clippy，并验证 Edge 签名、生产登记/能力探针、两个真实 SeedableNoise 会话的启动/停止及跨上下文观测。
新增 `private_worker_lifecycle_acceptance` 使用同一个 loopback fixture，验证 Service Worker
停止后不被诊断读取唤醒、消息唤醒后的首段脚本、版本更新后的首段脚本，以及硬件、
时区、Canvas 重复读取和 WebGL 声明的一致性。生产会话验收同时比较六种上下文的
Canvas 与页面是否一致，并在基线支持 WebGL 时要求该上下文仍支持且声明一致。
JSON 报告作为 Actions artifact 保存；Rust 工具链与仓库的 1.97.1 保持一致。

[生产会话及 Worker 生命周期验收记录](https://github.com/kvF02/brows/actions/runs/41665123143) 已通过。
样本包括页面、同源 iframe、跨源 OOPIF、Dedicated/Shared/Service Worker 的首段脚本。
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
