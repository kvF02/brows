$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/windows-authenticated-proxy-acceptance.ps1"
function Assert-Probe([bool] $Condition, [string] $Message) { if (-not $Condition) { throw $Message } }
$user = 'fixture-user'
$password = 'fixture-"p\ass:word'
$config = New-CurlProbeConfig -ProxyUrl 'http://127.0.0.1:1234' -Username $user -Password $password
Assert-Probe ($config.Contains('proxy-user = "fixture-user:fixture-\"p\\ass:word"')) 'curl configuration must preserve quote, backslash and colon'
Assert-Probe ($config.Contains('noproxy = ""')) 'Explicit proxy requests must not honor direct bypass entries'
$program = (Get-Command pwsh).Source
$oldParentPassword = $env:BROWS_PROXY_ACCEPTANCE_PASSWORD
$directory = Join-Path $env:RUNNER_TEMP ('brows-proxy-driver-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
try {
    $env:BROWS_PROXY_ACCEPTANCE_PASSWORD = 'parent-secret-not-for-child'
    $command = @'
$password = $env:BROWS_PROXY_PROBE_PASSWORD
if (-not $password) { exit 10 }
if ([Environment]::CommandLine.Contains($password)) { exit 11 }
if ($env:BROWS_PROXY_ACCEPTANCE_PASSWORD) { exit 12 }
$configuration = [Console]::In.ReadToEnd()
$escaped = $password.Replace('\', '\\').Replace('"', '\"')
if (-not $configuration.Contains($escaped)) { exit 13 }
[Console]::Out.Write('safe-result')
'@
    $process = Invoke-ProbeProcess -Program $program -Arguments @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', $command) -InputText $config -Variables @{ BROWS_PROXY_PROBE_USERNAME = $user; BROWS_PROXY_PROBE_PASSWORD = $password }
    Assert-Probe ($process.code -eq 0 -and $process.stdout -eq 'safe-result') 'Secret must reach stdin/environment without entering argv or inheriting unrelated credential variables'
    $ok = Convert-CurlProbeResult @{ code = 0; stdout = "203.0.113.9`n`nBROWS_CURL_STATUS:200:200`n"; stderr = '' }
    Assert-Probe ($ok.passed -and $ok.exitIp -eq '203.0.113.9') 'A verified HTTP 200 IP response should pass'
    $auth = Convert-CurlProbeResult @{ code = 56; stdout = "`nBROWS_CURL_STATUS:407:000`n"; stderr = "authentication failed $password" }
    Assert-Probe (-not $auth.passed -and $auth.stage -eq 'proxy_authentication_rejected') 'Explicit CONNECT 407 must be distinguished from transport failure'
    $transport = Convert-CurlProbeResult @{ code = 7; stdout = ''; stderr = "connect failed $password" }
    Assert-Probe ($transport.stage -eq 'proxy_connect_failed') 'TCP failure must not be attributed to account credentials'
    $tls = Convert-CurlProbeResult @{ code = 60; stdout = ''; stderr = $password }
    Assert-Probe ($tls.stage -eq 'tls_certificate_rejected') 'Certificate rejection must remain a failure'
    $adapter = Convert-AdapterProbeResult @{ code = 1; stdout = "ERROR|ProxyAuth|$password"; stderr = $password }
    Assert-Probe (-not $adapter.passed -and $adapter.stage -eq 'authentication_or_proxy_policy_rejected') 'Broad adapter rejection must keep its uncertainty'
    foreach ($item in @($auth, $transport, $tls, $adapter)) {
        $json = $item | ConvertTo-Json
        Assert-Probe (-not $json.Contains($password) -and -not $json.Contains($user)) 'Raw child output must never reach the diagnostic report'
    }
    $timeout = Invoke-ProbeProcess -Program $program -Arguments @('-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 10') -TimeoutSeconds 1
    Assert-Probe ($timeout.code -eq 124) 'A stalled child must be terminated with a timeout result'
    $report = Join-Path $directory 'blocked.json'
    $blocked = Invoke-ProbeProcess -Program $program -Arguments @('-NoProfile', '-NonInteractive', '-File', "$PSScriptRoot/windows-authenticated-proxy-acceptance.ps1", '-Report', $report)
    $evidence = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
    Assert-Probe ($blocked.code -ne 0 -and $evidence.status -eq 'blocked_missing_credentials' -and -not $evidence.allRequestedChecksPassed) 'Missing credentials must never produce successful acceptance'
    Write-Output 'Proxy driver security, classification and timeout tests passed; no live proxy was tested'
} finally {
    $env:BROWS_PROXY_ACCEPTANCE_PASSWORD = $oldParentPassword
    Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
}
