param([Parameter(Mandatory)][string]$Application)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $IsWindows) {
    throw 'The installed UI crash is restricted to a hosted Windows fixture'
}
. "$PSScriptRoot/windows-process-fixture.ps1"
$app = (Resolve-Path -LiteralPath $Application).Path
$expected = Join-Path $env:GITHUB_WORKSPACE 'target\installed\brows-desktop.exe'
if ($app -ine $expected) { throw 'Only the fixture-installed executable may be terminated' }
$snapshot = @(Read-ProcessSnapshot)
$sessionId = [Diagnostics.Process]::GetCurrentProcess().SessionId
$owners = @($snapshot | Where-Object { $_.ExecutablePath -ieq $app -and $_.SessionId -eq $sessionId })
if ($owners.Count -ne 1) { throw "Expected one installed Brows process, found $($owners.Count)" }
$owner = $owners[0]
$edge = Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'
$roots = @($snapshot | Where-Object {
    $_.ParentProcessId -eq $owner.ProcessId -and $_.ExecutablePath -ieq $edge -and
    $_.CommandLine -and $_.CommandLine.Contains('\local-brows-desktop\profiles\', [StringComparison]::OrdinalIgnoreCase)
})
if ($roots.Count -ne 10) { throw "Expected ten browser roots owned by the installed app, found $($roots.Count)" }
$tree = @(Find-ProcessTree $snapshot $roots)
if ($tree.Count -le $roots.Count) { throw 'The browser trees must include observed descendants' }
Stop-OwnedProcess $owner
Wait-ProcessTreeExit $tree
@{
    browserRoots = $roots.Count
    observedProcessesReclaimed = $tree.Count
} | ConvertTo-Json -Compress
