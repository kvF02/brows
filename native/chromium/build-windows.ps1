param(
    [Parameter(Mandatory)][string]$BuildRoot,
    [Parameter(Mandatory)][string]$Report
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Windows x64 build host required' }
$python = (Get-Command python -CommandType Application).Source
$node = (Get-Command node -CommandType Application).Source
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'source-manifest.json') -Raw | ConvertFrom-Json
$reportPath = [IO.Path]::GetFullPath($Report)
$root = [IO.Path]::GetFullPath($BuildRoot)
if ($root -match '\s' -or (Test-Path -LiteralPath $root)) { throw 'Use a new build root without spaces' }
$result = [ordered]@{ schemaVersion = 1; sourceRevision = $manifest.upstream.revision; stage = 'preflight'; compiled = $false; nativeAcceptancePassed = $false; runtimeAdmitted = $false; complete = $false; failure = $null }
$oldPath = $env:PATH
$oldToolchain = $env:DEPOT_TOOLS_WIN_TOOLCHAIN
$oldUpdate = $env:DEPOT_TOOLS_UPDATE

function Assert-Exit([string]$Stage) {
    if ($LASTEXITCODE -ne 0) { throw "$Stage failed with exit $LASTEXITCODE" }
}

try {
    & (Join-Path $PSScriptRoot 'windows-preflight.ps1') -Report "$reportPath.preflight.json" -RequireReady
    $preflight = Get-Content -LiteralPath "$reportPath.preflight.json" -Raw | ConvertFrom-Json
    if (-not $preflight.readyForBuild) { throw 'Build prerequisites were not observed' }
    $drive = [IO.Path]::GetPathRoot($root).TrimEnd('\')
    $disk = @($preflight.drives | Where-Object drive -eq $drive)
    if ($disk.Count -ne 1 -or $disk[0].filesystem -ne 'NTFS' -or $disk[0].freeGiB -lt $manifest.buildRequirements.minimumFreeDiskGiB) { throw 'Build root is not on an admitted NTFS volume' }
    New-Item -ItemType Directory -Path $root | Out-Null
    $result.stage = 'depot-tools'
    $depot = Join-Path $root 'depot_tools'
    git clone --no-checkout $manifest.depotTools.repository $depot
    Assert-Exit 'depot_tools clone'
    git -C $depot checkout --detach $manifest.depotTools.revision
    Assert-Exit 'depot_tools pin'
    $env:PATH = "$depot;$oldPath"
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
    $env:DEPOT_TOOLS_UPDATE = '0'
    $sourceRoot = Join-Path $root 'chromium'
    New-Item -ItemType Directory -Path $sourceRoot | Out-Null
    $configuration = "solutions = [{'name': 'src', 'url': '$($manifest.upstream.repository)', 'managed': False, 'custom_deps': {}, 'custom_vars': {'checkout_android': False, 'checkout_ios': False}}]`ntarget_os = ['win']`n"
    [IO.File]::WriteAllText((Join-Path $sourceRoot '.gclient'), $configuration, [Text.UTF8Encoding]::new($false))
    Push-Location $sourceRoot
    try {
        $result.stage = 'source-checkout'
        & (Join-Path $depot 'gclient.bat') sync --no-history --nohooks --revision "src@$($manifest.upstream.revision)"
        Assert-Exit 'pinned Chromium sync'
        $src = Join-Path $sourceRoot 'src'
        $result.stage = 'patch'
        & $python (Join-Path $PSScriptRoot 'verify_patch.py') --source $src --apply --report "$reportPath.source.json"
        Assert-Exit 'pinned patch and probe generation'
        $result.stage = 'hooks'
        & (Join-Path $depot 'gclient.bat') runhooks
        Assert-Exit 'Chromium hooks'
        Push-Location $src
        try {
            $result.stage = 'generate'
            & (Join-Path $depot 'gn.bat') gen out/BrowsNative '--args=is_debug=false is_component_build=false symbol_level=0 blink_symbol_level=0 v8_symbol_level=0 target_cpu="x64" use_remoteexec=false'
            Assert-Exit 'GN generation'
            $result.stage = 'compile'
            & (Join-Path $depot 'autoninja.bat') -C out/BrowsNative headless_shell
            Assert-Exit 'headless shell compilation'
            $binary = Join-Path $src 'out/BrowsNative/headless_shell.exe'
            if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Compiled test executable missing' }
            $digest = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
            $result.compiled = $true
            $result.testExecutableSha256 = $digest
            $result.stage = 'native-acceptance'
            & $node (Join-Path $PSScriptRoot 'platform-acceptance.mjs') --browser $binary --sha256 $digest --expect native --report "$reportPath.acceptance.json"
            Assert-Exit 'native Worker platform acceptance'
            $evidence = Get-Content -LiteralPath "$reportPath.acceptance.json" -Raw | ConvertFrom-Json
            if (-not $evidence.assertionsPassed -or -not $evidence.nativeWorkerPlatform) { throw 'Native acceptance did not establish the requested capability' }
            $result.nativeAcceptancePassed = $true
            $result.stage = 'completed-test-build'
        } finally { Pop-Location }
    } finally { Pop-Location }
} catch {
    $result.failure = $_.Exception.Message
} finally {
    $env:PATH = $oldPath
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = $oldToolchain
    $env:DEPOT_TOOLS_UPDATE = $oldUpdate
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($reportPath)) | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding utf8
    $result | ConvertTo-Json -Depth 8 | Write-Output
}
if (-not $result.nativeAcceptancePassed) { exit 1 }
