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
