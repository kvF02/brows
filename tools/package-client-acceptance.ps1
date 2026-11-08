param([Parameter(Mandatory)][string]$ArtifactRoot, [Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/client-acceptance/support.ps1"
$installers = @(Get-ChildItem -LiteralPath (Join-Path $ArtifactRoot 'release/bundle/nsis') -Filter '*-setup.exe')
if ($installers.Count -ne 1) { throw 'Expected one installer for client kit' }
$stage = Join-Path ([IO.Path]::GetTempPath()) ('brows-client-kit-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Copy-Item -LiteralPath (Join-Path $ArtifactRoot 'release/brows-desktop.exe') -Destination $stage
    Copy-Item -LiteralPath $installers[0].FullName -Destination $stage
    foreach ($name in @('SHA256SUMS.txt', 'beta-build.json')) {
        Copy-Item -LiteralPath (Join-Path $ArtifactRoot $name) -Destination $stage
    }
    foreach ($name in @('run.ps1', 'support.ps1', 'README.md')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot "client-acceptance/$name") -Destination $stage
    }
    $kit = Test-BrowsKit $stage
    if ((Get-BrowsPeMachine (Join-Path $stage 'brows-desktop.exe')) -ne 'x64') { throw 'Client kit portable executable must be x64' }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $Destination -Force
    Write-Output "Client kit ready for commit $($kit.commit)"
} finally {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
