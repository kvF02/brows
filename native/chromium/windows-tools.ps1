function Resolve-NativeBuildTools {
    # Get-Command -CommandType Application may enumerate multiple PATH matches.
    # Keep the first executable as a scalar before depot_tools changes PATH.
    $python = (Get-Command python -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $node = (Get-Command node -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    if (-not ($python -is [string]) -or -not ($node -is [string])) { throw 'Build tools must resolve to single executable paths' }
    $pythonVersion = [string](& $python --version)
    if ($LASTEXITCODE -ne 0 -or $pythonVersion -notmatch '^Python 3\.(\d+)\.\d+$' -or [int]$Matches[1] -lt 11) { throw 'Python 3.11+ must be executable before source checkout' }
    $nodeVersion = [string](& $node --version)
    if ($LASTEXITCODE -ne 0 -or $nodeVersion -notmatch '^v(\d+)\.' -or [int]$Matches[1] -lt 24) { throw 'Node 24+ must be executable before source checkout' }
    [pscustomobject]@{ python = $python; node = $node; pythonVersion = $pythonVersion; nodeVersion = $nodeVersion }
}

function Initialize-NativeDepotTools {
    param([Parameter(Mandatory)][string]$Depot, [Parameter(Mandatory)][string]$Revision)
    if (-not $IsWindows -or $Revision -notmatch '^[a-f0-9]{40}$') { throw 'Pinned Windows depot_tools checkout required' }
    $before = [string](& git -C $Depot rev-parse HEAD)
    if ($LASTEXITCODE -ne 0 -or $before -ne $Revision) { throw 'depot_tools checkout does not match its source pin' }

    # DEPOT_TOOLS_UPDATE=0 skips this initialization in gclient.bat. GN and
    # autoninja invoke python-bin/python3.bat, which needs the generated Python
    # location file. Bootstrap dependencies without updating the pinned Git tree.
    & (Join-Path $Depot 'bootstrap/win_tools.bat') | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Pinned depot_tools Windows bootstrap failed' }
    $shim = Join-Path $Depot 'python-bin/python3.bat'
    $version = [string](& $shim --version)
    if ($LASTEXITCODE -ne 0 -or $version -notmatch '^Python 3\.\d+\.\d+') { throw 'depot_tools Python shim is not executable after bootstrap' }
    $after = [string](& git -C $Depot rev-parse HEAD)
    if ($LASTEXITCODE -ne 0 -or $after -ne $Revision) { throw 'Bootstrap changed the pinned depot_tools revision' }
    $changes = & git -C $Depot status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0 -or $changes) { throw 'Bootstrap modified tracked depot_tools files' }
    [pscustomobject]@{ sourceRevision = $after; pythonVersion = $version; pythonShimVerified = $true; trackedFilesUnchanged = $true }
}
