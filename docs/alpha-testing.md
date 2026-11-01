# Brows Alpha 测试说明

这是 Windows x64 的早期内测版，主要验证环境管理、独立多开、恢复和当前指纹覆盖范围。完整设备指纹独立尚未验收。无需等全部开发完成即可收集本轮反馈。

## 下载与运行

1. 从仓库 Actions → **Windows Alpha test build** 的成功 `package` job 下载 `brows-0.1.0-alpha.*-windows-x64-*` artifact，解压后运行 `Brows-Alpha.exe`。
2. 需要 Windows 11 x64、Microsoft Edge WebView2 Runtime 和微软 Visual C++ 2015–2022 x64 运行库。若提示缺少 `VCRUNTIME140*.dll`，使用[微软官方运行库安装程序](https://aka.ms/vs/17/release/vc_redist.x64.exe)。测试者不需要 Node、Rust 或开发工具。程序内已包含管理界面，不需要启动开发服务器。
3. 窗口标题为 **Brows Alpha**，数据使用独立的 `%LOCALAPPDATA%\local-brows-desktop-alpha`。它不是把所有数据写在 exe 旁边的便携模式；解压位置变化不会重置测试数据。不要把已有正式目录复制进来，先用全新测试环境。
4. 本轮优先使用已安装且签名验证通过的 Google Chrome 测试系统原生多开；未安装时先安装官方 Chrome，再启动 Alpha。2026-11-01 实测 CFT `156.0.8078.80` 主程序没有签名，被安装入口拒绝；这不是已通过的指纹内核。此版本不要依赖 CFT 安装入口完成首次浏览器配置。
5. 包含 `build-info.json`、`SHA256SUMS.txt` 和这份说明。用以下命令查看 exe 摘要，并与两个文件中的值比较：

```powershell
Get-FileHash .\Brows-Alpha.exe -Algorithm SHA256
```

测试程序尚未代码签名。若 Windows 拦截启动，请记录提示供核查，不要关闭系统安全保护。

## 建议按顺序测试

| 步骤 | 操作 | 预期结果 |
|---|---|---|
| 1 | 新建 A/B 两个系统原生环境，分别打开 | 独立窗口；同一环境重复启动被拒绝 |
| 2 | 在同一测试网站分别设置不同状态，关闭后重开 | 每个环境保留各自状态，不串用 |
| 3 | 在设置页调低并发上限，继续启动其他测试环境 | 超额排队；关闭一个窗口后 FIFO 补位 |
| 4 | 使用十个空白环境进行并发测试，再启动第十一个 | 十个活动，第十一个排队；记录机器内存和资源占用 |
| 5 | 关闭管理器，再重新打开 | 自有浏览器收尾，环境列表保留，可以重新启动 |
| 6 | 用一个空白测试环境导出、恢复，再测试错误口令和处理中取消 | 成功恢复可用；失败不覆盖原环境；取消等待清理完成 |
| 7 | 配置测试用 HTTP/SOCKS5 代理，测试正确和错误凭据 | 正确时通过连通性检测；失败明确报错，不静默直连 |
| 8 | 设置页检测运行时指纹支持，通过后执行“双临时环境指纹实测” | 展示真实读数及覆盖边界；不通过时不能启动 SeedableNoise |
| 9 | 对通过检测的运行时创建 SeedableNoise 环境并克隆 | 副本使用新 seed，原环境配置保持；记录两者实际页面读数 |

强制结束管理器、浏览器或恢复任务的测试，只使用可丢弃的测试环境。当前实测报告只保留在设置页，离开页面前复制完整 JSON。报告不会读取已有登录环境；反馈无需包含 Cookie、密码、代理凭据或备份口令。

“覆盖项通过”只针对当前 headless 样本；Worker、UA-CH、时区、字体、实际 GPU 和网络栈仍有未覆盖维度。系统原生环境的存储隔离与指纹加载检测是不同验收项。

## 反馈格式

- `build-info.json` 中的版本与 commit：
- Windows 版本、CPU、内存：
- 系统 Chrome / CFT 版本：
- 测试步骤、预期、实际结果：
- 错误文本、窗口/进程是否残留：
- 指纹测试请附完整观测 JSON；移除任何额外添加的私人信息：

## 维护者构建入口

Windows 构建机需要 Git、Node 24+、Rust 1.97.1 MSVC、clippy/rustfmt、Visual Studio C++ 工具与 Windows SDK。从已提交且工作区干净的源码执行：

```powershell
powershell -NoProfile -File .\tools\build-alpha.ps1
```

脚本安装锁定的前端依赖、构建界面、执行自动回归与严格检查，再生成 release exe 和 ZIP；输出在 `target/test-builds`。它临时设置 Alpha 配置和独立构建目录，退出时恢复原环境变量。失败不会发布为成功包，不会写入正式数据目录。也可执行 `npm run desktop:alpha`。

GitHub Actions 可手动运行 **Windows Alpha test build**，默认勾选官方运行时验收，也可取消勾选只构建测试包。推送 `alpha/**` 分支会构建测试包，并在独立 job 下载官方 CFT、验证签名和执行 headless 双样本验收。新 workflow 尚未在默认分支注册时，推送包含该文件的 `alpha/**` 分支可启动首次构建。

`package` job 成功后，独立的 `startup` job 在全新 Windows runner 下载同一个测试包，检查主窗口标题和正常关闭，并保存 `startup.json`；它不代表页面内容、多开或登录态测试已经通过。`package` job 与真实指纹 `fingerprint` job 独立。下载包成功只证明构建及其自动回归通过；需要查看另一 job 的结果和已生成的 JSON，不能把打包成功当成指纹已通过。`runtime-acceptance.json` 记录安装/基础探针结果；签名等准入失败时仍有负向报告，`fingerprint-measurement.json` 只在真正完成采集后生成，不能把准入报告当成页面实测。artifact 保留 14 天，下载需要仓库访问权限。

独立的 `native-browser` job 执行 `system_native_windows_acceptance`：通过生产服务打开已签名的系统 Chrome，让 A/B 环境在同一 localhost 网站分别写入 Cookie、LocalStorage 和 IndexedDB，验证重复启动拦截、并发上限和第三个环境排队补位，再关闭管理服务、重启后读取原有状态。结果保存在 `windows-native-browser-acceptance` artifact；失败也保留已完成步骤和真实页面读数。测试只创建临时环境，不读取已有登录数据。它未覆盖前端点击、十环境压力或指纹独立，相关结果不能从存储隔离测试推断。

同一 job 还对系统 Chrome 执行独立的 headless 指纹能力探针，保存 `windows-system-chrome-fingerprint-probe` 报告。此步骤验证探针执行和状态记录；`fingerprintSupported: false` 仍表示该浏览器不支持当前指纹入口，步骤本身成功不代表指纹验收通过。双环境完整观测需先通过能力探针。

运行状态以对应 commit 的 Actions 记录和 `build-info.json` 为准。首次 Windows 构建耗时取决于依赖下载、编译与 Actions 排队；说明文件本身不证明构建或真机验收已经执行。

已知限制：当前官方 CFT `156.0.8078.80` 缺少 Authenticode 签名，Windows 验收拒绝它，未执行双样本采集。系统 Chrome 的历史扩展探针也曾返回 Unsupported，因此本轮 Alpha 可测试原生多开，但不能宣称指纹独立已经可用。
