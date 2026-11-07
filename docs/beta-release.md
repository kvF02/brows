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
WebView2 未安装时，安装器需要联网取得 Microsoft WebView2 引导程序。
设计依据：[Tauri 官方 Windows 安装包文档](https://v2.tauri.app/distribute/windows-installer/)。

每次 PR 和 master 更新会运行 `Windows beta package`，产物包含安装器、单文件应用、
SHA256SUMS、构建提交号、实际 Authenticode 状态以及安装验收报告。
CI 不自动发布 GitHub Release，不自动升级用户机器上的应用。

仓库没有配置产品代码签名证书或更新签名密钥。当前包应作为**未签名内测包**处理；
浏览器运行时的 Google/Microsoft 签名校验和 Brows 安装包自身签名是两回事。

## 安装与升级验收

CI 在独立 Windows runner 上执行真实静默安装、打开打包后的主窗口、初始化数据库、
正常关闭、覆盖安装以及卸载，并检查应用数据标记保留。
随后在 Windows Server 2022 runner 使用 Microsoft Edge WebDriver 驱动**同一构建产物**的 WebView2，确认内嵌资源和真实 Tauri
适配器，再操作并发保存、环境新建/编辑/归档恢复、SystemNative 浏览器启动/停止、
加密备份和恢复到新环境，并核对 UI 重载后的持久数据。测试通过 DOM 输入/点击，
不直接调用应用 IPC 或写数据库；单独生成 `packaged-ui-acceptance.json` 与截图。
WebDriver 只用于 Windows CI，匹配实际 WebView2 的构建版本并验证 Microsoft 签名；
没有向产品增加调试端口或自动化管理接口。当前 Windows 2025 镜像的 WebDriver 会话创建失败（DevToolsActivePort），因此 UI 自动操作独立执行；安装与生产浏览器验收仍在 windows-latest。报告记录 Windows 和实际 WebView2 版本，不能据此声称较新 WebView2 的自动操作已覆盖。方法依据
[Microsoft WebView2 WebDriver 文档](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/webdriver)。

这仍不等于代理、SeedableNoise 设置、所有错误分支和完整桌面流程已经验收。
只有对应提交的工作流成功且两份验收 JSON 均存在，才算本次安装及所列 UI 流程通过。

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
SeedableNoise 验收用通过能力探针的同版 Edge 启动十个配置一致、种子各异的环境。
比较本地页面和 Dedicated Worker 首段脚本中的设备字段和 Canvas，验证十组 Canvas 各异，
且持续负载期间重复读取保持稳定。正常关闭后重开原 SQLite 数据库，重新启动十个环境并
逐环境复测同一组信号。报告为 `windows-seeded-soak-report.json`。

两种验收都使用生产运行时和本地页面。完整安装包 UI 长稳、真实网站会话数据恢复、
SeedableNoise 故障恢复及断电恢复分别验收。

## 设置与退出

并发配置写入 SQLite，重启后继续生效。改变上限前必须停止运行和排队中的会话；
保存失败不会改变调度器。退出停止自有会话、禁用遥测、禁用自动更新目前为固定基线。
`retainArchivedDays` 仅保存偏好，当前不会自动删除归档环境。

备份导出期间为该环境保留停止状态，阻止它重新启动或同时迁移；其他环境仍可启动和停止。
恢复先安装数据，再发布新的环境记录，失败不留下可启动的半成品环境。
备份加密、恢复和迁移复制在后台执行。进程崩溃或断电仍可能留下未发布的暂存/数据目录，
不等于完整事务恢复或自动垃圾回收已经实现。

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
- 完整桌面 UI 长稳、SeedableNoise 故障恢复和断电故障恢复验收。
- 公网 DNS/ICE 服务部署和互联网环境实测。
