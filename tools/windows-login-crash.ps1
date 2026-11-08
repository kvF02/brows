param([Parameter(Mandatory)][uint32]$OwnerId, [Parameter(Mandatory)][string]$ProfileRoot)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $IsWindows) { throw 'Hosted Windows fixture only' }
. "$PSScriptRoot/windows-process-fixture.ps1"
$root = (Resolve-Path -LiteralPath $ProfileRoot).Path.Replace('\\?\', '')
$temp = [IO.Path]::GetTempPath().TrimEnd('\') + '\'
if (-not $root.StartsWith($temp, [StringComparison]::OrdinalIgnoreCase) -or $root -notmatch '\\brows-login-[a-f0-9-]{36}\\data\\profiles$') { throw 'Expected the isolated login fixture' }
$snapshot = @(Read-ProcessSnapshot)
$owner = @($snapshot | Where-Object { $_.ProcessId -eq $OwnerId -and $_.Name -match '^brows_desktop-[a-f0-9]+\.exe$' })
if ($owner.Count -ne 1) { throw 'Missing owned Rust test host' }
$caller = @($snapshot | Where-Object ProcessId -eq $PID)
if ($caller.Count -ne 1 -or $caller[0].ParentProcessId -ne $OwnerId) { throw 'Crash helper must be a direct child of the test host' }
$roots = @($snapshot | Where-Object { $_.ParentProcessId -eq $OwnerId -and $_.Name -ieq 'msedge.exe' -and $_.CommandLine -and $_.CommandLine.Replace('\\?\', '').Contains($root + '\', [StringComparison]::OrdinalIgnoreCase) })
if ($roots.Count -ne 1) { throw "Expected one owned active browser, found $($roots.Count)" }
$tree = @(Find-ProcessTree $snapshot $roots)
Stop-OwnedProcess $roots[0]
foreach ($remaining in @(Find-RemainingProcesses $tree)) {
    try { Stop-OwnedProcess $remaining }
    catch {
        # Killing the root can make a captured child exit by itself between
        # the snapshot and handle acquisition. Never target a replacement PID.
        if (@(Find-RemainingProcesses @($remaining)).Count -gt 0) { throw }
    }
}
Wait-ProcessTreeExit $tree
@{ browserRoots = 1; observedProcessesReclaimed = $tree.Count } | ConvertTo-Json -Compress
