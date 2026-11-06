param([Parameter(Mandatory)][string]$Installer, [Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
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
    checks = $checks
    launches = $launches
    notChecked = @('interactive UI workflows', 'older-schema rollback', 'Authenticode signing', 'long-running multi-session load')
    failure = $null
}

function Install-Beta {
    $setup = Start-Process -FilePath $Installer -ArgumentList @('/S', "/D=$installRoot") -PassThru
    if (-not $setup.WaitForExit(120000)) { $setup.Kill(); throw 'Installer timed out' }
    if ($setup.ExitCode -ne 0) { throw "Installer exited $($setup.ExitCode)" }
}

function Test-InstalledLaunch([string]$Executable, [int]$Round) {
    $launch = [ordered]@{
        round = $Round
        windowReadyMs = $null
        closeRequested = $false
        exitMs = $null
        exitCode = $null
        windowVisibleAtTimeout = $null
    }
    $launches.Add($launch)
    $process = $null
    $startup = [Diagnostics.Stopwatch]::StartNew()
    try {
        $process = Start-Process -FilePath $Executable -PassThru
        for ($attempt = 0; $attempt -lt 120; $attempt++) {
            $process.Refresh()
            if ($process.HasExited) {
                $launch.exitCode = $process.ExitCode
                throw 'Packaged application exited before opening its window'
            }
            if ($process.MainWindowTitle -eq 'Brows') {
                $launch.windowReadyMs = $startup.ElapsedMilliseconds
                break
            }
            Start-Sleep -Milliseconds 250
        }
        if ($null -eq $launch.windowReadyMs) { throw 'Packaged application did not open its main window' }
        if (-not (Test-Path (Join-Path $appRoot 'profiles.sqlite3'))) { throw 'Application did not initialize its database' }

        $shutdown = [Diagnostics.Stopwatch]::StartNew()
        $launch.closeRequested = $process.CloseMainWindow()
        if (-not $launch.closeRequested) { throw 'Unable to send close request to the packaged application' }
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
        if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    }
}

try {
    Install-Beta
    $checks.Add('silent-install')
    $executable = Join-Path $installRoot 'brows-desktop.exe'
    if (-not (Test-Path $executable)) { throw 'Installed executable missing' }
    $phase = 'initial-launch'
    Test-InstalledLaunch -Executable $executable -Round 1
    $checks.Add('packaged-window')
    $checks.Add('database-init')

    $phase = 'upgrade'
    [IO.File]::WriteAllText($sentinel, 'preserve-user-data')
    Install-Beta
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Upgrade removed application data' }
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
    if (-not $removal.WaitForExit(60000)) { $removal.Kill(); throw 'Uninstaller timed out' }
    if ($removal.ExitCode -ne 0) { throw 'Uninstaller failed' }
    for ($attempt = 0; $attempt -lt 120 -and (Test-Path $executable); $attempt++) { Start-Sleep -Milliseconds 250 }
    if (Test-Path $executable) { throw 'Uninstaller left the installed executable' }
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Uninstall removed application data' }
    $checks.Add('uninstall-preserves-data')
    $result.assertionsPassed = $true
} catch {
    $result.failure = @{ phase = $phase; message = $_.Exception.Message }
    throw
} finally {
    $json = $result | ConvertTo-Json -Depth 5
    $json | Set-Content -LiteralPath $Report -Encoding utf8
    Write-Output $json
}
