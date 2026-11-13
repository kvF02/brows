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
| 语言、CPU 核数 | 私有会话的原生 Emulation；独立扩展路径仍用 getter | 页面、iframe 与 Worker 在恢复首脚本前设置；保留原生 getter，不改变物理 CPU 或调度能力 |
| platform | 页面/iframe 原生 Emulation；跨系统 Worker 仍需 getter 覆盖 | Worker 的原生实现固定读取主机平台；配置与主机相同时保留原生 getter，不改变物理 OS |
| Touch | 私有会话的原生触控模拟；独立扩展路径仍用 getter | 同时影响原生 maxTouchPoints、指针/悬停特征和受控输入；Worker 保留原本缺失的接口 |
| Memory | Navigator getter | 仍是页面/Worker 读取覆盖，物理内存不变 |
| Screen 尺寸/方向 | 原生屏幕模拟候选，尚未合入；独立扩展仍用 getter | 普通 Edge 的 OOPIF 首脚本仍泄漏主机屏幕；须编译并验收 C++ 传播补丁。可用区为整个模拟屏幕，不模拟系统任务栏、物理显示器拓扑或 HDR |
| Screen 色深 | 匹配时保留原生 getter，否则读取覆盖 | colorDepth / pixelDepth 不匹配配置时仍是 JS 覆盖，不改变实际显示器色深 |
| 窗口与比例 | Chromium 参数 | 不是物理显示器替换；缩放和最大化影响实际视口 |
| WebGL vendor/renderer | WebGL1/2 `getParameter` | 仅有效的 unmasked 字符串；GPU 像素、扩展、能力仍真实 |
| Canvas / 部分 WebGL 像素 | 2D `getImageData`、HTML `toDataURL/toBlob`、Offscreen `convertToBlob`；默认 WebGL1/2 缓冲区与 WebGL2 RGBA8 renderbuffer 的 CPU 读取 | 稳定 RGB 低位扰动，导出用副本；覆盖不透明 `Uint8Array` 读取、PACK 对齐及 WebGL2 行长度、跳行、跳像素和目标偏移；默认缓冲区限 sRGB |
| Audio | `OfflineAudioContext.startRendering` 与完成事件 buffer | 离线输出稀疏微扰；实时播放、设备采样率保持真实 |
| 时区 | 原生 Emulation 与 Worker 读取核对 | 不支持或不一致时关闭会话 |
| Worker | 私有 target 暂停与配置，共用完整 hook | UA、语言、平台、CPU、Memory、时区、OffscreenCanvas 读取/导出和 WebGL 声明；缺失 API 保持缺失 |
| 字体、TLS/HTTP2 | 无覆盖 | 继续依赖主机/运行时 |

Canvas 的扰动由 seed 与绝对像素坐标确定，使普通数值参数的局部读取与整幅读取一致。
不改变 alpha 或透明黑，不累计扰动，不改原画布；Float16/HDR 读取保留原值。
不支持的输入形式或 API 保留原生行为。WebGL `readPixels` 先执行原生读取，保留 receiver、
异常及参数转换，不调用会消耗错误状态的 `getError`。仅在已知读取缓冲区尺寸、RGBA /
UNSIGNED_BYTE、完整范围和足够目标空间时扰动不透明 RGB。默认缓冲区限 sRGB，使用
drawingBufferHeight 转换到 Canvas 左上坐标。WebGL2 另支持完整、单采样 RGBA8
renderbuffer FBO：按实际 READ_BUFFER 选择颜色附件，原生查询该 renderbuffer 的尺寸，
查询后在 finally 中恢复原 RENDERBUFFER_BINDING，不改读写 framebuffer、附件或绘制内容。
纹理附件没有可查询的完整尺寸，不能以画布尺寸或历史 texImage 调用猜测；继续保留原生结果。
WebGL2 的 PACK_ROW_LENGTH、PACK_SKIP_ROWS、PACK_SKIP_PIXELS 与 dstOffset 共同确定
实际目的范围；每行填充、跳过区域、TypedArray view 之外的字节不变。非法跨行布局或不足
空间保持原生错误和目标内容。两类受支持缓冲区均按自己的高度转换像素坐标。
不改 GPU 缓冲区、alpha 或半透明像素。纹理 FBO、WebGL1 FBO、非 RGBA8 renderbuffer、
多采样直接读取、浮点格式、PBO、共享内存、默认 P3/HDR、越界读取及 `createImageBitmap` 等路径仍未覆盖。
这些原生路径可用于读取未扰动内容，不能声称 GPU 像素已全面隔离。
跨源污染画布仍由原生 API 抛出安全错误，
不绕过浏览器同源限制。

