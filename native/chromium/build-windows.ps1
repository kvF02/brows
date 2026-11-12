param(
    [Parameter(Mandatory)][string]$BuildRoot,
    [Parameter(Mandatory)][string]$Report,
    [switch]$PrepareHostedSdk
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Windows x64 build host required' }
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'source-manifest.json') -Raw | ConvertFrom-Json
$reportPath = [IO.Path]::GetFullPath($Report)
$root = [IO.Path]::GetFullPath($BuildRoot)
if ($root -match '\s' -or (Test-Path -LiteralPath $root)) { throw 'Use a new build root without spaces' }
$result = [ordered]@{ schemaVersion = 1; sourceRevision = $manifest.upstream.revision; stage = 'preflight'; compiled = $false; nativeAcceptancePassed = $false; runtimeAdmitted = $false; complete = $false; failure = $null }
$result.nativeFirstScriptsPassed = $false
$result.nativeLifecyclePassed = $false
$result.runtimeBundleReady = $false
$bundle = $null
$oldPath = $env:PATH
$oldToolchain = $env:DEPOT_TOOLS_WIN_TOOLCHAIN
$oldUpdate = $env:DEPOT_TOOLS_UPDATE
$gitCount = 0
if ($env:GIT_CONFIG_COUNT -and -not [int]::TryParse($env:GIT_CONFIG_COUNT, [ref]$gitCount)) { throw 'Invalid inherited Git configuration count' }
if ($gitCount -lt 0 -or $gitCount -gt 256) { throw 'Invalid inherited Git configuration count' }
$gitChanges = @{}
$gitChanges['GIT_CONFIG_COUNT'] = [string]($gitCount + 2)
$gitChanges["GIT_CONFIG_KEY_$gitCount"] = 'core.autocrlf'
$gitChanges["GIT_CONFIG_VALUE_$gitCount"] = 'false'
$gitChanges["GIT_CONFIG_KEY_$($gitCount + 1)"] = 'core.longpaths'
$gitChanges["GIT_CONFIG_VALUE_$($gitCount + 1)"] = 'true'
$oldGitEnvironment = @{}

function Assert-Exit([string]$Stage) {
    if ($LASTEXITCODE -ne 0) { throw "$Stage failed with exit $LASTEXITCODE" }
}

