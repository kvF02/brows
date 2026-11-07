param(
    [Parameter(Mandatory)][string]$TestExecutable,
    [Parameter(Mandatory)][ValidateRange(60, 14400)][int]$Seconds,
    [Parameter(Mandatory)][string]$Report,
    [ValidateSet('SystemNative', 'SeedableNoise')][string]$Persona = 'SystemNative'
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'The Windows soak requires PowerShell on Windows' }
. "$PSScriptRoot/windows-process-fixture.ps1"
$testExe = (Resolve-Path -LiteralPath $TestExecutable).Path
if ([IO.Path]::GetFileName($testExe) -notmatch '^brows_desktop-[a-f0-9]+\.exe$') {
    throw 'Use the brows-desktop library test executable'
}
$root = Join-Path ([IO.Path]::GetTempPath()) "brows-soak-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $root | Out-Null
New-Item -ItemType File -Path (Join-Path $root 'soak-fixture') | Out-Null
$reportPath = [IO.Path]::GetFullPath($Report)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($reportPath)) | Out-Null
$samples = [Collections.Generic.List[object]]::new()
$checks = [Collections.Generic.List[string]]::new()
$tracked = @()
$soakProcess = $null
$reopened = $null
$stage = 'startup'
$result = [ordered]@{
    schemaVersion = 1
    assertionsPassed = $false
    platform = [Environment]::OSVersion.VersionString
    loopbackSleepException = $env:BROWS_SOAK_LOOPBACK_POLICY
    commit = $env:GITHUB_SHA
    persona = $Persona
    concurrentProfiles = 10
    requestedSeconds = $Seconds
    observedSeconds = 0
    checks = $checks
    samples = $samples
    notChecked = @('packaged UI soak', 'real websites and public DNS/ICE', 'power loss', 'resource limits')
    failure = $null
}

function Start-SoakHost([string]$Phase) {
    $start = @{
        FilePath = $testExe
        ArgumentList = @('--ignored', '--exact', 'state::tests::soak::desktop_soak_host', '--nocapture')
        Environment = @{
            BROWS_SOAK_ROOT = $root
            BROWS_SOAK_PHASE = $Phase
            BROWS_SOAK_SECONDS = [string]$Seconds
            BROWS_SOAK_PERSONA = $Persona
        }
        RedirectStandardOutput = Join-Path $root "$Phase.stdout.txt"
        RedirectStandardError = Join-Path $root "$Phase.stderr.txt"
        PassThru = $true
    }
    Start-Process @start
}

function Wait-SoakMarker([Diagnostics.Process]$Process, [string]$Name, [int]$LimitSeconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath (Join-Path $root $Name))) {
        if ($Process.HasExited) { throw "Test host exited before $Name (code $($Process.ExitCode))" }
        if ($timer.Elapsed.TotalSeconds -ge $LimitSeconds) { throw "Timed out waiting for $Name" }
        Start-Sleep -Milliseconds 500
    }
}