扩展与私有会话控制器共用同一个脚本生成器，显式区分独立扩展与原生 Navigator 模式。
私有模式不替换 language/languages、hardwareConcurrency、maxTouchPoints 和页面 platform 的 getter；
CPU 通过 `Emulation.setHardwareConcurrencyOverride`，语言/平台通过原生 UA override，
页面触控通过 `Emulation.setTouchEmulationEnabled` 设置。支持私有 pipe 的运行时在探针与
生产启动中统一走该路径，不再同时加载一份会重新覆盖这些字段的 MV3 脚本。
独立扩展仍作为旧路径的有界观测工具，不应据其结果宣称原生控制生效。

私有控制器在顶层页面暂停期间设置屏幕尺寸与方向，由 Chromium 传播到 iframe。
本分支 Screen 原生切换尚未达到合入条件。Windows Server 2022 的签名 Edge 实测中，
顶层要求 1440×900，但跨进程 iframe 的首脚本原生宽度仍为 656。普通 CDP 设置
不能修复该传播缺口，详见 [原生屏幕候选](../native/chromium/screen/README.md)。
真实私有能力探针已增加独立跨进程 iframe 首脚本、原生 getter 与 CSS 检查，并校验
消息来源窗口、origin 和 nonce；此路径的 Windows 负向验收仍须以对应 CI 为准。
以下为候选实现的验收要求，须先完成内核编译、正向验收及签名运行时集成。

Screen 的 width、height、availWidth、availHeight 保留原生 getter，并在页面启动前同时
核对 CSS `device-width` / `device-height` 媒体查询；失败时拒绝配置。屏幕模拟使用桌面
模式，不指定视口宽高或设备缩放，不以声明屏幕大小代替真实窗口/视口；DPR 继续由
启动比例和浏览器原生缩放决定。色深仅在主机值不匹配时保留读取覆盖。Worker 中不会
凭空增加 Screen / Window。此能力不创建物理显示器，也不隔离 ScreenDetails 等完整
多显示器能力、GPU 或 HDR 状态。

原生指令不支持或 Worker 读取不符时拒绝会话；主页面在外网导航前的空白文档中也核对
这些值。触控为 0 表示关闭模拟，不能抹去真实触屏：如果设备仍原生返回非零，配置会
明确失败，不回退成 JS 伪造 0。启用触控会采用 Chromium 的粗指针/无悬停行为，
不等同于为桌面主机增加物理触摸设备。浏览器版本差异和之后的设置更新仍需实测。
跨系统 Worker 的 platform 是明确保留的脚本覆盖例外：上游 NavigatorBase 直接返回主机
平台，CDP 中保存的平台值未进入此读取路径；同主机平台时不重复包裹原生 getter。
原生 getter 的保留不代表全部脚本痕迹不可检测，也不是本项目定制的 Chromium 内核。

Worker 使用 `WorkerNavigator` 与
OffscreenCanvas，不创建 Window、Document、Screen 或原本缺失的音频 API。
同一 realm 重复安装相同策略直接返回，避免 Canvas XOR 扰动相互抵消或 Audio 累计；
不同策略重入明确失败，生产控制器按策略错误关闭会话。幂等标记只保留比较闭包，
不把原始 seed 或完整配置写到可读的全局字段；这些 JS hook 仍可被检测。

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

