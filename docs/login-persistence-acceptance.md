# 登录态保持与恢复验收

`Windows runtime acceptance` 的 `HTTPS login storage and backup recovery acceptance`
使用真实签名 Edge、生产应用组合及生产备份命令。只在 GitHub 托管 Windows 夹具中运行，
生成短期 localhost TLS 证书并临时信任测试 CA，结束后删除 CA 与测试私钥；没有忽略证书错误的启动参数。

同一 HTTPS Origin 的一次性站点为账号 a/b 签发随机持久 Cookie：`Secure`、`HttpOnly`、
`SameSite=Strict`、`Max-Age=86400` 和 `__Host-` 前缀。服务端通过请求 Cookie 判断实际认证账号；
页面报告 LocalStorage、IndexedDB 与 Cache Storage 的独立账号标记，同时确认 JavaScript 看不到该认证 Cookie。
只读检查页不设置 Cookie、不重新登录、不补写缺失账号标记。报告保存标签、布尔值与阶段，不保存 Cookie token 或真实凭据。

系统原生与 SeedableNoise 分别覆盖：

- 新环境初始为空；同源账号 a 存在时，环境 b 仍为空，之后双方各保持自己的状态。
- 正常停止并重开后，服务端认证与三种站点存储一致。
- 对精确识别的夹具浏览器进程注入强杀，观察失败状态，重开后读取已持久化登录态。
- 停止后通过生产命令加密导出、拒绝错误恢复口令、恢复成新 UUID，并在新目录真实启动读取。
- 再次核对源环境、独立环境和恢复环境；重建应用状态后读取持久化配置及站点状态。
- 正常停止应触发文档关闭；该证据与重开后的登录态读取分开断言。

Windows 正常停止先向该环境拥有的可见窗口异步请求关闭，最多等待 5 秒。
窗口无响应、关闭被阻止或没有窗口时，仍强制终止所拥有的 Job 并等待回收；失败会保留会话占用，允许重试。
宿主退出/崩溃的 Job 回收及策略异常的失败关闭仍使用强制终止，不等待页面。
正常关闭有助于浏览器提交最近写入；超时强杀不能保证尚未持久化的最新 Cookie 或事务。

证据保存在 `target/login-persistence-acceptance.json`。本地协议测试只验证夹具和证据来源，
不能替代 Windows 实际浏览器验收。本阶段也不能代替以下项目：

- 外部真实站点的登录、MFA、风控、会话有效期及服务器撤销策略。
- 跨机器、跨 Windows 用户或不同加密身份下的 Cookie 解密；没有绕过 DPAPI / App-Bound Encryption。
- Windows 10/11 客户端真机、物理断电及磁盘故障。

外部站点后续应使用专用测试账号，对同设备重启、备份恢复和退出登录分别验收。只记录成功/失败及条件，不提交账号、Cookie 或口令。
