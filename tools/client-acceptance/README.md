# Windows 10/11 本机与网站验收

这是对应提交的未签名 x64 内测包。Windows 11 ARM64 可用 x64 模拟运行，须单独记录；
不代表已验证原生 ARM64 版本。Windows Server 报告不能代替 Windows 10/11 客户端报告。

1. 从同一次 Actions 下载 `brows-windows-client-kit-<commit>`，完整解压 ZIP。
   在专用 Windows 测试用户下手动安装 `*-setup.exe`；不要覆盖重要的生产环境。
   核对 Actions 提交号与 `beta-build.json`。包内 SHA256 用于检测下载混用或损坏，
   不替代独立可信渠道或产品代码签名。安装器可能联网安装 WebView2。
2. 选择你有权测试、允许跨重启保持登录的 HTTPS 站点和测试账号。启用站点的“保持登录”
   （如有）。准备可见的无敏感测试标记；不要用付款、发帖等操作做标记。
   密码、MFA、代理凭据只在对应站点或 Brows 界面输入，不发到聊天里。
3. 打开普通 Windows PowerShell 5.1 或 PowerShell 7，切换到解压目录，执行：

   ```powershell
   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 -Application 'C:\实际安装目录\brows-desktop.exe' -SiteAlias test-site -Hardware physical
   ```

   `-Application` 指向安装器落盘的程序，不能指向 ZIP 内的便携程序。实际安装位置可从
   快捷方式属性找到；`-Hardware` 只能填 physical、virtual 或 unknown，是你声明的条件，
   脚本不会自动把虚拟机算成真机。`-SiteAlias` 只填字母数字或短横线的代号，不填网址。
   执行策略参数仅作用于这次 PowerShell 进程，不修改系统设置。
4. 按脚本英文步骤操作界面，完成一项输入 y；失败输入 n；没有测输入 s。
   步骤依次是：Native 登录、独立环境隔离、正常停止重开、应用退出重开、备份恢复新环境、
   错误备份密码拒绝；然后 Seeded 内核探针、登录与隔离、停止重开、应用重启后重新探针、
   备份恢复新环境。恢复必须使用同一个 Windows 用户及受支持的同一浏览器运行时。
5. 把生成的 `client-acceptance.json` 发回。报告只含构建号、Windows 构建及架构、步骤结果，
   不含账号名、机器名、完整路径、网址、Cookie、密钥或凭据。脚本不会自动启动、终止、
   安装、卸载应用，不读用户数据目录，不创建信任证书，不连接测试网站。

每一步都会保存进度；中途退出或选 s 的报告是 incomplete。任一步失败即 failed。
所有步骤通过才是 operator-accepted，表示人工验收，不伪装成自动化验证。
重新运行会覆盖指定报告；保留旧报告时先另存或用 `-Report` 指定新文件名。

网站如果主动过期会话、撤销设备或只发会话 Cookie，关浏览器后需要重登可能是其自身策略。
不要为了通过验收修改 Cookie、复制密钥、关闭证书校验或绕过网站验证。先换允许保持登录
的自有测试站点，再分别定位产品与站点策略。跨 Windows 用户/机器恢复、真实断电、
TURN 及具体业务站点的风控兼容性不属于此报告的自动保证。
