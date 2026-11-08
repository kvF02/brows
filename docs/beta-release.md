# Windows 内测包与回退

目标是单机内测，不承诺商用指纹抵抗能力。安装包不附带 Chromium，初次启动需要
本机可信 Chrome 或 Edge；SeedableNoise 仍须单独通过对应内核的真实探针。

## 构建和取得内测包

在 Windows 11 x64、Node 24、Rust 1.97.1 MSVC 和 C++ Build Tools 环境执行：

```powershell
npm ci --prefix ui
npm run desktop:package
```

命令固定 Tauri CLI 2.12.4，先构建嵌入式前端，再生成当前用户 NSIS 安装包。
CI 在编译前准备该版本固定的 NSIS 3.13 和 Tauri 插件 0.5.3；缓存只保存下载文件，
每次重新核对官方固定 SHA256，再核对 Tauri 使用的可执行文件/插件摘要后解压使用。
GitHub Release 下载失败时尝试官方 Release asset API（NSIS 另有官方 SourceForge 源），
每个端点等待最多 20 秒，整个准备步骤最多 3 分钟；所有来源均不跳过摘要校验。
WebView2 未安装时，安装器需要联网取得 Microsoft WebView2 引导程序。
设计依据：[Tauri 官方 Windows 安装包文档](https://v2.tauri.app/distribute/windows-installer/)。

每次 PR 和 master 更新会运行 `Windows beta package`，产物包含安装器、单文件应用、
SHA256SUMS、构建提交号、实际 Authenticode 状态以及安装验收报告。SHA256SUMS 中的
`installed/brows-desktop.exe` 是安装器实际落盘的程序摘要，单文件应用有独立记录。
CI 不自动发布 GitHub Release，不自动升级用户机器上的应用。

同一次工作流另附 `brows-windows-client-kit-<commit>`，包含同一安装器、便携程序、摘要、
构建标识和 Windows PowerShell 5.1/7 验收脚本。下载并完整解压后按其中的
[本机验收说明](../tools/client-acceptance/README.md)操作。脚本只校验包及已安装程序、
记录系统版本和逐项人工结果，不自动安装/卸载、不读 Cookie 或凭据、不上传数据。
跳过项目保留 incomplete，人工记录标为 operator-attestation，不能当作自动化网站证据。

仓库没有配置产品代码签名证书或更新签名密钥。当前包应作为**未签名内测包**处理；
浏览器运行时的 Google/Microsoft 签名校验和 Brows 安装包自身签名是两回事。
安装包签名元数据查询在独立进程等待最多 45 秒，超时/失败记录 `authenticodeChecked=false`、
`authenticode=Indeterminate`，不能当作已验证签名。此项仅为内测构建信息，不承担发布准入；
产品浏览器运行时的签名准入继续严格校验。

## 安装与升级验收

CI 在独立 Windows runner 上执行真实静默安装、打开打包后的主窗口、初始化数据库、
正常关闭、覆盖安装以及卸载，并检查应用数据标记保留。
另外在 `windows-11-arm` 桌面客户端运行器上，下载同一 x64 内测包，核对摘要、PE 架构、
Windows ProductType/构建号和 ARM64 本机架构，再执行安装、三次窗口启动/正常退出、
覆盖安装及卸载数据保留。`windows11-arm-acceptance.json` 单独记录 x64 模拟运行和每次
实际使用的 WebView2 版本。它不覆盖 Windows 10、x64 真机、ARM64 原生应用、ARM64
SeedableNoise 能力或完整 UI 流程。此项成功后才能声称对应提交通过该客户端冒烟验收。
随后在 Windows Server 2022 runner 用同一 NSIS 安装器安装并核对应用 SHA256，使用
Microsoft Edge WebDriver 驱动**已安装应用**的 WebView2，确认内嵌资源和真实 Tauri
适配器，再操作并发保存、环境新建/编辑/归档恢复、批量标签和代理编辑、批量启动/停止、
十个运行加一个排队，以及 SystemNative 浏览器启动/停止、
加密备份和恢复到新环境，并核对 UI 重载后的持久数据。测试通过 DOM 输入/点击，
不直接调用应用 IPC 或写数据库；单独生成 `packaged-ui-acceptance.json` 与截图。
同一流程从界面配置测试代理，验证本地 HTTP 代理认证、可信 TLS 成功、不可信证书拒绝和
主动认证拒绝。TLS 测试使用一次性 CI 测试机中的短期测试 CA，结束后清理；产品使用系统
信任库及正常域名校验，没有跳过证书校验的开关。再从界面
启动十个 Edge 环境并保留一个排队环境，强退已安装的 Brows 进程，检查已观测的 Edge
进程树全部退出。重启后核对十一条环境记录、启动一个已有环境，并重新验证代理认证。
代理仅在本地模拟固定检查端点，公网出口、DNS、ICE 和真实站点登录态需在现场实测。
WebDriver 只用于 Windows CI，匹配实际 WebView2 的构建版本并验证 Microsoft 签名；
没有向产品增加调试端口或自动化管理接口。当前 Windows 2025 镜像的 WebDriver 会话创建失败（DevToolsActivePort），因此 UI 自动操作独立执行；安装与生产浏览器验收仍在 windows-latest。报告记录 Windows 和实际 WebView2 版本，不能据此声称较新 WebView2 的自动操作已覆盖。方法依据
[Microsoft WebView2 WebDriver 文档](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/webdriver)。

真实代理、SeedableNoise 设置、其他错误分支和完整桌面流程分别验收。
只有对应提交的工作流成功且安装、客户端和 UI 验收 JSON 均通过，才算本次所列流程通过。

## 长稳和故障恢复验收

`Windows soak and recovery` 对相关同仓 PR 默认分别执行 1 分钟的 SystemNative 和
SeedableNoise 验收。维护者可在 Actions 手动选择模式和 1、30、120 或 240 分钟。
同仓 PR 标题可用 `[soak-2h] ` 触发 SystemNative 120 分钟验收，或用
`[seeded-soak-30m] ` 触发 SeedableNoise 30 分钟验收。

验收在 Windows 托管 runner 上用真实 Edge 启动十个 SystemNative 环境，同时保留一个
排队环境。每个页面持续向本地测试服务发心跳。稳态结束后强制终止一个浏览器根进程，
检查该会话进入失败状态、进程树退出、排队环境启动。再强制终止持有剩余会话的测试宿主，
检查已观察到的浏览器进程全部退出，随后重开原 SQLite 数据库并重新启动一个已有环境。
测试宿主使用生产应用状态和运行时适配器。
为避免 Edge 的后台标签休眠中止持续负载，CI 仅在一次性 runner 上对 `http://127.0.0.1`
设置临时[休眠例外](https://learn.microsoft.com/en-us/deployedge/microsoft-edge-policies/sleepingtabsblockedforurls)，
并将该条件写入报告。

`windows-soak-report.json` 记录实际持续时间、Edge 和系统版本、故障恢复结果与每分钟
进程数、工作集和存活进程累计 CPU 时间。资源数据用于比较运行情况，当前没有资源预算门槛。
SeedableNoise 验收用通过私有管道能力探针的托管 Edge 快照启动十个配置一致、种子各异的环境。
比较本地页面和 Dedicated Worker 首段脚本中的设备字段和 Canvas，验证十组 Canvas 各异，
且持续负载期间重复读取保持稳定。强退一个浏览器后，第十一个环境接替并提供新样本。
再强退宿主，检查已观测的浏览器进程全部退出。新宿主重开原 SQLite 数据库、重新验证同一快照，
只替换测试页面的本地启动地址，依次恢复原十个环境及接替环境，逐一核对页面和 Worker 信号。
报告 `windows-seeded-soak-report.json` 同时记录进程回收、十一组指纹和重启比对结果。

两种验收都使用生产运行时和本地页面。完整安装包 UI 长稳、真实网站会话数据恢复、
断电恢复分别验收。

## 设置与退出

并发配置写入 SQLite，重启后继续生效。改变上限前必须停止运行和排队中的会话；
保存失败不会改变调度器。退出停止自有会话、禁用遥测、禁用自动更新目前为固定基线。
`retainArchivedDays` 仅保存偏好，当前不会自动删除归档环境。

备份导出期间为该环境保留停止状态，阻止它重新启动或同时迁移；其他环境仍可启动和停止。
恢复先安装数据，再发布新的环境记录，失败不留下可启动的半成品环境。
备份加密、恢复和迁移复制在后台执行。重启时在独占数据目录锁下，按持久化操作记录和
数据库状态恢复中断的目录替换，并清理已知的未完成归档及解包目录；详见
[数据恢复边界](data-recovery.md)。真实断电、控制器/磁盘故障和不明确的外部修改仍需独立验收。

## 回退边界

安装器升级/卸载保留 `%LOCALAPPDATA%\local-brows-desktop` 中的应用数据。
升级前先停止所有会话并正常退出管理器，为重要环境导出加密 `.browsbak`，并将应用
数据目录复制到受保护的位置。Windows Credential Manager 凭据不随目录复制转移。

数据 schema 从 5 升到 6 后，旧二进制会拒绝读取新数据库。安装旧包本身不是数据回退：
应先保留当前数据，再在管理器关闭时恢复同版本的完整升级前数据副本。
不要让旧 Chromium 直接打开新版本已升级的用户目录；测试旧运行时使用新的隔离环境。
当前不提供自动 schema 降级、跨 Windows 用户凭据恢复或静默回退功能。

## 尚需独立完成

- 产品签名证书、签名更新源和防降级发布流程。
- 完整桌面 UI 长稳和断电故障恢复验收。
- 公网 DNS/ICE 服务部署和互联网环境实测。

## Windows 10/11 真机与真实代理验收

1. 在 Windows 10 或 Windows 11 x64 的专用测试用户下安装对应提交的 NSIS 包。核对
   `SHA256SUMS.txt` 的安装器和已安装程序摘要，记录 Windows 构建号、WebView2 和
   Edge 版本。测试用户使用自己的空 Brows 数据目录，避免影响已有环境。
2. 在设置确认最大并发为 10。通过界面保存一条授权使用的 HTTP CONNECT 或 SOCKS5
   代理，凭据只填在应用里。先完成代理测试，再为一个测试环境绑定该代理。若使用
   SeedableNoise，保存托管 Edge 快照并通过能力探针，再让环境固定到该快照。
   代理测试只通过 Broker 访问 `https://checkip.amazonaws.com/`，必须完成 TLS 1.2 或更新
   协议的证书、域名校验及 HTTP 200 出口 IP 读取。仅收到 CONNECT 200、明文 HTTP 可用、
   证书不受信任、过期或域名错误都不能标为可用；失败时不回退直连。这里的 HTTPS 指目标
   网站，TLS 加密的上游代理类型仍未启用。独立的环境诊断目前仍是单独披露的 HTTP 检查。
3. 按[公网检测部署条件](network-probe-service.md)配置自有 HTTPS、DNS、STUN 端点
   以及事先已知的代理和解析器出口范围。启动绑定代理的环境，在「诊断」确认端点并
   记录 HTTP、DNS 和 WebRTC 的逐项状态。证据不足时保留「未检查」。用一组明确排除
   测试出口的临时规则做一次反例，确认越界观测会报告失败，再恢复原规则。
4. 从界面启动十个测试环境，确认第十一个排队。选一个允许持久登录的自有测试站点，
   用测试账号登录并确认测试标记。记录已安装 Brows 的 PID、十个 Edge 根进程及
   已观测子进程。只强制终止确认属于本次验收的 Brows PID，检查这些进程全部退出。
5. 重新打开安装版，确认十一条环境记录可读取、旧会话不再占用配额。必要时重新运行
   快照能力探针。用原环境重新打开同一站点，记录是否保留了测试登录态和标记，并重跑
   代理与公网检查。仅汇总版本、持续时间、进程数、逐项结果及失败原因。
