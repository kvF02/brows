function Read-ProcessSnapshot {
    Get-CimInstance Win32_Process -OperationTimeoutSec 10
}

function Find-ProcessTree($Snapshot, $Roots) {
    $pending = [Collections.Generic.Queue[object]]::new()
    $seen = [Collections.Generic.HashSet[uint32]]::new()
    foreach ($entry in $Roots) { $pending.Enqueue($entry) }
    while ($pending.Count -gt 0) {
        $entry = $pending.Dequeue()
        if (-not $seen.Add($entry.ProcessId)) { continue }
        $entry
        foreach ($child in $Snapshot) {
            if ($child.ParentProcessId -eq $entry.ProcessId -and $child.CreationDate -ge $entry.CreationDate) {
                $pending.Enqueue($child)
            }
        }
    }
}

function Find-RemainingProcesses($Recorded) {
    $live = @{}
    foreach ($entry in (Get-CimInstance Win32_Process -Property ProcessId, CreationDate -OperationTimeoutSec 10)) {
        $live[$entry.ProcessId] = $entry.CreationDate
    }
    $Recorded | Where-Object { $live.ContainsKey($_.ProcessId) -and $live[$_.ProcessId] -eq $_.CreationDate }
}

function Wait-ProcessTreeExit($Recorded) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $remaining = @(Find-RemainingProcesses $Recorded)
        if ($remaining.Count -eq 0) { return }
        Start-Sleep -Milliseconds 500
    } while ($timer.Elapsed.TotalSeconds -lt 30)
    throw "$($remaining.Count) captured browser processes survived their owner"
}

function Stop-OwnedProcess($Entry) {
    $process = [Diagnostics.Process]::GetProcessById($Entry.ProcessId)
    try {
        if ($process.SafeHandle.IsInvalid) { throw 'Unable to retain the owned process handle' }
        $ageDifference = $process.StartTime.ToUniversalTime() - $Entry.CreationDate.ToUniversalTime()
        if ([Math]::Abs($ageDifference.TotalMilliseconds) -gt 1) {
            throw 'Process identity changed before fault injection'
        }
        $process.Kill()
        if (-not $process.WaitForExit(15000)) { throw 'Owned process did not terminate' }
    } finally {
        $process.Dispose()
    }
}
