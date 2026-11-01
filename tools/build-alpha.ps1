#Requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'Build Alpha on Windows x64 with MSVC, Node 24+, Rust 1.97.1 and WebView2.' }
if (-not [Environment]::Is64BitOperatingSystem) { throw 'Windows x64 is required.' }

$repoRoot = Split-Path -Parent $PSScriptRoot
$oldLocation = Get-Location
$previousConfig = $env:TAURI_CONFIG
$previousTarget = $env:CARGO_TARGET_DIR
function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    # Native tools use stderr for progress too. Check their exit code instead
    # of treating progress output as a terminating Windows PowerShell error.
    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $Program @Arguments | Out-Host
        $exitCode = $LASTEXITCODE
    } finally { $ErrorActionPreference = $previousPreference }
    if ($exitCode -ne 0) { throw "$Program exited with code $exitCode" }
}
function Get-SourceCommit {
    $value = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $value -notmatch '^[0-9a-f]{40}$') { throw 'Build from a Git checkout with a committed source revision.' }
    return $value.Trim()
}
function Assert-CleanSource {
    param([switch]$AfterBuild)
    $changes = @(& git diff --name-only HEAD)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect source changes.' }
    $untracked = @(& git ls-files --others --exclude-standard)
    if ($LASTEXITCODE -ne 0 -or $untracked.Count) { throw 'Commit source changes before producing a traceable Alpha package.' }
    $generated = @()
    if ($AfterBuild) {
        # Tauri regenerates these documentation schemas for the actual target.
        # They are not runtime permission inputs. Preserve them on disk; never
        # ignore changes to source capabilities, ACL definitions or other files.
        $generated = @('crates/desktop/gen/schemas/desktop-schema.json', 'crates/desktop/gen/schemas/windows-schema.json')
    }
    $unexpected = @($changes | Where-Object { $generated -notcontains $_ })
    if ($unexpected.Count) { throw 'Commit source changes before producing a traceable Alpha package.' }
}
try {
    Set-Location $repoRoot
    foreach ($command in @('git', 'node', 'npm.cmd', 'cargo')) { Get-Command $command -ErrorAction Stop | Out-Null }
    $nodeVersion = & node --version
    if ($LASTEXITCODE -ne 0 -or $nodeVersion -notmatch '^v(\d+)\.' -or [int]$Matches[1] -lt 24) { throw 'Node.js 24 or newer is required.' }
    Assert-CleanSource
    $commit = Get-SourceCommit
    $env:TAURI_CONFIG = Get-Content (Join-Path $repoRoot 'crates/desktop/tauri.alpha.conf.json') -Raw
    $env:CARGO_TARGET_DIR = Join-Path $repoRoot 'target/alpha-build'

    Invoke-Checked -Program 'npm.cmd' -Arguments @('ci', '--prefix', 'ui')
    Invoke-Checked -Program 'npm.cmd' -Arguments @('run', 'ui:build')
    foreach ($suite in @('test:fingerprint', 'test:settings', 'test:backups', 'test:runtimes', 'test:alpha')) {
        Invoke-Checked -Program 'npm.cmd' -Arguments @('run', $suite)
    }
    Invoke-Checked -Program 'cargo' -Arguments @('+1.97.1', 'fmt', '--all', '--', '--check')
    Invoke-Checked -Program 'cargo' -Arguments @('+1.97.1', 'test', '--locked', '--workspace', '--target', 'x86_64-pc-windows-msvc')
    Invoke-Checked -Program 'cargo' -Arguments @('+1.97.1', 'clippy', '--locked', '--workspace', '--all-targets', '--target', 'x86_64-pc-windows-msvc', '--', '-D', 'warnings')
    Invoke-Checked -Program 'cargo' -Arguments @('+1.97.1', 'build', '--locked', '--release', '-p', 'brows-desktop', '--features', 'custom-protocol', '--target', 'x86_64-pc-windows-msvc')
    Assert-CleanSource -AfterBuild
    if ((Get-SourceCommit) -ne $commit) { throw 'Source revision changed during the build.' }

    $executable = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc/release/brows-desktop.exe'
    $result = & node (Join-Path $repoRoot 'tools/package-alpha.mjs') $executable $commit
    if ($LASTEXITCODE -ne 0) { throw 'Alpha packaging failed.' }
    $package = $result | ConvertFrom-Json
    $zip = $package.directory + '.zip'
    Compress-Archive -Path (Join-Path $package.directory '*') -DestinationPath $zip -CompressionLevel Optimal
    $zipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText($zip + '.sha256', $zipHash + '  ' + [IO.Path]::GetFileName($zip) + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host "Alpha package: $zip"
    [pscustomobject]@{ Directory = $package.directory; Zip = $zip; ArtifactName = $package.artifactName; Commit = $commit }
} finally {
    $env:TAURI_CONFIG = $previousConfig
    $env:CARGO_TARGET_DIR = $previousTarget
    Set-Location $oldLocation
}