此前官方 Chrome 154 的真实冒烟未观测到 --load-extension 注入；兼容 feature 开关
也未恢复注入。因此当前系统 Chrome 不能被标记为 SeedableNoise 可用。
后续扩展探针的 frame/API 覆盖并引入可信且实测支持的内核。不要通过关闭
Chromium sandbox、长期锁定旧版或信任未知下载源来规避问题。受控 Chromium/Chrome
for Testing 的支持路线仍需可信发布者、安装来源与更新校验设计。

## 测试与验收

自动测试与真实内核观测分开记录，不能将 mock 成功换算成站点通过率。

1. Domain/契约：seed、平台/GPU、语言、窗口校验；共享信号列表；预览 DTO 可序列化。
2. Application：所选内核版本决定 UA；语言/数值 Memory 贯通；窗口与屏幕分离；
   同配置稳定，同 seed 不同配置不覆盖目录；时区与完整 persona 进入会话策略；
   扩展和会话生成相同的读取覆盖脚本。
3. 页面 hook 行为：API 缺失互不影响、原生非法 receiver/异常语义、重复读取一致、
   局部/导出一致、透明像素/原画布不变、Canvas/Audio 开关有效、音频不累计；
   Worker 无窗口 API 的行为和重复策略注入。
   WebGL 另验整图/局部读取、坐标原点、PACK 对齐、目标偏移、已有 GL 错误保留、原生失败
   不改目标、非法 receiver/转换副作用、上下文丢失和关闭开关。
4. UI：内核/语言/窗口修改后必须重预览；重复选择当前 persona 不重置 seed；显示
   配置值而非观测值；不把数字字符串写入页面 number 字段。
5. 真实运行时（独立步骤）：隔离临时用户数据，验证扩展确实载入，再记录窗口/frame/
   worker 与网络请求的差异。外部检测站仅作为可选人工观察，记录版本与结果，不作保证。

`private_native_navigator_acceptance` 在签名 Edge 上切换 CPU 3/7/5、触控 0/5、
Windows/Linux 平台和英语/波兰语，检查六类上下文的 getter 来源、原生 receiver 校验与
实际返回值。启用触控时另核对 CSS pointer/hover、可信双指输入事件、输入后的数值和
页面重载后的首脚本。现有九类经典/模块首脚本、Worker 生命周期和 GPU 样本也核对这些
getter 来源；跨系统 Worker 平台覆盖须明确记录为脚本，其余指定原生字段仍须通过。
以上描述是验收要求；通过范围以对应提交的 Windows CI 为准。

## 后续优先级

可重复执行的行为回归：`npm.cmd run test:fingerprint`。

生产探针验收（显式运行，会使用隔离 headless Chrome）：
`cargo test -p brows-desktop fingerprint_runtime_real_probe_smoke_test -- --ignored --nocapture`。
2026-10-06 在签名可信的 Google Chrome 154.0.8037.97 上执行，探针正确返回
Unsupported，拒绝 SeedableNoise，现有环境及会话未修改，临时目录清理通过。
这是负向能力结果被正确识别的测试，不代表该内核指纹功能已经可用。

P0：已安装签名 Edge 的发现、准入、能力探针与生产会话验收已通过。托管内核安装、
安全更新与签名目录仍是后续工作；本机 Chrome 的既有负向探针结果继续保留。

P1：session 内固定字段观测和私有 Client Hints 策略已实现，并已实测页面、同源 frame、
跨源 OOPIF、经典与模块 Dedicated/Shared/Service Worker 的首段脚本。九种上下文的
读取一致性、模块导入依赖、Service Worker 停止/消息唤醒/版本更新，以及隔离页面
renderer 崩溃后的重载已纳入原生验收。
继续扩展更多站点、动态 import/顶层 await、Worker 专属进程强杀与并发导航和其他 API；缺失请求头保留未检查。

