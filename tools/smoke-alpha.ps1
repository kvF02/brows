#Requires -Version 7.2
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Executable)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:RUNNER_OS -ne 'Windows' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'This launch smoke is restricted to fresh GitHub-hosted Windows runners.'
}
$reportDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) 'target/alpha-startup'
New-Item -ItemType Directory -Path $reportDirectory -Force | Out-Null
$result = [ordered]@{ schemaVersion = 1; scope = 'manager_window_start_close'; commit = $env:GITHUB_SHA; windowDetected = $false; normalExit = $false; error = $null }
$process = $null
try {
    $process = Start-Process -FilePath (Resolve-Path -LiteralPath $Executable).Path -PassThru `
        -RedirectStandardOutput (Join-Path $reportDirectory 'stdout.log') `
        -RedirectStandardError (Join-Path $reportDirectory 'stderr.log')
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $process.Refresh()
        if ($process.HasExited) { throw "Alpha exited before showing its window (code $($process.ExitCode))." }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero -and $process.MainWindowTitle -eq 'Brows Alpha') { break }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($process.MainWindowHandle -eq [IntPtr]::Zero -or $process.MainWindowTitle -ne 'Brows Alpha') { throw 'Alpha did not expose its expected main window.' }
    $result.windowDetected = $true
    if (-not $process.CloseMainWindow()) { throw 'Alpha did not accept the window close request.' }
    if (-not $process.WaitForExit(10000)) { throw 'Alpha did not exit after closing its main window.' }
    if ($process.ExitCode -ne 0) { throw "Alpha exited with code $($process.ExitCode)." }
    $result.normalExit = $true
} catch {
    $result.error = $_.Exception.Message
    throw
} finally {
    if ($null -ne $process) {
        if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit(5000) | Out-Null }
        $process.Dispose()
    }
    [IO.File]::WriteAllText((Join-Path $reportDirectory 'startup.json'), ($result | ConvertTo-Json) + "`n", [Text.UTF8Encoding]::new($false))
}
