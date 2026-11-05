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
这不等于环境编辑、代理、备份等完整 UI 流程已经通过自动点击验收。
只有对应提交的工作流成功且 `beta-package-acceptance.json` 存在，才算本次安装验收通过。

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
- 真实十开/长稳、完整桌面 UI 及故障恢复验收。
- 公网 DNS/ICE 服务部署和互联网环境实测。
