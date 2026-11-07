# Windows 带认证代理验收

此入口使用 Windows runner 对指定的 HTTP CONNECT / SOCKS5 代理实测。PR 只运行
无真实凭据的驱动测试；访问外部代理的工作流必须在 master 手动运行。

## 一次性配置

在仓库 **Settings → Secrets and variables → Actions → New repository secret**
分别保存 `BROWS_PROXY_ACCEPTANCE_USERNAME` 和 `BROWS_PROXY_ACCEPTANCE_PASSWORD`。
两项值只填写代理账号、代理密码，不包含 URL；不要填写到代码、Issue 或工作流输入。
同次运行的多个地址使用这组凭据，凭据不同的代理分开运行。

打开 **Actions → Authenticated proxy acceptance → Run workflow**，选择 master：

- endpoints：填写 `IPv4:端口`，多个地址用英文逗号分隔，最多四个；不填账号密码。
- protocols：选择两种都验证，或只验证服务商明确支持的 HTTP / SOCKS5。

下载该次运行的 `authenticated-proxy-acceptance-*` 报告。`proxy-1`、`proxy-2`
按输入地址的顺序对应；报告不包含账号密码、原始 stderr 或 curl 配置。

## 判定方式

1. 限时连接代理 TCP 端口；失败时，认证和 TLS 保持未检查，不能据此认定密码错误。
2. curl 通过指定代理访问唯一端点 `https://checkip.amazonaws.com/`，保持系统证书校验。
   HTTP CONNECT 407 / 明确 SOCKS5 认证拒绝才归类为认证拒绝。一般连接失败和超时
   分别记录，不猜测账号状态。
3. 使用 Brows 的生产 `TcpProxyConnectivityTester` 和同一份临时凭据独立验证。
   TLS、HTTP 状态或 IP 解码失败都不回退直连。错误含义较宽时保留宽分类。
4. 所有请求的地址/协议必须同时通过 curl 和 Brows 才算本次通过。单个子进程最多
   20 秒，curl 内部最多 15 秒；缺少凭据、无法执行或任一失败都不能标为通过。

账号密码经环境变量进入测试驱动；curl 通过 stdin 接收配置，Brows 通过专用子进程
环境变量接收。凭据不进入命令行、临时文件、报告或存储的应用保险库。报告只记录
别名、协议、阶段、状态码和查询到的出口 IP。出口 IP 相等与否单独记录，不能仅因
轮换代理的两次出口不同就断言协议失败。runner 的默认 HTTPS 查询是辅助基线。

PR 的驱动测试通过不代表真实代理通过。此入口也不替代浏览器会话的出口、DNS、ICE、
Windows 10/11 真机、长稳或登录态验收。后续只有取得有效的外部代理结果，才继续
验证绑定代理的真实浏览器会话及对应网络观测。
