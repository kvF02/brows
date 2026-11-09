param([Parameter(Mandatory)][string]$Report)
$ErrorActionPreference = 'Stop'
$reportPath = [IO.Path]::GetFullPath($Report)
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'source-manifest.json') -Raw | ConvertFrom-Json
$result = [ordered]@{ schemaVersion = 1; scope = 'Disposable GitHub-hosted Windows SDK setup only'; readyForBuild = $false; installed = $false; compiled = $false; nativeAcceptancePassed = $false; complete = $false; failure = $null }
$installer = $null

try {
    if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') { throw 'SDK setup is restricted to disposable GitHub-hosted Windows runners' }
    & (Join-Path $PSScriptRoot 'windows-preflight.ps1') -Report "$reportPath.before.json"
    $before = Get-Content -LiteralPath "$reportPath.before.json" -Raw | ConvertFrom-Json
    if ($before.failure) { throw 'Initial build inventory failed' }
    if (-not $before.readyForBuild) {
        if ($before.blockers.Count -ne 1 -or $before.blockers[0] -ne 'required-windows-sdk-not-observed') { throw 'Host has build blockers other than the supported SDK setup' }
        $sdk = $manifest.windowsSdkInstaller
        if ($sdk.url -notmatch '^https://download\.microsoft\.com/' -or $sdk.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid pinned Microsoft SDK installer' }
        $installer = Join-Path $env:RUNNER_TEMP "brows-winsdk-$([Guid]::NewGuid()).exe"
        Invoke-WebRequest -Uri $sdk.url -OutFile $installer -TimeoutSec 120
        $digest = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($digest -ne $sdk.sha256) { throw 'SDK installer digest mismatch' }
        $signature = Get-AuthenticodeSignature -LiteralPath $installer
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -ne 'Microsoft Corporation') { throw 'SDK installer Microsoft signature admission failed' }
        $result.installerSha256 = $digest
        $log = "$reportPath.installer.log"
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($log)) | Out-Null
        # Current SDK bundles use architecture-specific desktop components.
        # The x86 tools are also needed by the x64 Chromium build tooling.
        $process = Start-Process -FilePath $installer -ArgumentList @('/quiet', '/norestart', '/ceip', 'off', '/features', 'OptionId.DesktopCPPx86', 'OptionId.DesktopCPPx64', '/log', "`"$log`"") -PassThru
        if (-not $process.WaitForExit(900000)) { $process.Kill($true); throw 'Owned SDK installer exceeded 15 minutes' }
        $result.installerExitCode = $process.ExitCode
        if ($process.ExitCode -notin @(0, 3010)) { throw "SDK installer failed with exit $($process.ExitCode)" }
        $result.installed = $true
    }
    & (Join-Path $PSScriptRoot 'windows-preflight.ps1') -Report "$reportPath.after.json"
    $after = Get-Content -LiteralPath "$reportPath.after.json" -Raw | ConvertFrom-Json
    if (-not $after.readyForBuild) { throw 'Build prerequisites are still not observed after SDK setup' }
    $result.readyForBuild = $true
} catch {
    $result.failure = $_.Exception.Message
} finally {
    if ($installer -and (Test-Path -LiteralPath $installer)) { Remove-Item -LiteralPath $installer }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($reportPath)) | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding utf8
    $result | ConvertTo-Json -Depth 8 | Write-Output
}
if (-not $result.readyForBuild) { exit 1 }
