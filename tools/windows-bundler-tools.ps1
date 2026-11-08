# Pinned to the toolchain in tauri-cli-v2.12.4, not the runner's NSIS version.
# https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.12.4/crates/tauri-bundler/src/bundle/windows/nsis/mod.rs
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:OS -ne 'Windows_NT') { throw 'Disposable hosted Windows build only' }
$downloads = Join-Path $env:RUNNER_TEMP 'brows-tauri-downloads'
New-Item -ItemType Directory -Path $downloads -Force | Out-Null
function Get-PinnedDownload([string]$Name, [string]$Sha256, [string[]]$Urls) {
    $file = Join-Path $downloads $Name
    if ((Test-Path -LiteralPath $file) -and (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ieq $Sha256) { return $file }
    $index = 0
    foreach ($url in $Urls) {
        $index++
        $partial = "$file.partial"
        try {
            Write-Host "Downloading pinned $Name from official endpoint $index"
            $headers = @{ 'User-Agent' = 'brows-pinned-bundler-tools' }
            if ($url.StartsWith('https://api.github.com/')) { $headers.Accept = 'application/octet-stream'; $headers['X-GitHub-Api-Version'] = '2022-11-28' }
            Invoke-WebRequest -Uri $url -Headers $headers -OutFile $partial -TimeoutSec 20
            if ((Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash -ine $Sha256) { throw 'Pinned download hash mismatch' }
            Move-Item -LiteralPath $partial -Destination $file -Force
            return $file
        } catch {
            # No unverified executable is used, even if an endpoint responds 200.
            Write-Host "Pinned $Name endpoint $index unavailable or hash mismatch"
        } finally { Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue }
    }
    throw "Could not obtain hash-verified $Name from its official endpoints"
}
$archive = Get-PinnedDownload 'nsis-3.13.zip' 'ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8' @(
    'https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.13/nsis-3.13.zip'
    'https://api.github.com/repos/tauri-apps/binary-releases/releases/assets/623832696'
    'https://downloads.sourceforge.net/project/nsis/NSIS%203/3.13/nsis-3.13.zip'
)
$plugin = Get-PinnedDownload 'nsis_tauri_utils-v0.5.3.dll' '5ba143b5db4a87d32d6e7802e033330aae56cbceabe0d1e3ba41948385ad4709' @(
    'https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll'
    'https://api.github.com/repos/tauri-apps/nsis-tauri-utils/releases/assets/345882097'
)
$stage = Join-Path $env:RUNNER_TEMP ('brows-nsis-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA1).Hash -ine 'DB14F8AF2EA346B786C6AE19343F61AAF5E09B8E') { throw 'Tauri NSIS archive admission hash mismatch' }
    Expand-Archive -LiteralPath $archive -DestinationPath $stage
    $nsis = Join-Path $stage 'nsis-3.13'
    if ((Get-FileHash -LiteralPath (Join-Path $nsis 'NSIS.exe') -Algorithm SHA1).Hash -ine '6D45D4E8CCF18FE1FDAC9164058F356102A2ECF2') { throw 'Tauri NSIS executable admission hash mismatch' }
    if ((Get-FileHash -LiteralPath $plugin -Algorithm SHA1).Hash -ine '75197FEE3C6A814FE035788D1C34EAD39349B860') { throw 'Tauri plugin admission hash mismatch' }
    $plugins = Join-Path $nsis 'Plugins/x86-unicode/additional'
    New-Item -ItemType Directory -Path $plugins -Force | Out-Null
    Copy-Item -LiteralPath $plugin -Destination (Join-Path $plugins 'nsis_tauri_utils.dll')
    $tauri = Join-Path $env:LOCALAPPDATA 'tauri'
    New-Item -ItemType Directory -Path $tauri -Force | Out-Null
    $target = Join-Path $tauri 'NSIS'
    if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
    Move-Item -LiteralPath $nsis -Destination $target
    Write-Host 'Prepared hash-verified NSIS 3.13 and Tauri plugin 0.5.3 before compilation'
} finally { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