try {
    $result.stage = 'tool-resolution'
    . (Join-Path $PSScriptRoot 'windows-tools.ps1')
    $tools = Resolve-NativeBuildTools
    $python = $tools.python
    $node = $tools.node
    $result.pythonVersion = $tools.pythonVersion
    $result.nodeVersion = $tools.nodeVersion
    $result.stage = 'preflight'
    if ($PrepareHostedSdk) {
        & (Join-Path $PSScriptRoot 'prepare-hosted-windows.ps1') -Report "$reportPath.sdk.json"
        $setup = Get-Content -LiteralPath "$reportPath.sdk.json" -Raw | ConvertFrom-Json
        if (-not $setup.readyForBuild) { throw 'Hosted SDK preparation did not establish build prerequisites' }
    }
    & (Join-Path $PSScriptRoot 'windows-preflight.ps1') -Report "$reportPath.preflight.json" -RequireReady
    $preflight = Get-Content -LiteralPath "$reportPath.preflight.json" -Raw | ConvertFrom-Json
    if (-not $preflight.readyForBuild) { throw 'Build prerequisites were not observed' }
    $drive = [IO.Path]::GetPathRoot($root).TrimEnd('\')
    $disk = @($preflight.drives | Where-Object drive -eq $drive)
    if ($disk.Count -ne 1 -or $disk[0].filesystem -ne 'NTFS' -or $disk[0].freeGiB -lt $manifest.buildRequirements.minimumFreeDiskGiB) { throw 'Build root is not on an admitted NTFS volume' }
    New-Item -ItemType Directory -Path $root | Out-Null
    # Preserve exact pinned bytes and long paths without changing global Git
    # settings or discarding inherited per-process configuration entries.
    foreach ($name in $gitChanges.Keys) {
        $oldGitEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $gitChanges[$name], 'Process')
    }
    $result.stage = 'depot-tools'
    $depot = Join-Path $root 'depot_tools'
    git clone --no-checkout $manifest.depotTools.repository $depot
    Assert-Exit 'depot_tools clone'
    git -C $depot checkout --detach $manifest.depotTools.revision
    Assert-Exit 'depot_tools pin'
    $env:PATH = "$depot;$oldPath"
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
    $env:DEPOT_TOOLS_UPDATE = '0'
    $result.stage = 'depot-bootstrap'
    $initialized = Initialize-NativeDepotTools -Depot $depot -Revision $manifest.depotTools.revision
    $result.depotPythonVersion = $initialized.pythonVersion
    $result.depotPythonShimVerified = $initialized.pythonShimVerified
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
            $gnArgs = 'is_debug=false is_component_build=false symbol_level=0 blink_symbol_level=0 v8_symbol_level=0 target_cpu="x64" use_remoteexec=false'
            $result.gnArgs = $gnArgs
            $outDirectory = Join-Path $src 'out/BrowsNative'
            [IO.Directory]::CreateDirectory($outDirectory) | Out-Null
            # A real args.gn file avoids PowerShell/batch quote transformations.
            [IO.File]::WriteAllText((Join-Path $outDirectory 'args.gn'), "$gnArgs`n", [Text.UTF8Encoding]::new($false))
            & (Join-Path $depot 'gn.bat') gen out/BrowsNative
            Assert-Exit 'GN generation'
            $result.stage = 'compile'
            & (Join-Path $depot 'autoninja.bat') -C out/BrowsNative headless_shell
            Assert-Exit 'headless shell compilation'
            $binary = Join-Path $src 'out/BrowsNative/headless_shell.exe'
            if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Compiled test executable missing' }
            $digest = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
            $result.compiled = $true
            $result.testExecutableSha256 = $digest
            $result.stage = 'runtime-bundle'
            & (Join-Path $depot 'autoninja.bat') -C out/BrowsNative generate_headless_shell_license_file
            Assert-Exit 'runtime license generation'
            $dependencies = & (Join-Path $depot 'gn.bat') desc out/BrowsNative //headless:headless_shell runtime_deps
            Assert-Exit 'runtime dependency inventory'
            $dependencies | Set-Content -LiteralPath "$reportPath.runtime-deps.txt" -Encoding utf8
            $bundle = Join-Path $root 'test-runtime'
            & $python (Join-Path $PSScriptRoot 'stage_runtime.py') --out (Join-Path $src 'out/BrowsNative') --deps "$reportPath.runtime-deps.txt" --dest $bundle --sha256 $digest
            Assert-Exit 'runtime dependency staging'
            $result.runtimeBundleReady = $true
            $binary = Join-Path $bundle 'headless_shell.exe'
            $result.stage = 'native-acceptance'
            & $node (Join-Path $PSScriptRoot 'platform-acceptance.mjs') --browser $binary --sha256 $digest --expect native --report "$reportPath.acceptance.json"
            Assert-Exit 'native Worker platform acceptance'
            $evidence = Get-Content -LiteralPath "$reportPath.acceptance.json" -Raw | ConvertFrom-Json
            if (-not $evidence.assertionsPassed -or -not $evidence.nativeWorkerPlatform) { throw 'Native acceptance did not establish the requested capability' }
            $result.nativeFirstScriptsPassed = $true
            $result.stage = 'native-lifecycle'
            $lifecycle = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../tools/native-worker-lifecycle.mjs'))
            & $node $lifecycle --browser $binary --sha256 $digest --expect native --report "$reportPath.lifecycle.json"
            Assert-Exit 'native Worker lifecycle acceptance'
            $lifecycleEvidence = Get-Content -LiteralPath "$reportPath.lifecycle.json" -Raw | ConvertFrom-Json
            if (-not $lifecycleEvidence.assertionsPassed -or -not $lifecycleEvidence.nativeLifecyclePassed) { throw 'Native lifecycle acceptance did not establish the requested capability' }
            $result.nativeLifecyclePassed = $true
            $result.nativeAcceptancePassed = $true
            $result.stage = 'completed-test-build'
        } finally { Pop-Location }
    } finally { Pop-Location }
} catch {
    $result.failure = $_.Exception.Message
} finally {
    if ($result.runtimeBundleReady) {
        try {
            & $python (Join-Path $PSScriptRoot 'stage_runtime.py') --verify --dest $bundle --sha256 $digest
            Assert-Exit 'runtime bundle integrity after browser tests'
        } catch {
            $result.runtimeBundleReady = $false
            $result.nativeAcceptancePassed = $false
            $result.bundleFailure = $_.Exception.Message
            if (-not $result.failure) { $result.failure = $result.bundleFailure; $result.stage = 'runtime-bundle-integrity' }
        }
        if ($result.runtimeBundleReady -and $env:GITHUB_OUTPUT) {
            'test_runtime_ready=true' | Add-Content -LiteralPath $env:GITHUB_OUTPUT
            "test_runtime_directory=$bundle" | Add-Content -LiteralPath $env:GITHUB_OUTPUT
        }
    }
    foreach ($name in $oldGitEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $oldGitEnvironment[$name], 'Process')
    }
    $env:PATH = $oldPath
    $env:DEPOT_TOOLS_WIN_TOOLCHAIN = $oldToolchain
    $env:DEPOT_TOOLS_UPDATE = $oldUpdate
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($reportPath)) | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding utf8
    $result | ConvertTo-Json -Depth 8 | Write-Output
}
if (-not $result.nativeAcceptancePassed) { exit 1 }
