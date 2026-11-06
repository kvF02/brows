param([Parameter(Mandatory)][string]$Application, [Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $IsWindows) { throw 'Windows GitHub Actions fixture only' }
. "$PSScriptRoot/windows-close-probe.ps1"
$app = (Resolve-Path -LiteralPath $Application).Path
$releaseRoot = [IO.Path]::GetFullPath((Join-Path $env:GITHUB_WORKSPACE 'target/release'))
if ([IO.Path]::GetDirectoryName($app) -ine $releaseRoot -or [IO.Path]::GetFileName($app) -ine 'brows-desktop.exe') { throw 'Application must be the CI release build' }
$probe = $null
$probeTrace = Join-Path $env:RUNNER_TEMP 'brows-ui-runtime-discovery.stderr.txt'
$stage = 'runtime-discovery'
function Read-Version([string]$File) {
    $v = [Diagnostics.FileVersionInfo]::GetVersionInfo($File)
    return "$($v.FileMajorPart).$($v.FileMinorPart).$($v.FileBuildPart).$($v.FilePrivatePart)"
}
function Assert-Microsoft([string]$File) {
    $signature = Get-AuthenticodeSignature -LiteralPath $File
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -ne 'Microsoft Corporation') { throw 'Microsoft executable signature admission failed' }
}
try {
    $probe = Start-BrowsProbe -Executable $app -TracePath $probeTrace
    $webview = $null
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        $probe.Refresh()
        if ($probe.HasExited) { throw 'Application exited during WebView2 discovery' }
        $webview = Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe' AND ParentProcessId=$($probe.Id)" | Select-Object -First 1
        if ($webview -and $webview.ExecutablePath) { break }
        Start-Sleep -Milliseconds 250
    }
    if (-not $webview -or -not $webview.ExecutablePath) { throw 'Actual application WebView2 runtime not found' }
    Assert-Microsoft $webview.ExecutablePath
    $version = Read-Version $webview.ExecutablePath
    if ($version -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw 'Invalid WebView2 file version' }
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        $probe.Refresh()
        if ($probe.MainWindowTitle -eq 'Brows') { break }
        Start-Sleep -Milliseconds 250
    }
    $stage = 'runtime-discovery-close'
    Request-BrowsClose -Process $probe
    if (-not $probe.WaitForExit(30000)) {
        $probe.Refresh()
        throw "Application did not exit after runtime discovery (visible window: $($probe.MainWindowHandle -ne 0))"
    }
    if ($probe.ExitCode -ne 0) { throw "Application exited after runtime discovery with code $($probe.ExitCode)" }
    $stage = 'driver-setup'
    $driver = if ($env:EDGEWEBDRIVER) { Join-Path $env:EDGEWEBDRIVER 'msedgedriver.exe' } else { Join-Path $env:RUNNER_TEMP 'brows-ui-webdriver/msedgedriver.exe' }
    $branch = ($version.Split('.')[0..2] -join '.')
    $matches = (Test-Path -LiteralPath $driver) -and ((Read-Version $driver).StartsWith("$branch."))
    if (-not $matches) {
        $downloadRoot = Join-Path $env:RUNNER_TEMP 'brows-ui-webdriver'
        New-Item -ItemType Directory -Path $downloadRoot -Force | Out-Null
        $archivePath = Join-Path $downloadRoot 'driver.zip'
        Invoke-WebRequest -Uri "https://msedgedriver.microsoft.com/$version/edgedriver_win64.zip" -OutFile $archivePath -TimeoutSec 60
        $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            $entry = @($archive.Entries | Where-Object { $_.FullName -eq 'msedgedriver.exe' })
            if ($entry.Count -ne 1 -or $entry[0].Length -lt 1 -or $entry[0].Length -gt 100MB) { throw 'Unexpected driver archive' }
            $driver = Join-Path $downloadRoot 'msedgedriver.exe'
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry[0], $driver, $true)
        } finally { $archive.Dispose() }
    }
    Assert-Microsoft $driver
    if (-not (Read-Version $driver).StartsWith("$branch.")) { throw 'WebView2 and WebDriver build mismatch' }
    $env:BROWS_UI_WEBVIEW_VERSION = $version
    Write-Output "Testing packaged UI with WebView2 $version and driver $(Read-Version $driver)"
    $stage = 'webdriver'
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        # Modern WebView2 ignores WebDriver environment overrides in elevated
        # hosts. Launch this CI fixture with a Basic User token; product code
        # and its browser arguments remain unchanged.
        $done = Join-Path $env:RUNNER_TEMP 'brows-ui-result.txt'
        $output = Join-Path $env:RUNNER_TEMP 'brows-ui-output.txt'
        Remove-Item -LiteralPath $done -Force -ErrorAction SilentlyContinue
        function Quote-Literal([string]$Text) { return "'" + $Text.Replace("'", "''") + "'" }
        $node = (Get-Command node.exe).Source
        $script = Join-Path $env:GITHUB_WORKSPACE 'tools/packaged-ui-acceptance.mjs'
        $reportPath = [IO.Path]::GetFullPath($Report)
        $invoke = "& $(Quote-Literal $node) $(Quote-Literal $script) $(Quote-Literal $app) $(Quote-Literal $driver) $(Quote-Literal $reportPath) *> $(Quote-Literal $output)"
        $child = @"
