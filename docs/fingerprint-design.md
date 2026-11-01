# 指纹配置设计与验收边界

更新：2026-10-06。本文取代早期 `v0.2-anti-detect-plan.md` 的实现说明。

Brows 的当前基础能力是多环境数据隔离；`SeedableNoise` 是可选的页面读取覆盖层，
不是定制 Chromium 内核，也不代表不同物理设备。默认 `SystemNative` 保留主机信号。
不承诺 Pixelscan、CreepJS 或任何检测站的通过率。

## 配置一致性

每个环境持久化 persona、语言、窗口以及所选可信运行时。预览与启动共用
`PersonaFingerprint::derive(persona, runtime_version)`：UA 的 Chrome 主版本取自
所选运行时，不能从版本池随机产生；非数值运行时版本不能生成 SeedableNoise 配置。
相同 persona 与运行时版本产生相同配置，但不同 seed 可能选中同一个有限 GPU 候选，
因此 seed 不等于唯一设备身份。

克隆环境默认生成由新环境身份分配的独立 seed，保留其余显式配置，原环境不修改。
副本保存后重复启动使用同一 seed；备份恢复保留备份内 seed，以延续原配置。
新 seed 不保证有限 GPU 候选或所有硬件声明均不同，仍需逐字段验证实际运行结果。

语言贯通 Chromium 参数、Preferences 和页面 `navigator.language/languages`。
窗口大小使用环境的 `window`，不能用声明屏幕大小代替；窗口外框、视口和物理屏幕
是不同概念。缩放使用 Chromium 的比例参数，`devicePixelRatio` 保留浏览器原生行为，
仍可随页面缩放变化。Device Memory 在页面中为 number，预览中的 GB 文本仅用于展示。

扩展产物按环境 ID 与完整生成内容的哈希分目录。相同 seed 下改变语言、屏幕、
硬件字段或模板不能覆盖旧配置目录；相同内容编译时复用已存在文件，避免写入正在
使用的扩展。复用时核对文件类型、长度与完整内容；损坏文件明确报错且不自动覆盖。
生成先进入同环境的专用暂存，写完并验证后原子发布完整目录；同环境文件锁协调并发生成，已有完整产物只验证复用，半成品和损坏产物不原地修补。

预览必须指定 `runtimeId`，按 pinned ID 解析并验证构建及可信状态，不能选取“最近安装”
的浏览器替代。返回运行时版本、语言、窗口、屏幕、缩放及覆盖边界；配置变化使旧预览
失效，迟到的异步结果不能覆盖新配置。预览从不启动浏览器，因此所有值都是计划值。

## 扩展发布、恢复与启动检查

扩展布局保持 `extensions/<Profile ID SHA-256>/<产物 SHA-256>`。产物摘要继续绑定
manifest、content.js、background.js 与计划 UA，正常 ID 的旧摘要算法不变；manifest
中的 ID 使用 JSON 转义，不能破坏 JSON 结构。新产物增加固定格式的所有权记录；
旧版完整三文件产物仍可验证复用，缺文件的旧目录保持原样并报错。

同一 Profile 的 `.publish.lock` 使用 OS 文件锁，等待上限 3 秒；不同环境各自加锁。
文件锁随进程退出释放，锁文件本身保留，不能在发布期间删除重建。
暂存在该 Profile 的 `.staging/stage-*`，每个文件以 create_new 写入并 sync_all，
完整校验后通过一次目录 rename 发布。最终路径在三个文件写完之前不可见。
生成失败只清理本次可验证的暂存；已经发布的目录不覆盖、不补写，也不自动删除旧版本。

下一次编译同环境时，在文件锁内检查并清理有匹配所有权记录的残留暂存，再重新生成；
不扫描或删除 userdata。无标记、损坏标记、链接或未知内容保留，不能证明归属时不删除。
每次暂存扫描最多 64 项，单个脚本/manifest 最多 2 MiB，所有权记录最多 2 KiB。
达到限制或不能清理时明确报错，不返回半成品路径。

