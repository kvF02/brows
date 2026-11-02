# 可信运行时安装与指纹验收

此流程先验证扩展能否加载，再记录覆盖效果与不一致。安装成功、能力探针成功、
跨上下文一致性成功是三个不同结果；均不代表外部检测站通过率或完整防关联。

## Windows 安装和选择

在项目根目录使用 Windows x64 PowerShell 执行：

```powershell
./tools/install-runtime.ps1
```

安装器仅查询官方 Stable 元数据并下载该版本的官方 win64 资产；不接受自定义 URL，
不允许固定到过时版本。它验证下载地址、ZIP 路径、展开大小、Google LLC Authenticode
签名和可执行文件版本，生成全部文件的 SHA-256 清单，再通过同卷目录重命名发布。
现有版本不覆盖；失败时只清理此次 staging 目录。默认路径是
`%LOCALAPPDATA%/local-brows-desktop/runtimes/<version>/chrome-win64/chrome.exe`。
可用 `-DataRoot` 指定测试用数据根；这不是前端传入任意路径的 IPC。

重启 Brows，在环境编辑器选择 `Chrome for Testing`，再到设置运行指纹能力检测。
系统 Chrome 仍可用于系统原生环境。没有系统 Chrome 时，已安装的受控运行时也可以
独立登记。签名与哈希通过不会自动授予 `Supported`；必须通过生产能力探针。

应用启动时重新验证签名与清单；每次探针和启动前重新验证清单中的全部资源，
禁止新增、缺失或修改文件以及符号链接/reparse point。当前应用进程持有清单快照，
替换磁盘上的清单不能改变本进程已经接受的摘要。清单用于本机完整性，**不是官方
签名的包目录**；安装来源仍依赖官方 HTTPS 与签名校验。系统 Chrome 继续沿用现有
发行者与主可执行文件摘要检查。

升级时重新执行安装器，安装当前 Stable 的新目录，然后重启、选择新版本并重新检测。
旧版本不会自动切换、删除或获得新的检测结果；维护者需及时更新并迁移配置。

## 实测报告

```powershell
npm run observe:fingerprint -- "C:/path/to/chrome.exe" target/fingerprint-observation.json
# 将跨上下文差异也作为命令失败条件：
npm run observe:fingerprint -- "C:/path/to/chrome.exe" target/fingerprint-observation.json --require-consistent
```

报告使用生产 hook 模板、实际内核主版本派生的 UA、语言 Preferences 和独立临时目录。
它比较页面首段脚本、同源 iframe、Dedicated Worker、请求 UA/语言/Client Hints，
并检查 Canvas 重复读取以及同一配置重新启动后的稳定性。只记录允许的请求头，
不保存 Cookie、凭据或用户网页内容；不读取现有会话，也不是用户当前 session 的观测。
所有报告只写到显式指定的输出路径。

结果包括 `hooksObserved`、`repeatStable`、`consistent`、`complete` 和逐项检查。
未收到的字段是 `not_checked`，不能算通过。默认命令对扩展没有生效、重启不稳定或
执行错误返回非零；`--require-consistent` 还会要求所有已纳入比较的上下文信号一致。
`complete` 当前始终为 false，因为未覆盖下面的项目。

## 尚未完成的验收

- Worker、UA-CH 与时区的原生统一覆盖；报告能够揭示差异，不会修复这些差异。
- 现有用户 session 内的只读实测报告；当前只支持独立临时环境。
- 跨源 iframe、Shared/Service Worker、字体、GPU 像素和网络栈的完整观测。
- DNS 公网泄漏需要受控 DNS/HTTP 关联服务，WebRTC 公网出口需要浏览器 ICE/STUN
  实测。当前工具仅访问 localhost，绝不将 HTTP 出口检查当成这些项目的通过证据。

这些缺口应继续作为未通过验收项；不得将本变更描述为完整指纹浏览器交付。

## 自动化验证

`.github/workflows/acceptance.yml` 在 Windows 执行 JavaScript 测试、UI 构建、
Rust workspace 测试及 clippy，然后从官方安装当前 Stable，运行真实生产登记/
能力探针与跨上下文观测，并将 JSON 报告作为 Actions artifact 保存。

本地行为回归入口是 `npm test`。真实运行时验收显式运行：

```powershell
$env:BROWS_ACCEPTANCE_DATA_ROOT = Join-Path $env:TEMP 'brows-acceptance'
./tools/install-runtime.ps1 -DataRoot $env:BROWS_ACCEPTANCE_DATA_ROOT
cargo test -p brows-desktop managed_runtime_acceptance -- --ignored --nocapture
```

只使用新的 `brows-acceptance` 测试目录，不指定真实用户的数据目录。