`$ErrorActionPreference = 'Stop'
`$code = 1
try {
  `$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
  if (`$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'UI fixture must run without administrator membership' }
  `$ErrorActionPreference = 'Continue'
  $invoke
  `$code = `$LASTEXITCODE
} catch { `$_ | Out-File -LiteralPath $(Quote-Literal $output) -Append }
finally { [IO.File]::WriteAllText($(Quote-Literal $done), [string]`$code) }
"@
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($child))
        $hostExe = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
        $runas = Join-Path $env:SystemRoot 'System32/runas.exe'
        $runasArgs = @('/trustlevel:0x20000', "$hostExe -NoProfile -EncodedCommand $encoded")
        if ([Environment]::OSVersion.Version.Build -ge 22621) { $runasArgs = @('/machine:amd64') + $runasArgs }
        & $runas @runasArgs
        if ($LASTEXITCODE -ne 0) { throw 'Unable to launch the non-elevated UI fixture' }
        $deadline = [DateTime]::UtcNow.AddMinutes(4)
        while (-not (Test-Path -LiteralPath $done) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 250 }
        if (Test-Path -LiteralPath $output) { Get-Content -LiteralPath $output -Tail 35 | Write-Output }
        if (-not (Test-Path -LiteralPath $done) -or [IO.File]::ReadAllText($done).Trim() -ne '0') {
            if (Test-Path -LiteralPath $reportPath) { Get-Content -LiteralPath $reportPath | Write-Output }
            throw 'Non-elevated packaged UI acceptance failed'
        }
    } else {
        node tools/packaged-ui-acceptance.mjs $app $driver $Report
        if ($LASTEXITCODE -ne 0) { throw 'Packaged UI acceptance failed' }
    }
} catch {
    if (-not (Test-Path -LiteralPath $Report)) {
        $failure = @{
            schemaVersion = 1; assertionsPassed = $false; complete = $false
            checks = @()
            failure = @{ stage = $stage; message = $_.Exception.Message }
            lifecycle = @(Read-BrowsLifecycleTrace -TracePath $probeTrace)
        } | ConvertTo-Json -Depth 5
        $failure | Set-Content -LiteralPath $Report -Encoding utf8
        Write-Output $failure
    }
    throw
} finally {
    if ($probe -and -not $probe.HasExited) { Stop-Process -Id $probe.Id -Force -ErrorAction SilentlyContinue }
    # Only this CI build is owned. Never stop browsers by process image name.
    Get-CimInstance Win32_Process -Filter "Name='brows-desktop.exe'" | Where-Object { $_.ExecutablePath -ieq $app } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}
