param([Parameter(Mandatory)][string]$KitDirectory, [Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:OS -ne 'Windows_NT') {
    throw 'Disposable hosted Windows fixture only'
}
. "$PSScriptRoot/client-acceptance/support.ps1"
$result = [ordered]@{
    schemaVersion = 1; evidenceType = 'automated-hosted-client'
    assertionsPassed = $false; complete = $false
    clientPackageChecksComplete = $false
    commit = $null; platform = $null; applicationArchitecture = $null
    hardware = 'hosted-virtual-machine'; x64Emulation = $false
    package = $null; failure = $null
    notChecked = @('Windows 10', 'physical hardware', 'business website login', 'SeedableNoise capability on ARM64', 'full UI workflows on ARM64')
}
$phase = 'client-platform'
try {
    $result.platform = Get-BrowsClientPlatform
    if (-not $result.platform.supportedTestTarget -or $result.platform.family -ne 'Windows 11' -or $result.platform.nativeArchitecture -ne 'ARM64') {
        throw 'This job must run on the Windows 11 ARM64 desktop client image'
    }
    $phase = 'package-integrity'
    $kit = Test-BrowsKit $KitDirectory
    $result.commit = $kit.commit
    $result.applicationArchitecture = Get-BrowsPeMachine (Join-Path $KitDirectory 'brows-desktop.exe')
    if ($result.applicationArchitecture -ne 'x64') { throw 'Expected the same x64 beta package' }
    $result.x64Emulation = $true
    $phase = 'installed-application'
    $packageReport = "$Report.package.json"
    & "$PSScriptRoot/windows-beta-smoke.ps1" -Installer (Join-Path $KitDirectory $kit.installerName) -Report $packageReport
    $result.package = [IO.File]::ReadAllText($packageReport) | ConvertFrom-Json
    if (-not $result.package.assertionsPassed -or -not $result.package.packageChecksComplete -or $result.package.installedApplicationSha256 -cne $kit.installedSha256) {
        throw 'Installed package acceptance incomplete or application hash mismatch'
    }
    $result.assertionsPassed = $true
    $result.clientPackageChecksComplete = $true
} catch {
    $result.failure = @{ phase = $phase; message = $_.Exception.Message }
    throw
} finally {
    $result | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath $Report -Encoding utf8
}