function Find-BrowserRoots($Snapshot, [int]$OwnerId, $Ready) {
    $edge = $Ready.edgeExecutable.Replace('\\?\', '')
    $profilePrefix = $Ready.profileRoot.Replace('\\?\', '') + '\'
    $children = @($Snapshot | Where-Object ParentProcessId -eq $OwnerId)
    $edgeChildren = @($children | Where-Object {
        $_.ExecutablePath -and $_.ExecutablePath.Replace('\\?\', '') -ieq $edge
    })
    $roots = @($edgeChildren | Where-Object {
        $_.CommandLine -and $_.CommandLine.Replace('\\?\', '').Contains($profilePrefix, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($roots.Count -ne 10) {
        throw "Expected ten owned Edge roots, found $($roots.Count) ($($children.Count) direct children, $($edgeChildren.Count) matching Edge paths)"
    }
    $roots
}

try {
    $soakProcess = Start-SoakHost 'exercise'
    Wait-SoakMarker $soakProcess 'steady-ready.json' 180
    $ready = Get-Content -LiteralPath (Join-Path $root 'steady-ready.json') -Raw | ConvertFrom-Json
    $result.runtimeVersion = $ready.runtimeVersion
    if ($Persona -eq 'SeedableNoise') { $result.runtimeId = $ready.runtimeId }
    $checks.Add('ten-pages-sending-heartbeats')

    $stage = 'steady'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $nextSample = 0
    while (-not (Test-Path -LiteralPath (Join-Path $root 'browser-crash-ready'))) {
        if ($soakProcess.HasExited) { throw "Test host failed during steady load (code $($soakProcess.ExitCode))" }
        $elapsed = [int]$timer.Elapsed.TotalSeconds
        if ($elapsed -gt ($Seconds + 60)) { throw 'The steady phase did not finish in time' }
        if ($elapsed -ge $nextSample) {
            $snapshot = @(Read-ProcessSnapshot)
            $roots = @(Find-BrowserRoots $snapshot $soakProcess.Id $ready)
            $tracked = @(Find-ProcessTree $snapshot $roots)
            $bytes = ($tracked | Measure-Object -Property WorkingSetSize -Sum).Sum
            $userTime = ($tracked | Measure-Object -Property UserModeTime -Sum).Sum
            $kernelTime = ($tracked | Measure-Object -Property KernelModeTime -Sum).Sum
            $sample = [ordered]@{
                elapsedSeconds = $elapsed
                browserProcesses = $tracked.Count
                workingSetMiB = [Math]::Round($bytes / 1MB, 1)
                liveProcessCpuSeconds = [Math]::Round(($userTime + $kernelTime) / 10000000, 2)
            }
            $result.observedSeconds = $elapsed
            $samples.Add($sample)
            Write-Output ($sample | ConvertTo-Json -Compress)
            $nextSample = $elapsed + 60
        }
        Start-Sleep -Seconds 2
    }
    $result.observedSeconds = [int](Get-Content -LiteralPath (Join-Path $root 'browser-crash-ready') -Raw)
    if ($result.observedSeconds -lt $Seconds -or $samples.Count -eq 0) { throw 'Missing steady load evidence' }
    $checks.Add('continuous-ten-profile-load')
    Write-Output "Ten profiles stayed active for $($result.observedSeconds) seconds"

    $stage = 'browser-crash'
    $snapshot = @(Read-ProcessSnapshot)
    $roots = @(Find-BrowserRoots $snapshot $soakProcess.Id $ready)
    if ($Persona -eq 'SeedableNoise') {
        # Controlled startup URLs are sent through the pipe. Match the exact
        # user-data argument emitted for the first fixture-owned browser.
        $argument = '"--user-data-dir=' + $ready.victimUserData.Replace('\\?\', '') + '"'
        $victims = @($roots | Where-Object {
            $_.CommandLine.Replace('\\?\', '').Contains($argument, [StringComparison]::OrdinalIgnoreCase)
        })
    } else {
        $victims = @($roots | Where-Object { $_.CommandLine.Contains($ready.victimPage) })
    }
    if ($victims.Count -ne 1) { throw 'Could not identify the owned fault target' }
    $victimTree = @(Find-ProcessTree $snapshot $victims)
    $tracked += $victimTree
    if ($victimTree.Count -lt 2) { throw 'The crash target has no observed child processes' }
    Stop-OwnedProcess $victims[0]
    Wait-SoakMarker $soakProcess 'host-crash-ready' 90
    Wait-ProcessTreeExit $victimTree
    $result.reclaimedAfterBrowserCrash = $victimTree.Count
    $checks.Add('browser-crash-reclaims-tree-and-promotes-queue')
    Write-Output "Browser crash reclaimed $($victimTree.Count) observed processes and promoted the queued profile"

    $stage = 'host-crash'
    $snapshot = @(Read-ProcessSnapshot)
    $roots = @(Find-BrowserRoots $snapshot $soakProcess.Id $ready)
    $hostTree = @(Find-ProcessTree $snapshot $roots)
    $tracked += $hostTree
    if ($hostTree.Count -le 10) { throw 'The host has no observed browser descendants' }
    $soakProcess.Kill()
    if (-not $soakProcess.WaitForExit(15000)) { throw 'The test host did not terminate' }
    Wait-ProcessTreeExit $hostTree
    $result.reclaimedAfterHostCrash = $hostTree.Count
    $checks.Add('abrupt-host-exit-reclaims-all-ten-browser-trees')
    Write-Output "Host crash reclaimed $($hostTree.Count) observed processes across ten trees"

    $stage = 'reopen'
    $reopened = Start-SoakHost 'reopen'
    Wait-SoakMarker $reopened 'reopen-verified' 120
    if (-not $reopened.WaitForExit(30000) -or $reopened.ExitCode -ne 0) { throw 'The reopened test host failed' }
    $checks.Add('sqlite-profiles-and-relaunch-survive-host-crash')
    if ($Persona -eq 'SeedableNoise') {
        $evidence = Get-Content -LiteralPath (Join-Path $root 'reopen-verified') -Raw | ConvertFrom-Json
        if ($evidence.restartedProfiles -ne 11 -or $evidence.profileSignals.Count -ne 11) {
            throw 'Missing SeedableNoise fingerprint recovery evidence'
        }
        $result.restartedProfiles = $evidence.restartedProfiles
        $result.contexts = $evidence.contexts
        $result.profileSignals = $evidence.profileSignals
        $result.complete = $false
        $checks.Add('distinct-seed-canvas')
        $checks.Add('page-and-worker-consistency')
        $checks.Add('continuous-page-canvas')
        $checks.Add('eleven-fresh-matching-fingerprints-after-host-crash')
        Write-Output 'All eleven reopened SeedableNoise profiles reproduced their page and Worker signals'
    } else {
        Write-Output 'Reopened all eleven profiles and relaunched an existing profile'
    }
    $result.assertionsPassed = $true
} catch {
    $result.failure = @{ stage = $stage; message = $_.Exception.Message }
    foreach ($name in @('exercise.stdout.txt', 'exercise.stderr.txt', 'reopen.stdout.txt', 'reopen.stderr.txt')) {
        $log = Join-Path $root $name
        if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Tail 25 | Write-Output }
    }
    throw
} finally {
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding utf8
    foreach ($process in @($soakProcess, $reopened)) {
        if ($process -and -not $process.HasExited) { $process.Kill(); $null = $process.WaitForExit(15000) }
    }
    if (-not $result.assertionsPassed -and $tracked.Count -gt 0) {
        foreach ($entry in (Find-RemainingProcesses $tracked)) {
            try { Stop-OwnedProcess $entry } catch { Write-Warning 'A captured process could not be reaped' }
        }
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
