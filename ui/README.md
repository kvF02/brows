# Brows UI

`ui/` 是 Brows 本地优先多 Profile 管理器的 React/TypeScript 壳层。它只依赖一个窄的、版本化的 `ApiClient`，不直接读取文件、启动进程、访问 SQLite 或发起任意网络请求。

## 开发

需要 Node.js 18+：

```powershell
npm install
npm run dev
```

浏览器预览和测试场景会自动使用内存 `MockApiClient`。检测到 Tauri 运行时后，界面改用 `@tauri-apps/api/core`，通过固定 handler 白名单访问真实 application service；模拟数据不会混入桌面运行态。

```powershell
npm run typecheck
npm run build
npm run preview
```

若交付包未包含 `node_modules`，网络不可用时以上命令需要由构建机预先缓存 npm 依赖。UI 源码本身不依赖图标、CSS 框架或远程字体。

## Tauri 接入

真实桥接位于 `src/api/tauri.ts`。它只声明以下固定 handler：

```text
profile_list / profile_get / profile_create / profile_update
profile_clone / profile_archive / profile_restore
session_list / session_launch / session_stop / session_cancel_queued
```

浏览器开发态仍可显式注入 `window.__BROWS_BRIDGE__` 进行契约测试，但业务组件无法直接访问它。没有通用 `execute`、`readFile`、`writeFile` 或 `request` 方法；不要为了省事扩展成任意 JSON/命令通道。

Rust 端应验证：

- 命令名和参数 schema（包括 Profile revision 乐观锁）；
- Profile UUID、运行时 ID 和代理 ID 是否属于当前用户；
- 所有文件路径由核心生成，不接受前端路径；
- 返回 DTO 与 `brows-contracts` 的 camelCase wire format 保持版本兼容，敏感字段不返回到 UI。

## CSP 与网络边界

`index.html` 自带严格 CSP：默认只允许同源资源，禁止 `object`、远程 frame、`eval` 和表单外发。为了 Vite 热更新，开发元标签暂时允许 `127.0.0.1:4173` 的 HTTP/WebSocket；打包进 Tauri 时必须替换为：

```text
default-src 'self';
script-src 'self';
style-src 'self';
img-src 'self' data:;
font-src 'self';
connect-src 'self' ipc: http://ipc.localhost;
object-src 'none';
base-uri 'none';
frame-ancestors 'none';
form-action 'self'
```

Tauri 窗口应只加载打包资源，拒绝远程导航；正式配置不安装 Shell、任意文件或通用 HTTP 权限。主动出口/DNS/WebRTC 诊断由 Rust `DiagnosticRunner` 在用户点击后执行，并将实际端点回显给用户。

## 数据与指纹说明

UI 将属性分为三类并始终展示标签：

- **独立隔离**：Cookie、LocalStorage、IndexedDB、缓存、Service Worker 和登录状态。
- **可配置**：代理、语言、启动页和权限策略（受 Chromium 能力限制）。
- **主机共享**：首版真实 Windows 的 Canvas、WebGL、GPU、字体、Audio、CPU 和 TLS 特征。

因此“可以多开”表示多个环境可同时拥有独立浏览器状态，不代表每个环境已经成为一台完整的虚拟设备。后续差异指纹能力必须通过新的 `FingerprintRuntime`/Persona 适配器接入，不在 UI 内注入临时脚本。
