# 官方运行时安装与验收

设置页提供“安装并检测官方运行时”，下载 Chrome for Testing 当前 Windows 64 位稳定版。安装功能仅在 Windows 桌面版启用，浏览器预览不会执行真实安装。

## 使用与版本选择

1. 在设置页点击安装，页面会显示查询目录、下载、解包、验证、保存和探针检测阶段。下载显示实际接收字节；提交前可以取消。
2. 安装完成后检查指纹探针结果。Supported 只证明现有 headless 本地首段脚本样本通过，不代表完整设备指纹或检测站通过率。失败或未验证时继续拒绝 SeedableNoise 启动。
3. 在新建或编辑环境中选择 Chrome for Testing 对应版本。已有环境绑定的版本保留；备份恢复和克隆不会偷偷替换其运行时。
4. 再次点击安装会查询最新稳定版，相同已登记版本复用现有文件并重新检测，新版本安装到新目录。系统 Chrome 的稳定通道保持；没有系统 Chrome 时，CFT 稳定通道采用已验证安装中的最高版本。Pinned 环境保留原版本。
5. 重启后重新检查已安装包，指纹支持恢复为 Unverified，需要再次手动检测。没有自动下载或后台更新轮询；低于已登记 CFT 版本的官方目录结果被拒绝。

## 来源与完整性

应用只构造以下下载请求，不接受前端提供的 URL、路径、发行者、执行参数或任意安装包：

- 目录：`https://raw.githubusercontent.com/GoogleChromeLabs/chrome-for-testing/main/data/last-known-good-versions-with-downloads.json`
- 安装包：`https://storage.googleapis.com/chrome-for-testing-public/<四段版本>/win64/chrome-win64.zip`

目录必须为 Stable、恰有一个 win64 Chrome 下载项，并与程序构造的地址完全相同。HTTPS 使用平台信任链，关闭自动重定向，限制连接、读取和总请求超时；下载等待期间检查取消。仅用户明确点击安装时访问这些服务。

解包拒绝目录穿越、绝对路径、反斜线、NTFS stream、保留设备名、大小写冲突、链接、特殊文件和加密 ZIP。下载上限 512 MiB，展开上限 2 GiB，最多 4096 条目。检查必要文件后验证 `chrome.exe` 的 Windows Authenticode Google LLC 发布者，以及 PE 版本资源中的实际版本；实际版本必须与目录一致。

安装到应用的 `runtimes/chrome-for-testing/cft-<版本>-<归档 SHA-256>`，目录只创建一次。独立 `receipt.json` 保存版本、归档摘要及包内每个文件的长度和摘要。启动登记、浏览器启动以及探针前后均检查包文件集合与摘要，因此修改 DLL 或添加文件也会拒绝使用。收据是本地完整性基线，不是远程签名版本目录；系统 Chrome 继续使用已有的发布者与主程序摘要校验。

下载和解包先进入 `.install-<随机 ID>`，验证完成后改名发布。取消或失败只清理本次专用暂存；重启重试清理同命名空间，保留链接、未知顶层内容和无法安全判断的布局。已发布包不因探针失败而删除，仍可在设置中重新检测。安装目录最多保留 64 项（包括待检查暂存），达到上限时拒绝新增，需先检查未使用版本。此流程不提供断点续传、旧版本自动删除或无人值守更新。

退出时先停止接收安装和探针任务，取消尚未提交的安装，等待已开始的工作释放资源后再释放数据目录锁。探针使用 Windows kill-on-close Job Object 回收进程树；同一构建只运行一个探针任务，整个管理器最多同时运行两个临时浏览器；双样本观测独占两个名额。会话启动、停止和查询移入阻塞线程池，完整包校验不在 Tauri 主线程执行。证书验证和文件 OS 调用仍需等待系统返回。

## 验证记录与实机入口

本次在 Linux 云环境核对到官方稳定目录版本为 `156.0.8078.80`，这里只记录目录检查，不代表已执行 Windows 安装或获得指纹正向结果。自动回归使用隔离的伪安装包与测试验证器，覆盖恶意路径、签名拒绝、取消/提交竞态、下载等待取消、DLL 篡改、重启加载、版本选择和退出等待；这些结果与真实内核验收分开记录。

Windows 实机执行：

```powershell
cargo +1.97.1 test --locked -p brows-adapters-runtime official_windows_install_and_probe_smoke -- --ignored --nocapture
```

该显式测试下载当时的官方稳定版到临时目录，检查真实签名与版本，连续执行两次本地指纹探针并要求 Supported，再执行两临时样本的页面 / iframe / HTTP 头 / Canvas / Audio / 存储与重启观测。完整报告写入 `target/fingerprint-measurement.json` 后才断言覆盖项通过；失败会保留已生成报告及日志原因。默认测试不会下载浏览器。

本轮通过 177 项可移植 Rust、22 项桌面和 35 项 JavaScript 自动测试，前端构建、完整 workspace 严格 Clippy 与运行时的 Windows MSVC 目标交叉 Clippy 通过。真实浏览器验收及 Windows 专用测试需在相应平台显式执行，不计为本次实机通过记录。

GitHub Actions 的 Development checks 增加手动运行入口：选择 `official_runtime=true` 后，独立 Windows job 执行上述验收并上传已生成的 JSON 报告。该 job 尚未在本次会话触发，不能将新增 CI 配置视为测试已经通过。

双样本观测的具体判定与边界见 [实测说明](fingerprint-measurement.md)。真实加载和 headless 观测通过后，仍需执行可见窗口、十环境压力与网络泄漏验收；Worker、UA-CH 和时区等未覆盖项继续明确列示。
