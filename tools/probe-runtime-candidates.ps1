#Requires -Version 7.2
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:RUNNER_OS -ne 'Windows' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Run candidate admission only on fresh GitHub-hosted Windows runners.'
}
$repo = Split-Path -Parent $PSScriptRoot
$output = Join-Path $repo 'target/runtime-candidate.json'
New-Item -ItemType Directory -Path (Split-Path -Parent $output) -Force | Out-Null
$report = [ordered]@{
    schemaVersion = 1; scope = 'candidate_only_headless_extension_observation'; commit = $env:GITHUB_SHA
    provider = 'Microsoft Edge'; admitted = $false; hooksObserved = $false; error = $null
}
try {
    $candidates = @(
        "${env:ProgramFiles(x86)}/Microsoft/Edge/Application/msedge.exe",
        "$env:ProgramFiles/Microsoft/Edge/Application/msedge.exe"
    )
    $executable = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $executable) { throw 'The signed system Edge candidate is not installed.' }
    $signature = Get-AuthenticodeSignature -LiteralPath $executable
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.GetNameInfo('SimpleName', $false) -ne 'Microsoft Corporation') {
        throw 'Candidate did not pass Microsoft Authenticode publisher verification.'
    }
    $report.admitted = $true
    $report.version = (Get-Item -LiteralPath $executable).VersionInfo.ProductVersion
    $report.sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    $observation = & node (Join-Path $PSScriptRoot 'fingerprint-browser-smoke.mjs') $executable --diagnostic-matrix
    $exitCode = $LASTEXITCODE
    if ($exitCode -notin @(0, 1)) { throw "Candidate observation did not complete (code $exitCode)." }
    $report.observation = ($observation -join "`n") | ConvertFrom-Json
    $report.hooksObserved = $report.observation.hooksObserved
    if (-not $report.hooksObserved) { throw 'Candidate did not observe all required hooks; production admission is unchanged.' }
} catch {
    $report.error = $_.Exception.Message
    throw
} finally {
    [IO.File]::WriteAllText($output, ($report | ConvertTo-Json -Depth 8) + "`n", [Text.UTF8Encoding]::new($false))
}