实际启动在写入 Profile Preferences 前和创建浏览器进程前两次检查：只有一个扩展和一个
计划 UA；环境身份、路径摘要、文件集合及内容必须匹配。含逗号的扩展路径被拒绝，
防止 Chromium 将单参数解释为多个扩展路径。损坏、缺失、额外文件、符号链接或 Windows
reparse point 都会被拒绝。双临时环境观测也走相同检查，基础探针在启动前重新验证产物。

这是应用级目录检查与进程中断恢复；不提供 OS 级隔离或整机断电持久性保证。
自动回归包括多线程/独立进程并发发布，以及在所有权记录、各文件写完后强制结束实际
写入子进程并重试。当前 Linux 已执行上述文件系统回归；Windows 文件锁、junction
和真实 Chrome 行为仍需对应平台的 CI/实机执行。

## 当前覆盖矩阵

| 信号 | 配置机制 | 实际边界 |
|---|---|---|
| UA | Chromium `--user-agent` | 主版本来自选中内核；UA-CH 未统一覆盖 |
| 语言 | 参数、Preferences、MAIN-world getter | 请求行为仍须实测；worker 未覆盖 |
| platform、CPU 核数、Memory、Touch | Navigator getter | 页面读取覆盖，不改变实际 OS 或设备 |
| Screen | Screen getter | 声明宽高、色深；可用区按声明尺寸返回，不模拟系统任务栏 |
| 窗口与比例 | Chromium 参数 | 不是物理显示器替换；缩放和最大化影响实际视口 |
| WebGL vendor/renderer | WebGL1/2 `getParameter` | 仅有效的 unmasked 字符串；GPU 像素、扩展、能力仍真实 |
| Canvas | 2D `getImageData`、HTML `toDataURL/toBlob`、Offscreen `convertToBlob` | 稳定 RGB 低位扰动，导出用副本；未覆盖所有绘制/读取 API |
| Audio | `OfflineAudioContext.startRendering` 与完成事件 buffer | 离线输出稀疏微扰；实时播放、设备采样率保持真实 |
| 时区、字体、worker、TLS/HTTP2 | 无覆盖 | 继续依赖主机/运行时 |

Canvas 的扰动由 seed 与绝对像素坐标确定，使普通数值参数的局部读取与整幅读取一致。
不改变 alpha 或透明黑，不累计扰动，不改原画布；Float16/HDR 读取保留原值。
不支持的输入形式或 API 回退原生行为。WebGL `readPixels`、worker 内 OffscreenCanvas、
`createImageBitmap` 等旁路不能宣称已覆盖。跨源污染画布仍由原生 API 抛出安全错误，
不绕过浏览器同源限制。

Audio 只在离线渲染完成后对非零有限采样值加入微小稳定噪声，buffer 至多处理一次，
Promise 与完成事件读取同一处理后的 buffer。它不改变真实设备，不应冒称“采样率稳定化”。

hook 保持原生 receiver 校验和参数转发，不伪造 `chrome.runtime`，不替换 WebRTC 构造器，
不尝试隐藏 `Function.prototype.toString` 的 JS 痕迹。代理的 WebRTC 策略通过既有 typed
Chromium 参数实施，指纹扩展不自行更改 TURN/ICE 行为。

显式时区覆盖目前没有完整实现；启动编译拒绝此能力，避免保存的声明被静默忽略。
读取覆盖字段和主机共享字段应同时展示；启用 Canvas/Audio 也不能清空共享字段列表。

## 运行时加载是独立验收门槛

