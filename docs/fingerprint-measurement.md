# 双临时环境指纹实测

设置 → 浏览器运行时 → 先执行“检测指纹支持”，通过后点击“双临时环境指纹实测”。这是两个全新测试样本的真实 headless 浏览器观测，不读取已有环境或登录态。浏览器预览模式不生成模拟成功报告。

## 流程与报告

1. 解析已验证运行时，检查完整性，占用两个探针名额。同一运行时不能同时检测或新启动 SeedableNoise 会话；已运行的会话不受影响。
2. 使用生产 LaunchPlanCompiler、指纹扩展与偏好设置构造 A/B。样本使用固定不同 seed、CPU、数值 Memory、语言、Screen 与 WebGL 声明。两个样本均无代理绑定，不读取凭据或已保存配置。
3. 两个浏览器都到达本地页面后才放行 HTML，确保采样时有重叠运行。首段脚本立即采样主页面；随后采集同源 iframe、Worker、离线 Audio 以及专用 Cookie / LocalStorage 键。
4. 服务器接收报告后释放页面加载屏障，让 `--dump-dom` 正常退出并刷写存储；随后回收剩余子进程。用各自原临时目录再同时启动一轮，比较重启稳定性。
5. 再次检查运行时完整性，校验所有权标记并清理临时目录，再释放探针名额。覆盖项失败或执行错误会清除先前 Supported 状态；正常退出管理器导致的取消不算能力失败。

报告包含运行时版本、SHA-256、时间、每个样本的配置值、两轮实际读数和逐项判定。采集页面不接收硬件、语言或 GPU 预期值；缺失值返回 null，Rust 独立比较，绝不用配置填充实测。Cookie / LocalStorage 只返回 empty / own / other / unavailable 分类，不返回标记原文。

| 检查 | 判定方式 | 当前边界 |
|---|---|---|
| 主页面 / 同源 iframe | UA、平台、语言、CPU、Memory 的值与类型、Touch、Screen、WebGL 声明，两轮都与配置匹配 | 证明声明覆盖，不代表物理设备；iframe 的 Canvas/Audio 未纳入判定 |
| HTTP UA / 语言 | 本地服务器独立记录主文档请求头并比较 | 只保留 UA / Accept-Language / 两项低熵 UA-CH，不保留认证或 Cookie 请求头 |
| Canvas | 重复读取、重复 PNG 导出、导出后读取、正常退出后重启稳定；A/B 两种摘要分别不同 | 像素摘要与 PNG 文本摘要各自比较，不互相等同；固定样本差异不保证任意 seed 唯一 |
| 离线 Audio | 两个渲染、Promise / 完成事件、重启后摘要稳定；A/B 不同 | 不测真实声卡、实时 Audio 或采样率伪装 |
| Cookie / LocalStorage | 同一 origin；首次为空，写入后及重启时只有本样本标记 | 不涵盖 IndexedDB、Cache Storage、扩展存储或代理出口 |
| Worker / UA-CH / 时区 | 保留真实读数；始终标为未覆盖或未测量 | 值碰巧相等也不会计为已实现 |
| 窗口 / 视口 | 记录 outer、inner、DPR 与请求尺寸 | headless 读数，可见窗口需另验收 |
| 字体 / GPU 像素 / TLS / DNS / WebRTC | 未测量 | 不从本机 HTTP 头部推断网络隔离 |

界面结论为“覆盖项通过”或“覆盖项存在失败”，另列共享和未覆盖维度。完整 JSON 可展开查看。报告仅保留在当前设置页；离开页面后后台任务仍会完成，但没有历史记录或重新读取入口。

## 资源与边界

观测器只监听随机 127.0.0.1 端口，使用随机 128-bit token、固定四个运行身份、Host / Origin 检查及 CSP。四个 HTTP 工作线程、长度上限、单请求超时和有界队列避免加载屏障阻塞报告提交。每轮浏览器等待最多 30 秒、两轮总等待最多 80 秒；文件校验、进程回收与 OS 调用另计。退出管理器会停止接收任务并等待自有资源收尾。

Windows 使用 kill-on-close Job Object。删除只作用于本次拥有且检查通过的探针目录；无法安全清理时返回错误并保留目录供检查，不触及已有环境。不开放调试 TCP 端口、不绕过 sandbox、不接受前端传入 URL、路径、persona 或参数。

观测页面没有外部检测端点，JSON 的 externalEndpoints 为空；这不等于已审计浏览器或系统的全部后台流量。测试样本禁用常规后台网络功能，也不用于验收代理出口。

## 验证与 Windows 实机入口

Rust 回归使用合成观测和真实 loopback HTTP，覆盖错误配置、类型、缺失数据、Canvas/Audio 漂移、存储串用、请求身份、重复提交、并发屏障与能力降级。JavaScript VM 回归检查缺失 API 不补值、类型保真及首段同步读取。这些测试验证采集与判定程序，**不代表真实 Chrome 已通过**。

在 Windows 显式执行：

```powershell
cargo +1.97.1 test --locked -p brows-adapters-runtime official_windows_install_and_probe_smoke -- --ignored --nocapture
```

此入口下载官方 CFT、验证签名和版本、执行两次基础探针，再执行双样本与重启观测。报告先写入 `target/fingerprint-measurement.json`，再断言覆盖项通过；若浏览器未完成采集，可能没有报告。GitHub Actions 手动选择 `official_runtime=true` 后上传该 JSON，测试失败也尝试保留已生成报告。

2026-11-01：当前 Linux 云工作区尚未执行上述 Windows 实机入口，尚无新增 CFT 正向实测记录。已有 Chrome 154 的 Unsupported 历史记录继续有效。下一步需执行 Windows 实机观测、可见窗口与十环境并发/强杀验收，再逐步补齐 UA-CH、Worker、时区和网络泄漏维度。
