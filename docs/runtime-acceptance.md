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
`complete` 当前始终为 false，因为尚未覆盖下面的项目。

## 尚未完成的能力

- Worker、UA-CH 与时区的原生统一覆盖；报告能揭示差异，尚未修复这些差异。
- 现有用户 session 内的只读实测报告；当前使用独立临时环境。
- 跨源 iframe、Shared/Service Worker、字体、GPU 像素和网络栈的完整观测。
- DNS 公网泄漏需要受控 DNS/HTTP 关联服务，WebRTC 公网出口需要浏览器 ICE/STUN
  实测。当前工具仅访问 localhost，不能将 HTTP 出口检查当成这些项目通过的证据。

不得将本变更描述为完整指纹浏览器交付，也不承诺外部检测站通过率。

## 自动化验证

`.github/workflows/acceptance.yml` 在 Windows 执行 JavaScript 测试、完整 UI 构建、
Rust workspace 测试和 clippy，并验证 Edge 签名、生产登记/能力探针、两个真实 SeedableNoise 会话的启动/停止及跨上下文观测。
JSON 报告作为 Actions artifact 保存；Rust 工具链与仓库的 1.97.1 保持一致。

本地行为回归是 `npm test`。真实生产验收显式执行：

```powershell
$env:BROWS_ACCEPTANCE_DATA_ROOT = Join-Path $env:TEMP 'brows-acceptance'
cargo test -p brows-desktop signed_runtime_acceptance -- --ignored --nocapture
```

只使用新的 `brows-acceptance` 测试目录，不指定真实用户的数据目录。
