# 公网 DNS / ICE 关联检测

`tools/network-probe-service.mjs` 是可选的自建 IPv4 检测服务。应用没有内置第三方收集端点；
未配置时，公网 DNS 与 WebRTC 检查保持 `not_checked`。HTTP 出口检查不能代替这两项。

## 部署条件

需要一台有公网 IPv4 的服务器、一个可委派的域名和有效 TLS 证书。例如将
`probe.example.com` 的 NS 委派给 `ns.probe.example.com`，为 NS 配置指向服务器的
IPv4 glue 记录。TLS 证书应同时覆盖 `probe.example.com` 和 `*.probe.example.com`。
这些是示例域名，使用时替换成自己的域名。

开放 UDP 53、TCP 443、UDP 40000–40127；DNS 服务需要独占对应监听端口。
运行时需要 Node.js 24，TLS 私钥与证书由部署者提供：

```sh
PROBE_ZONE=probe.example.com PUBLIC_IPV4=203.0.113.10 \
TLS_KEY=/run/secrets/probe.key TLS_CERT=/run/secrets/probe.crt \
DNS_ALLOWED_CIDRS=192.0.2.0/24 EGRESS_ALLOWED_CIDRS=198.51.100.0/24 \
node tools/network-probe-service.mjs
```

IP 与 CIDR 也均为文档示例。`DNS_ALLOWED_CIDRS` 应来自预期递归解析器的已知出口范围，
`EGRESS_ALLOWED_CIDRS` 应来自被测代理的已知出口范围，逗号分隔。不能直接将本次观测到的
地址加入白名单再宣称通过。缺少任一范围时，对应项目保留 `not_checked`。
服务只支持 IPv4；不能据此证明 IPv6、DoH 服务内部链路或所有 DNS 查询均无泄漏。

## 应用配置与执行

在应用数据目录放置 `network-probe.json`，重启应用：

```json
{
  "origin": "https://probe.example.com",
  "zone": "probe.example.com",
  "stunMin": 40000,
  "stunMax": 40127
}
```

前端只能读取这份核心配置，不能指定其他 URL、端口或脚本。启动 SeedableNoise 环境后，
在“指纹与泄漏诊断”选择“公网 DNS / ICE 检查”，确认显示的 HTTPS、随机子域和 UDP
端点。核心创建独立测试标签，执行固定探测并关闭标签，不读取原有页面、Cookie 或登录数据。

服务为每次请求分配随机子域与独立 STUN 端口，关联递归 DNS 请求、HTTPS 请求及浏览器
ICE 的 srflx 候选。所有观测地址均在配置范围内才可能返回 `pass`；STUN 还必须与浏览器
候选地址相符。发现范围外的地址返回 `fail`。没有包、没有候选、样本溢出、超时或规则
缺失均不能推断无泄漏。每个结果只代表这一次 IPv4 challenge 的观测。

服务不写分析日志，挑战在结果取回或 60 秒后清理；最多同时 64 个挑战，每类地址最多
16 个。部署时仍需按自己的环境配置 TLS 更新、进程管理及网络访问策略。

## 验证状态

`npm test` 包含真实 loopback UDP/HTTP 协议回归，以及 DNS 报文、STUN XOR 地址与
FINGERPRINT 校验、CIDR 判定和缺失证据规则。这些测试不代表公网浏览器泄漏验收。
公网验收需要部署后的域名、证书和预期出口范围；未取得这些证据前保持未验收。
