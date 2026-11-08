param(
    [Parameter(Mandatory)][string]$Application,
    [ValidatePattern('^[A-Za-z0-9_-]{1,40}$')][string]$SiteAlias = 'test-site',
    [ValidateSet('physical', 'virtual', 'unknown')][string]$Hardware = 'unknown',
    [string]$Report = (Join-Path $PSScriptRoot 'client-acceptance.json')
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/support.ps1"
$checks = [System.Collections.Generic.List[object]]::new()
$result = [ordered]@{
    schemaVersion = 1; evidenceType = 'operator-attestation'
    status = 'incomplete'; complete = $false; automatedChecksPassed = $false
    operatorAccepted = $false; createdUtc = [DateTime]::UtcNow.ToString('o')
    commit = $null; platform = $null; applicationArchitecture = $null
    hardwareReportedByOperator = $Hardware; siteAlias = $SiteAlias
    checks = $checks; failureCode = $null
    notChecked = @('site account policy guarantees', 'cross-Windows-user restore', 'physical power loss', 'TURN')
}
function Save-Report {
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Report -Encoding UTF8
}
function Confirm-Step([string]$Id, [string]$Instruction) {
    Write-Host "`n[$Id] $Instruction"
    do { $answer = Read-Host 'Result: y=passed, n=failed, s=not checked' } while ($answer -notin @('y', 'n', 's'))
    $value = @{ y = 'pass'; n = 'fail'; s = 'not-checked' }[$answer]
    $checks.Add([pscustomobject]@{ id = $Id; source = 'operator'; result = $value })
    $result.status = Get-BrowsOperatorStatus @($checks.ToArray())
    Save-Report
}
try {
    Save-Report
    $kit = Test-BrowsKit $PSScriptRoot
    $result.commit = $kit.commit
    $result.platform = Get-BrowsClientPlatform
    if (-not $result.platform.supportedTestTarget) { throw 'Unsupported client test target' }
    $app = (Resolve-Path -LiteralPath $Application).Path
    if ([IO.Path]::GetFileName($app) -ine 'brows-desktop.exe' -or (Get-BrowsSha256 $app) -cne $kit.installedSha256) { throw 'Installed application hash does not match this kit' }
    $result.applicationArchitecture = Get-BrowsPeMachine $app
    if ($result.applicationArchitecture -ne 'x64') { throw 'This kit expects the x64 beta package' }
    $result.automatedChecksPassed = $true
    Save-Report
    Write-Host 'Preflight passed. Use dedicated test profiles and an authorized test account.'
    Write-Host 'Enter credentials only in the website. This script never installs, starts, kills or uninstalls Brows.'
    Confirm-Step 'native-login' 'In Brows, create SystemNative profile A. Log into your HTTPS test site. Confirm the account and a harmless saved test marker.'
    Confirm-Step 'native-isolation' 'Open the same site in a new independent SystemNative profile B. Confirm A login and saved marker did not appear in B.'
    Confirm-Step 'native-reopen' 'Normally stop A from Brows, start A again, and revisit the site without logging in again. Confirm the same account and marker.'
    Confirm-Step 'native-app-restart' 'Normally stop all sessions and exit Brows. Open the installed app again, start A, and confirm account and marker without logging in again.'
    Confirm-Step 'native-backup' 'Stop A. Export an encrypted backup. Restore it under the same Windows user into a NEW profile. Confirm its account and marker without logging in again; confirm A still exists.'
    Confirm-Step 'backup-wrong-password' 'Attempt a second restore using a wrong backup password. Confirm rejection and no usable half-restored profile.'
    Confirm-Step 'seeded-capability' 'Register a trusted managed runtime and pass its capability probe. Create a separate SeedableNoise test profile with a fixed seed and runtime.'
    Confirm-Step 'seeded-login-isolation' 'Log into the test site in that SeedableNoise profile and save a marker. Confirm a separate new SeedableNoise profile has neither login nor marker.'
    Confirm-Step 'seeded-reopen' 'Normally stop and reopen the logged-in SeedableNoise profile. Confirm the account and marker without logging in again.'
    Confirm-Step 'seeded-app-restart' 'Normally stop sessions and exit Brows. Open it again, re-run the runtime capability probe, then open the original SeedableNoise profile. Confirm account and marker.'
    Confirm-Step 'seeded-backup' 'Stop the SeedableNoise profile. Export and restore an encrypted backup into a NEW profile under this Windows user. Confirm account and marker without logging in again; original profile remains present.'
    $result.complete = @($checks | Where-Object { $_.result -eq 'not-checked' }).Count -eq 0
    $result.status = Get-BrowsOperatorStatus @($checks.ToArray())
    $result.operatorAccepted = $result.complete -and $result.status -eq 'operator-accepted'
    Save-Report
    Write-Host "Saved report. Status: $($result.status). This is operator evidence, not automated website verification."
} catch {
    $result.status = 'incomplete'
    # Do not persist exception text: paths can contain the Windows account name.
    $result.failureCode = 'preflight-or-report-incomplete'
    Save-Report
    throw 'Acceptance did not complete. Check the kit, installed executable and supported Windows client; no website secrets are collected.'
} finally {
    Save-Report
}
