param([Parameter(Mandatory)][string]$Installer, [Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:OS -ne 'Windows_NT') {
    throw 'Disposable hosted Windows fixture only; do not run this installer/uninstaller on a user PC'
}
. "$PSScriptRoot/windows-close-probe.ps1"
$installRoot = Join-Path $env:RUNNER_TEMP 'BrowsBetaInstall'
$appRoot = Join-Path $env:LOCALAPPDATA 'local-brows-desktop'
$sentinel = Join-Path $appRoot 'beta-upgrade-sentinel.txt'
$checks = [System.Collections.Generic.List[string]]::new()
$launches = [System.Collections.Generic.List[object]]::new()
$phase = 'install'
$result = [ordered]@{
    schemaVersion = 1
    assertionsPassed = $false
    complete = $false
    packageChecksComplete = $false
    checks = $checks
    launches = $launches
    installedApplicationSha256 = $null
    notChecked = @('interactive UI workflows', 'older-schema rollback', 'Authenticode signing', 'long-running multi-session load')
    failure = $null
}

function Install-Beta {
    $setup = Start-Process -FilePath $Installer -ArgumentList @('/S', "/D=$installRoot") -PassThru
    try {
        if (-not $setup.WaitForExit(120000)) { $setup.Kill(); throw 'Installer timed out' }
        if ($setup.ExitCode -ne 0) { throw "Installer exited $($setup.ExitCode)" }
    } finally {
        $setup.Dispose()
    }
}

function Get-ExecutableWriteProbe([string]$Path) {
    # Only opens the disposable fixture's executable; no bytes are written.
    $stream = $null
    try {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        return @{ canOpen = $true; nativeError = $null }
    } catch [IO.IOException], [UnauthorizedAccessException] {
        return @{ canOpen = $false; nativeError = $_.Exception.HResult -band 0xffff }
    } finally {
        if ($stream) { $stream.Dispose() }
    }
}

function Wait-ExecutableWriteAccess([string]$Path) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $checks = 0
    while ($true) {
        $checks++
        $probe = Get-ExecutableWriteProbe $Path
        if ($probe.canOpen) {
            return @{ canOpen = $true; waitMs = $clock.ElapsedMilliseconds; checks = $checks }
        }
        if ($probe.nativeError -notin @(32, 33)) {
            throw "Installed executable is not writable (native error $($probe.nativeError))"
        }
        if ($clock.ElapsedMilliseconds -ge 5000) {
            throw "Installed executable remained locked after normal exit (native error $($probe.nativeError))"
        }
        Start-Sleep -Milliseconds 50
    }
}

function Test-InstalledLaunch([string]$Executable, [int]$Round) {
    $launch = [ordered]@{
        round = $Round
        windowReadyMs = $null
        closeRequested = $false
        exitMs = $null
        exitCode = $null
        windowVisibleAtTimeout = $null
        lifecycle = @()
        webview2Version = $null
        processResourcesReleased = $false
        writeAccessBeforeDispose = $null
        writeAccessAfterDispose = $null
        writeAccessReady = $null
    }
    $launches.Add($launch)
    $process = $null
    $tracePath = Join-Path $env:RUNNER_TEMP "brows-package-launch-$Round.stderr.txt"
    $startup = [Diagnostics.Stopwatch]::StartNew()
    try {
        $process = Start-BrowsProbe -Executable $Executable -TracePath $tracePath
        Wait-BrowsWindow -Process $process
        $launch.windowReadyMs = $startup.ElapsedMilliseconds
        $webview = Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe' AND ParentProcessId=$($process.Id)" | Select-Object -First 1
        if (-not $webview -or -not $webview.ExecutablePath) { throw 'Actual application WebView2 process missing' }
        $launch.webview2Version = [Diagnostics.FileVersionInfo]::GetVersionInfo($webview.ExecutablePath).FileVersion
        if (-not (Test-Path (Join-Path $appRoot 'profiles.sqlite3'))) { throw 'Application did not initialize its database' }

        $shutdown = [Diagnostics.Stopwatch]::StartNew()
        Request-BrowsClose -Process $process
        $launch.closeRequested = $true
        if (-not $process.WaitForExit(30000)) {
            $process.Refresh()
            $launch.windowVisibleAtTimeout = $process.MainWindowHandle -ne [IntPtr]::Zero
            throw 'Packaged application did not exit within 30 seconds of its close request'
        }
        $launch.exitMs = $shutdown.ElapsedMilliseconds
        $launch.exitCode = $process.ExitCode
        if ($launch.exitCode -ne 0) { throw 'Packaged application reported a failed exit' }
        Write-Output "Launch ${Round}: window ready in $($launch.windowReadyMs) ms, normal exit in $($launch.exitMs) ms"
    } finally {
        $launch.lifecycle = @(Read-BrowsLifecycleTrace -TracePath $tracePath)
        if ($process) {
            try {
                if ($process.HasExited) {
                    $launch.exitCode = $process.ExitCode
                    $launch.writeAccessBeforeDispose = Get-ExecutableWriteProbe $Executable
                } else {
                    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                }
            } finally {
                $process.Dispose()
                $launch.processResourcesReleased = $true
            }
            $launch.writeAccessAfterDispose = Get-ExecutableWriteProbe $Executable
            # A signalled process is not proof that its image is replaceable.
            # Wait on the actual file-sharing condition, never retry an installer.
            $launch.writeAccessReady = Wait-ExecutableWriteAccess $Executable
        }
    }
}

try {
    Install-Beta
    $checks.Add('silent-install')
    $executable = Join-Path $installRoot 'brows-desktop.exe'
    if (-not (Test-Path $executable)) { throw 'Installed executable missing' }
    $result.installedApplicationSha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    $phase = 'initial-launch'
    Test-InstalledLaunch -Executable $executable -Round 1
    $checks.Add('packaged-window')
    $checks.Add('database-init')

    $phase = 'upgrade'
    [IO.File]::WriteAllText($sentinel, 'preserve-user-data')
    Install-Beta
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Upgrade removed application data' }
    if ((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash -ine $result.installedApplicationSha256) {
        throw 'Reinstalling the same package changed the installed executable hash'
    }
    $checks.Add('upgrade-preserves-data')
    $phase = 'post-upgrade-launch'
    foreach ($round in 2..3) {
        Test-InstalledLaunch -Executable $executable -Round $round
    }
    $checks.Add('normal-exit')

    $phase = 'uninstall'
    $uninstall = Join-Path $installRoot 'uninstall.exe'
    if (-not (Test-Path $uninstall)) { throw 'Uninstaller missing' }
    $removal = Start-Process -FilePath $uninstall -ArgumentList '/S' -PassThru
    try {
        if (-not $removal.WaitForExit(60000)) { $removal.Kill(); throw 'Uninstaller timed out' }
        if ($removal.ExitCode -ne 0) { throw 'Uninstaller failed' }
    } finally {
        $removal.Dispose()
    }
    for ($attempt = 0; $attempt -lt 120 -and (Test-Path $executable); $attempt++) { Start-Sleep -Milliseconds 250 }
    if (Test-Path $executable) { throw 'Uninstaller left the installed executable' }
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Uninstall removed application data' }
    $checks.Add('uninstall-preserves-data')
    $result.assertionsPassed = $true
    $result.packageChecksComplete = $true
} catch {
    $result.failure = @{ phase = $phase; message = $_.Exception.Message }
    throw
} finally {
    $json = $result | ConvertTo-Json -Depth 5
    $json | Set-Content -LiteralPath $Report -Encoding utf8
    Write-Output $json
}
