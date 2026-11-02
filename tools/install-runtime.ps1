# Explicit Windows-only installer; no startup downloads and no caller URLs.
[CmdletBinding()]
param(
    [string]$DataRoot = (Join-Path $env:LOCALAPPDATA 'local-brows-desktop'),
    [string]$Version = ''
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [Environment]::Is64BitProcess -or $env:OS -ne 'Windows_NT') { throw 'Windows x64 PowerShell is required' }
# The channel metadata is fetched on every explicit install. Older pinned builds
# are intentionally rejected rather than offering a vulnerable downgrade path.
$metadataUri = 'https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json'
$metadata = Invoke-RestMethod -Uri $metadataUri -MaximumRedirection 0 -TimeoutSec 60
$stable = [string]$metadata.channels.Stable.version
if ($stable -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw 'Invalid official Stable version' }
if ($Version -and $Version -ne $stable) { throw 'Only the current official Stable version may be installed' }
$Version = $stable
$url = "https://storage.googleapis.com/chrome-for-testing-public/$Version/win64/chrome-win64.zip"
$asset = @($metadata.channels.Stable.downloads.chrome | Where-Object { $_.platform -eq 'win64' })
if ($asset.Count -ne 1 -or $asset[0].url -cne $url) { throw 'Unexpected official asset URL' }
$runtimeRoot = Join-Path ([IO.Path]::GetFullPath($DataRoot)) 'runtimes'
[IO.Directory]::CreateDirectory($runtimeRoot) | Out-Null
function Assert-NoLinks([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force
    while ($item) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse points are not allowed in runtime paths' }
        $item = if ($item -is [IO.FileInfo]) { $item.Directory } else { $item.Parent }
    }
}
Assert-NoLinks $runtimeRoot
$destination = Join-Path $runtimeRoot $Version
if (Test-Path -LiteralPath $destination) { throw 'This version already exists; existing installations are never overwritten' }
$staging = Join-Path $runtimeRoot ('.staging-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($staging) | Out-Null
$archive = Join-Path $staging 'download.zip'
$package = Join-Path $staging 'package'
[IO.Directory]::CreateDirectory($package) | Out-Null
try {
    Invoke-WebRequest -Uri $url -OutFile $archive -MaximumRedirection 0 -TimeoutSec 600
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        if ($zip.Entries.Count -gt 10000) { throw 'Too many archive entries' }
        [long]$total = 0
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            if ($name -notmatch '^chrome-win64/' -or $name.Contains('\') -or $name.Contains(':') -or $name -match '(^|/)\.\.?(/|$)' -or -not $names.Add($name)) { throw 'Unsafe or duplicate archive entry' }
            if ((($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'Archive links are not allowed' }
            $total += $entry.Length
            if ($total -gt 4GB) { throw 'Expanded runtime is too large' }
        }
    } finally { $zip.Dispose() }
    [IO.Compression.ZipFile]::ExtractToDirectory($archive, $package)
    $executable = Join-Path $package 'chrome-win64/chrome.exe'
    $signature = Get-AuthenticodeSignature -LiteralPath $executable
    $publisher = if ($signature.SignerCertificate) { $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) } else { '' }
    if ($signature.Status -ne 'Valid' -or $publisher -cne 'Google LLC') { throw 'Chrome Authenticode trust or publisher check failed' }
    $fileVersion = (Get-Item -LiteralPath $executable).VersionInfo.ProductVersion
    if ($fileVersion -ne $Version) { throw 'Executable product version differs from the selected release' }
    $files = [ordered]@{}
    foreach ($file in Get-ChildItem -LiteralPath $package -Recurse -File -Force) {
        Assert-NoLinks $file.FullName
        $relative = $file.FullName.Substring($package.Length + 1).Replace('\', '/')
        $files[$relative] = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $manifest = [ordered]@{ formatVersion = 1; version = $Version; source = $url; installedAt = [DateTime]::UtcNow.ToString('o'); files = $files }
    [IO.File]::WriteAllText((Join-Path $package 'brows-runtime.json'), ($manifest | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
    # Same-volume directory rename publishes a complete, inventoried version.
    [IO.Directory]::Move($package, $destination)
    Write-Output "Installed $Version at $destination. Restart Brows, select Chrome for Testing, then run the fingerprint capability check."
} finally {
    # Cleanup is confined to this invocation's freshly created staging directory.
    if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force }
}