P2：原生时区、Worker 核对、别名规范化及 Intl/Date 冬夏偏移验收已通过。
公网 DNS/ICE 服务代码与配置入口已加入，部署和公网浏览器实测仍是独立门槛。

P3：如需内核级字体、GPU、Audio、网络栈一致性，评估受控 Chromium 内核路线及
安全更新成本。Canvas 输出扰动可以继续改进，但不能替代这项工作。

### 受控内核接入还缺什么

目前 `BrowserBuild` 的摘要、发行者与信任状态解决运行时准入；
`FingerprintSupport::Supported` 只表示局部探针通过。`session_control::configure`
使用现有 Emulation 命令和页面/Worker 脚本，尚无本项目的 Chromium 原生实现。
下一阶段应沿现有边界接入，不能把这个枚举直接解释成原生指纹能力：

| 接入点 | 必须补齐的实现与证据 |
|---|---|
| 可信运行时登记 | 发布可复现的内核源码版本、补丁集和构建产物；继续校验摘要、发行者与受信任目录，配套安全更新和回退验证 |
| 能力探针 | 单独协商原生策略协议及版本、逐项返回实测能力；脚本读取覆盖与原生能力分别记录，不接受内核自己声明即通过 |
| 会话启动与目标创建 | 在网页、frame、Worker 首脚本之前从受保护的会话通道设置策略；原生实现不支持请求字段时明确拒绝，不能静默降级 |
| 字体 | 同时覆盖 CSS 字体匹配、文字度量与渲染、字体加载/查询等可观察路径；伪造字体列表不能证明渲染隔离 |
| GPU | 渲染、纹理/多采样 FBO、PBO、ImageBitmap 与导出使用一致策略；扩展、限制、shader 精度与设备声明一起验收 |
| 音频与网络 | 将离线读取、真实音频设备/采样率以及浏览器网络行为分别验收；页面返回值不能替代设备或网络证据 |
| 跨设备验收 | 同种子跨进程、profile 重开及不同主机核对；保留原生基线和不同种子对照，在真实 Win10/Win11 x64 上验证 |

上表是尚未实现的内核接入要求。本仓库当前没有对应 Chromium 补丁和原生产物，
本轮 WebGL 扩展不会改变这个状态，也不能将 `complete` 置为 true。

`private_webgl_pixel_acceptance` 在签名 Edge 上分别验证关闭扰动、启用扰动、相同种子的新进程、
不同种子四组真实浏览器，并覆盖九类页面/Worker 首脚本。页面 WebGL1 和 WebGL2 必须有
正向可用控制；其他上下文的原生可用性不能被覆盖层改变。比较 CPU 读取与 2D 读取的
像素、重复/局部读取和实际 PNG 解码结果。WebGL2 另验五组 PACK 布局、非零 view/dstOffset、
边界字节及既有错误；用 9x7 的 RGBA8 renderbuffer 与独立的 READ/DRAW 绑定、COLOR_ATTACHMENT1
核对坐标和绑定恢复，再用未覆盖的纹理 FBO 副本确认源缓冲区没有被改写。RGBA4 保持原生，
多采样和不完整 FBO 的失败不改调用方字节或新增错误。相同种子的检查使用新目录与新进程，
不替代持久化 profile 重开的验收。未通过对应提交的原生验收前不能
把本地 mock 成功换算成 GPU 覆盖已生效；`complete` 继续为 false。


## 签名运行时与实测工具补充

生产代码增加了对已安装 Microsoft Edge Stable 的发行者校验、目录准入和登记，
并补上能力探针的 Tauri 权限；Chrome 既有配置继续使用原运行时。新增
页面/iframe/Worker/请求头和重启稳定性的隔离观测工具。操作方法、自动验收及
仍未实现的能力见 [运行时验收流程](runtime-acceptance.md)。新路径是否可用，
以对应浏览器版本的真实探针结果为准，不能从签名或产物生成成功推断。