扩展使用 MV3 MAIN world、`document_start` 与 frame 匹配。生成 manifest/content.js
只能证明产物存在，不能证明浏览器已载入，更不能证明注入早于网页检测脚本。
官方 Google Chrome 137+ 的 `--load-extension` 存在支持限制；签名可信与扩展加载能力
是两件事。设置页面提供“检测指纹能力”，生产探针使用本机 localhost、
全新临时 profile 与同一生产模板，通过页面首段脚本读取 MAIN-world 哨兵。
运行时分为 unverified / supported / unsupported；未验证或检测失败时，
编译器与实际浏览器适配器均拒绝 SeedableNoise 启动，SystemNative 不受影响。
状态仅保留当前 App 进程；重启登记时恢复未验证，文件摘要变更也必须重新登记和检测。
检测通过只代表 document_start 样本通过，不代表完整 API 或所有 frame 覆盖。

本机官方 Chrome 154 的真实冒烟未观测到 --load-extension 注入；兼容 feature 开关
也未恢复注入。因此当前系统 Chrome 不能被标记为 SeedableNoise 可用。
后续扩展探针的 frame/API 覆盖并引入可信且实测支持的内核。不要通过关闭
Chromium sandbox、长期锁定旧版或信任未知下载源来规避问题。受控 Chromium/Chrome
for Testing 已有固定来源、签名、版本和完整包校验，支持结果仍需实测。

## 测试与验收

自动测试与真实内核观测分开记录，不能将 mock 成功换算成站点通过率。

1. Domain/契约：seed、平台/GPU、语言、窗口校验；共享信号列表；预览 DTO 可序列化。
2. Application：所选内核版本决定 UA；语言/数值 Memory 贯通；窗口与屏幕分离；
   同配置稳定，同 seed 不同配置不覆盖目录；时区覆盖明确报错。
3. 页面 hook 行为：API 缺失互不影响、原生非法 receiver/异常语义、重复读取一致、
   局部/导出一致、透明像素/原画布不变、Canvas/Audio 开关有效、音频不累计。
4. UI：内核/语言/窗口修改后必须重预览；重复选择当前 persona 不重置 seed；显示
   配置值而非观测值；不把数字字符串写入页面 number 字段。
5. 真实运行时（独立步骤）：隔离临时用户数据，验证扩展确实载入，再记录窗口/frame/
   worker 与网络请求的差异。外部检测站仅作为可选人工观察，记录版本与结果，不作保证。

## 后续优先级

可重复执行的行为回归：`npm.cmd run test:fingerprint`。

生产探针验收（显式运行，会使用隔离 headless Chrome）：
`cargo test -p brows-desktop fingerprint_runtime_real_probe_smoke_test -- --ignored --nocapture`。
2026-10-06 在签名可信的 Google Chrome 154.0.8037.97 上执行，探针正确返回
Unsupported，拒绝 SeedableNoise，现有环境及会话未修改，临时目录清理通过。
这是负向能力结果被正确识别的测试，不代表该内核指纹功能已经可用。

P0：官方 Chrome for Testing 的固定来源安装入口、多版本保存、签名/实际版本和完整
文件校验已落地，安装后自动执行现有探针。需要在 Windows 运行实际安装与探针验收，
双临时环境观测和报告已实现，可见窗口验收待完成。当前系统 Chrome 的历史探针未通过，不能据此推断
新安装的 Chrome for Testing 结果；没有实机正向记录前不将本阶段视为完整可用。
操作与手动 CI 入口见 [运行时安装与验收](runtime-installation.md)。

P1：已提供两个临时样本的只读报告，区分“配置、实测、共享、未覆盖”，记录
UA/UA-CH 请求头、iframe/worker、窗口/屏幕、语言、Canvas/Audio、存储与重启结果。
不接触已有登录会话；详见 [实测设计与验收](fingerprint-measurement.md)。下一步执行
Windows 验收，逐步统一 Client Hints 与 Worker；不将未覆盖字段的相等值判为通过。

P2：真实时区机制及多 API 一致性。需要评估 page、worker、Intl/Date 与网络信号的
一致实现，不能仅增加一个 getter 便宣称完整支持。

P3：如需内核级字体、GPU、Audio、网络栈一致性，评估受控 Chromium 内核路线及
安全更新成本。Canvas 输出扰动可以继续改进，但不能替代这项工作。
