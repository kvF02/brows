# 指纹配置设计与验收边界

本文取代早期 `v0.2-anti-detect-plan.md` 的实现说明。真实内核验收状态见
[运行时验收流程](runtime-acceptance.md)。

Brows 的当前基础能力是多环境数据隔离；`SeedableNoise` 是可选的页面读取覆盖层，
不是定制 Chromium 内核，也不代表不同物理设备。默认 `SystemNative` 保留主机信号。
不承诺 Pixelscan、CreepJS 或任何检测站的通过率。

## 配置一致性

每个环境持久化 persona、语言、窗口以及所选可信运行时。预览与启动共用
`PersonaFingerprint::derive(persona, runtime_version)`：UA 的 Chrome 主版本取自
所选运行时，不能从版本池随机产生；非数值运行时版本不能生成 SeedableNoise 配置。
相同 persona 与运行时版本产生相同配置，但不同 seed 可能选中同一个有限 GPU 候选，
因此 seed 不等于唯一设备身份。

语言贯通 Chromium 参数、Preferences 和页面 `navigator.language/languages`。
窗口大小使用环境的 `window`，不能用声明屏幕大小代替；窗口外框、视口和物理屏幕
是不同概念。缩放使用 Chromium 的比例参数，`devicePixelRatio` 保留浏览器原生行为，
仍可随页面缩放变化。Device Memory 在页面中为 number，预览中的 GB 文本仅用于展示。

扩展产物按环境 ID 与完整生成内容的哈希分目录。相同 seed 下改变语言、屏幕、
硬件字段或模板不能覆盖旧配置目录；相同内容编译时复用已存在文件，避免写入正在
使用的扩展。复用时核对文件类型、长度与完整内容；损坏文件明确报错且不自动覆盖。
生成使用 create_new，不改写已存在文件；完整目录原子发布仍是后续可靠性项。

预览必须指定 `runtimeId`，按 pinned ID 解析并验证构建及可信状态，不能选取“最近安装”
的浏览器替代。返回运行时版本、语言、窗口、屏幕、缩放及覆盖边界；配置变化使旧预览
失效，迟到的异步结果不能覆盖新配置。预览从不启动浏览器，因此所有值都是计划值。

## 当前覆盖矩阵

| 信号 | 配置机制 | 实际边界 |
|---|---|---|
| UA / UA-CH | Chromium 参数与私有 CDP 会话策略 | 主版本来自选中内核；UA-CH 完整版本取自运行中的浏览器 |
| 语言 | 参数、Preferences、各目标会话策略 | 请求行为仍须在实际内核验收 |
| platform、CPU 核数、Memory、Touch | Navigator getter | 页面读取覆盖，不改变实际 OS 或设备 |
| Screen | Screen getter | 声明宽高、色深；可用区按声明尺寸返回，不模拟系统任务栏 |
| 窗口与比例 | Chromium 参数 | 不是物理显示器替换；缩放和最大化影响实际视口 |
| WebGL vendor/renderer | WebGL1/2 `getParameter` | 仅有效的 unmasked 字符串；GPU 像素、扩展、能力仍真实 |
| Canvas | 2D `getImageData`、HTML `toDataURL/toBlob`、Offscreen `convertToBlob` | 稳定 RGB 低位扰动，导出用副本；未覆盖所有绘制/读取 API |
| Audio | `OfflineAudioContext.startRendering` 与完成事件 buffer | 离线输出稀疏微扰；实时播放、设备采样率保持真实 |
| 时区 | 原生 Emulation 与 Worker 读取核对 | 不支持或不一致时关闭会话 |
| Worker | 私有 target 暂停与配置 | UA、语言、平台、CPU、Memory 与时区；Canvas/Audio 旁路仍未完整覆盖 |
| 字体、TLS/HTTP2 | 无覆盖 | 继续依赖主机/运行时 |

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

显式时区进入 typed 启动策略，由私有控制器原生实施并核对 Worker；不能仅保存声明。
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
for Testing 的支持路线仍需可信发布者、安装来源与更新校验设计。

## 测试与验收

自动测试与真实内核观测分开记录，不能将 mock 成功换算成站点通过率。

1. Domain/契约：seed、平台/GPU、语言、窗口校验；共享信号列表；预览 DTO 可序列化。
2. Application：所选内核版本决定 UA；语言/数值 Memory 贯通；窗口与屏幕分离；
   同配置稳定，同 seed 不同配置不覆盖目录；时区进入会话策略。
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

P0：完成可信、可注入运行时的选择/安装路径，并对其真实内核验收。基础能力探针与
失败门禁已落地，但当前系统 Chrome 未通过；此阶段不等于指纹功能完整可用。

P1：session 内固定字段观测和私有 Client Hints 策略已实现；继续扩展跨源 frame、
Shared/Service Worker 及更多 API 的真实内核验收。

P2：原生时区与 Worker 核对已进入控制器；继续扩展 Intl/Date 的多 API 一致性验收。
公网 DNS/ICE 服务代码与配置入口已加入，部署和公网浏览器实测仍是独立门槛。

P3：如需内核级字体、GPU、Audio、网络栈一致性，评估受控 Chromium 内核路线及
安全更新成本。Canvas 输出扰动可以继续改进，但不能替代这项工作。


## 签名运行时与实测工具补充

生产代码增加了对已安装 Microsoft Edge Stable 的发行者校验、目录准入和登记，
并补上能力探针的 Tauri 权限；Chrome 既有配置继续使用原运行时。新增
页面/iframe/Worker/请求头和重启稳定性的隔离观测工具。操作方法、自动验收及
仍未实现的能力见 [运行时验收流程](runtime-acceptance.md)。新路径是否可用，
以对应浏览器版本的真实探针结果为准，不能从签名或产物生成成功推断。
