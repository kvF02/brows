param([Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'source-manifest.json') -Raw | ConvertFrom-Json
$result = [ordered]@{ schemaVersion = 1; scope = 'Pinned depot_tools Python bootstrap only; no Chromium build'; assertionsPassed = $false; compiled = $false; nativeAcceptancePassed = $false; complete = $false; failure = $null }
$oldPath = $env:PATH
$oldUpdate = $env:DEPOT_TOOLS_UPDATE
$oldToolchain = $env:DEPOT_TOOLS_WIN_TOOLCHAIN

try {
    if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') { throw 'Bootstrap smoke requires a disposable GitHub-hosted Windows runner' }
    . (Join-Path $PSScriptRoot 'windows-tools.ps1')
    $depot = Join-Path $env:RUNNER_TEMP "brows-depot-bootstrap-$([Guid]::NewGuid())"
    git clone --no-checkout $manifest.depotTools.repository $depot
    if ($LASTEXITCODE -ne 0) { throw 'depot_tools smoke clone failed' }
    git -C $depot checkout --detach $manifest.depotTools.revision
    if ($LASTEXITCODE -ne 0) { throw 'depot_tools smoke pin failed' }
    $env:PATH = "$depot;$oldPath"
    $env:DEPOT_TOOLS_UPDATE = '0'
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
    $result.pythonLocationPresentBefore = Test-Path -LiteralPath (Join-Path $depot 'python3_bin_reldir.txt')
    $initialized = Initialize-NativeDepotTools -Depot $depot -Revision $manifest.depotTools.revision
    $result.sourceRevision = $initialized.sourceRevision
    $result.pythonVersion = $initialized.pythonVersion
    $result.pythonShimVerified = $initialized.pythonShimVerified
    $result.trackedFilesUnchanged = $initialized.trackedFilesUnchanged
    $result.assertionsPassed = $true
} catch {
    $result.failure = $_.Exception.Message
} finally {
    $env:PATH = $oldPath
    $env:DEPOT_TOOLS_UPDATE = $oldUpdate
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = $oldToolchain
    $path = [IO.Path]::GetFullPath($Report)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $path -Encoding utf8
    $result | ConvertTo-Json -Depth 8 | Write-Output
}
if (-not $result.assertionsPassed) { exit 1 }
