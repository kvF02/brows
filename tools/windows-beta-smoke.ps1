param([Parameter(Mandatory)][string]$Installer, [Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
$installRoot = Join-Path $env:RUNNER_TEMP 'BrowsBetaInstall'
$appRoot = Join-Path $env:LOCALAPPDATA 'local-brows-desktop'
$sentinel = Join-Path $appRoot 'beta-upgrade-sentinel.txt'
$process = $null
function Install-Beta {
    $setup = Start-Process -FilePath $Installer -ArgumentList @('/S', "/D=$installRoot") -PassThru
    if (-not $setup.WaitForExit(120000)) { $setup.Kill(); throw 'Installer timed out' }
    if ($setup.ExitCode -ne 0) { throw "Installer exited $($setup.ExitCode)" }
}
try {
    Install-Beta
    $executable = Join-Path $installRoot 'brows-desktop.exe'
    if (-not (Test-Path $executable)) { throw 'Installed executable missing' }
    $process = Start-Process -FilePath $executable -PassThru
    $ready = $false
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) { throw 'Packaged application exited before opening its window' }
        if ($process.MainWindowTitle -eq 'Brows') { $ready = $true; break }
    }
    if (-not $ready) { throw 'Packaged application did not open its main window' }
    if (-not (Test-Path (Join-Path $appRoot 'profiles.sqlite3'))) { throw 'Application did not initialize its database' }
    [IO.File]::WriteAllText($sentinel, 'preserve-user-data')
    if (-not $process.CloseMainWindow()) { throw 'Unable to close the application normally' }
    if (-not $process.WaitForExit(30000)) { throw 'Packaged application did not exit normally' }
    if ($process.ExitCode -ne 0) { throw 'Packaged application reported a failed exit' }
    Install-Beta
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Upgrade removed application data' }
    $uninstall = Join-Path $installRoot 'uninstall.exe'
    if (-not (Test-Path $uninstall)) { throw 'Uninstaller missing' }
    $removal = Start-Process -FilePath $uninstall -ArgumentList '/S' -PassThru
    if (-not $removal.WaitForExit(60000)) { $removal.Kill(); throw 'Uninstaller timed out' }
    if ($removal.ExitCode -ne 0) { throw 'Uninstaller failed' }
    for ($attempt = 0; $attempt -lt 120 -and (Test-Path $executable); $attempt++) { Start-Sleep -Milliseconds 250 }
    if (Test-Path $executable) { throw 'Uninstaller left the installed executable' }
    if ([IO.File]::ReadAllText($sentinel) -ne 'preserve-user-data') { throw 'Uninstall removed application data' }
    @{
        schemaVersion = 1; assertionsPassed = $true; complete = $false
        checks = @('silent-install', 'packaged-window', 'database-init', 'normal-exit', 'upgrade-preserves-data', 'uninstall-preserves-data')
        notChecked = @('interactive UI workflows', 'older-schema rollback', 'Authenticode signing', 'long-running multi-session load')
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $Report -Encoding utf8
} finally {
    if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
}
