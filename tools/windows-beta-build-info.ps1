param(
    [Parameter(Mandatory)][string]$Application,
    [Parameter(Mandatory)][ValidatePattern('^[a-f0-9]{40}$')][string]$Commit,
    [Parameter(Mandatory)][string]$Report,
    [ValidateRange(5, 60)][int]$TimeoutSeconds = 45
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:OS -ne 'Windows_NT') { throw 'Hosted Windows build metadata only' }
$app = (Resolve-Path -LiteralPath $Application).Path
$directory = Join-Path $env:RUNNER_TEMP ('brows-signature-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
$probeReport = Join-Path $directory 'signature.json'
$result = [ordered]@{
    commit = $Commit; channel = 'internal-beta'; autoUpdate = $false
    authenticode = 'Indeterminate'; authenticodeChecked = $false; authenticodeProbe = 'failed'
}
$process = $null
function Quote-Value([string]$Value) { return "'" + $Value.Replace("'", "''") + "'" }
try {
    # Authenticode can wait for OS/network trust providers. This beta metadata
    # query has no authority to admit a browser runtime or publish a release.
    $child = @"
`$ErrorActionPreference = 'Stop'
`$result = @{ checked = `$false; status = 'Indeterminate' }
try {
    `$signature = Get-AuthenticodeSignature -LiteralPath $(Quote-Value $app)
    `$result = @{ checked = `$true; status = [string]`$signature.Status }
} catch { }
`$result | ConvertTo-Json | Set-Content -LiteralPath $(Quote-Value $probeReport) -Encoding UTF8
"@
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($child))
    # Reuse this host's executable/module environment for the isolated query.
    $hostExe = (Get-Process -Id $PID).Path
    $process = Start-Process -FilePath $hostExe -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) -PassThru -WindowStyle Hidden
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $result.authenticodeProbe = 'timeout'
        $process.Kill()
        if (-not $process.WaitForExit(5000)) { throw 'Owned signature metadata process did not exit' }
    } elseif ($process.ExitCode -eq 0 -and (Test-Path -LiteralPath $probeReport)) {
        $probe = [IO.File]::ReadAllText($probeReport) | ConvertFrom-Json
        if ($probe.checked -eq $true -and $probe.status -in @('Valid', 'NotSigned', 'HashMismatch', 'NotTrusted', 'NotSupportedFileFormat', 'Incompatible', 'UnknownError')) {
            $result.authenticode = $probe.status
            $result.authenticodeChecked = $true
            $result.authenticodeProbe = 'completed'
        }
    }
    if (-not $result.authenticodeChecked) { Write-Output 'Beta signature metadata unavailable; recorded Indeterminate, not a verified signature.' }
} finally {
    if ($process -and -not $process.HasExited) { $process.Kill() }
    $result | ConvertTo-Json | Set-Content -LiteralPath $Report -Encoding UTF8
    Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
}
