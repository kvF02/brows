param(
    [string] $Report = 'target/authenticated-proxy-acceptance.json',
    [string] $Protocols = 'both',
    [string] $ProbeExecutable = 'target/debug/examples/public_proxy_probe.exe'
)
$ErrorActionPreference = 'Stop'

function Invoke-ProbeProcess {
    param([string] $Program, [string[]] $Arguments, [string] $InputText = '', [hashtable] $Variables = @{}, [int] $TimeoutSeconds = 20)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Program
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    foreach ($name in @('BROWS_PROXY_ACCEPTANCE_USERNAME', 'BROWS_PROXY_ACCEPTANCE_PASSWORD', 'BROWS_PROXY_PROBE_USERNAME', 'BROWS_PROXY_PROBE_PASSWORD')) { $null = $start.Environment.Remove($name) }
    foreach ($name in $Variables.Keys) { $start.Environment[$name] = $Variables[$name] }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { throw 'Unable to start probe process' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        $finished = $process.WaitForExit($TimeoutSeconds * 1000)
        if (-not $finished) {
            try { $process.Kill($true) } catch { if (-not $process.HasExited) { throw } }
            $process.WaitForExit()
        }
        $code = if ($finished) { $process.ExitCode } else { 124 }
        return @{ code = $code; stdout = $stdout.GetAwaiter().GetResult(); stderr = $stderr.GetAwaiter().GetResult() }
    } finally { $process.Dispose() }
}

function Quote-CurlConfig([string] $Value) { return '"' + $Value.Replace('\', '\\').Replace('"', '\"') + '"' }
function New-CurlProbeConfig([string] $ProxyUrl = '', [string] $Username = '', [string] $Password = '') {
    $lines = @('url = "https://checkip.amazonaws.com/"', 'silent', 'show-error', 'fail', 'connect-timeout = 5', 'max-time = 15', 'max-filesize = 128', 'write-out = "\nBROWS_CURL_STATUS:%{http_connect}:%{http_code}\n"')
    if ($ProxyUrl) {
        $lines += 'proxy = ' + (Quote-CurlConfig $ProxyUrl)
        $lines += 'proxy-user = ' + (Quote-CurlConfig ($Username + ':' + $Password))
        $lines += 'noproxy = ""'
    }
    return ($lines -join "`n") + "`n"
}

function Read-ProbeIp([string] $Text) {
    $ip = $null
    if ([Net.IPAddress]::TryParse($Text.Trim(), [ref] $ip)) { return $ip.ToString() }
    return $null
}
function Convert-CurlProbeResult([hashtable] $Result) {
    $connectStatus = 0; $httpStatus = 0; $exitIp = $null
    if ($Result.stdout -match '(?s)^(.*?)\r?\nBROWS_CURL_STATUS:(\d{3}):(\d{3})\r?\n?$') {
        $exitIp = Read-ProbeIp $Matches[1]
        $connectStatus = [int] $Matches[2]; $httpStatus = [int] $Matches[3]
    }
    $passed = $Result.code -eq 0 -and $httpStatus -eq 200 -and $null -ne $exitIp
    $stage = if ($passed) { 'verified_https_exit' }
        elseif ($connectStatus -eq 407 -or $Result.code -eq 67 -or $Result.stderr -match '(?i)(user was rejected|authentication failed|no authentication method was acceptable)') { 'proxy_authentication_rejected' }
        elseif ($Result.code -in @(51, 60)) { 'tls_certificate_rejected' }
        elseif ($Result.code -eq 35) { 'tls_handshake_failed' }
        elseif ($Result.code -eq 7) { 'proxy_connect_failed' }
        elseif ($Result.code -in @(28, 124)) { 'timeout_stage_unknown' }
        elseif ($Result.code -eq 97) { 'proxy_protocol_failed' }
        else { 'https_status_or_exit_ip_failed' }
    return [ordered]@{ exitCode = $Result.code; stage = $stage; connectStatus = $connectStatus; httpStatus = $httpStatus; exitIp = $exitIp; passed = $passed }
}
function Convert-AdapterProbeResult([hashtable] $Result) {
    $output = ([string] $Result.stdout).Trim()
    $exitIp = $null; $kind = $null
    if ($Result.code -eq 0 -and $output -match '^OK\|true\|true\|([0-9a-fA-F:.]+)$') { $exitIp = Read-ProbeIp $Matches[1] }
    if ($output -match '^ERROR\|(ProxyAuth|ProxyConnect|InvalidData|CapabilityUnsupported|Unavailable)\|') { $kind = $Matches[1] }
    $stage = if ($exitIp) { 'verified_https_exit' }
        elseif ($Result.code -eq 124) { 'timeout_stage_unknown' }
        elseif ($kind -eq 'ProxyAuth') { 'authentication_or_proxy_policy_rejected' }
        elseif ($output -match '^ERROR\|ProxyConnect\|proxy TLS ') { 'tls_verification_or_handshake_failed' }
        elseif ($kind -eq 'InvalidData') { 'invalid_exit_response' }
        else { 'proxy_or_https_request_failed' }
    return [ordered]@{ exitCode = $Result.code; stage = $stage; errorKind = $kind; exitIp = $exitIp; passed = $null -ne $exitIp }
}

function Invoke-AuthenticatedProxyAcceptance {
    $evidence = [ordered]@{ schemaVersion = 1; status = 'not_checked'; commit = $env:GITHUB_SHA; capturedAt = [DateTimeOffset]::UtcNow.ToString('o'); endpoint = 'https://checkip.amazonaws.com/'; checks = @(); allRequestedChecksPassed = $false; notChecked = @('browser session exit', 'DNS and ICE', 'Windows 10/11 clients', 'profile persistence') }
    $reportPath = [IO.Path]::GetFullPath($Report)
    New-Item -ItemType Directory -Force -Path (Split-Path $reportPath -Parent) | Out-Null
    try {
        if (-not $IsWindows) { throw 'Windows required' }
        $username = $env:BROWS_PROXY_ACCEPTANCE_USERNAME
        $password = $env:BROWS_PROXY_ACCEPTANCE_PASSWORD
        if ([string]::IsNullOrWhiteSpace($username) -or -not $password) { $evidence.status = 'blocked_missing_credentials'; throw 'Missing credentials' }
        if ($username.Contains(':') -or $username -match '[\x00-\x1f\x7f]' -or $password -match '[\x00-\x1f\x7f]' -or [Text.Encoding]::UTF8.GetByteCount($username) -gt 255 -or [Text.Encoding]::UTF8.GetByteCount($password) -gt 255) { throw 'Invalid credential format' }
        $addresses = @(([string] $env:BROWS_PROXY_ENDPOINTS).Split(',') | ForEach-Object { $_.Trim() })
        if ($addresses.Count -lt 1 -or $addresses.Count -gt 4) { throw 'Provide 1 to 4 proxy addresses' }
        $targets = @()
        foreach ($address in $addresses) {
            if ($address -notmatch '^([0-9.]+):([0-9]{1,5})$') { throw 'Expected numeric IPv4:port without credentials' }
            $ip = $null; $proxyPort = [int] $Matches[2]
            if (-not [Net.IPAddress]::TryParse($Matches[1], [ref] $ip) -or $ip.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork -or $proxyPort -lt 1 -or $proxyPort -gt 65535) { throw 'Invalid proxy endpoint' }
            $targets += @{ host = $ip.ToString(); port = $proxyPort }
        }
        $schemes = switch ($Protocols) { 'both' { @('http', 'socks5') }; 'http' { @('http') }; 'socks5' { @('socks5') }; default { throw 'Unknown proxy protocol' } }
        $probe = (Resolve-Path -LiteralPath $ProbeExecutable).Path
        $curl = (Get-Command curl.exe).Source
        $baseline = Invoke-ProbeProcess -Program $curl -Arguments @('-q', '--config', '-') -InputText (New-CurlProbeConfig)
        $evidence.runnerDefaultHttps = Convert-CurlProbeResult $baseline
        foreach ($index in 0..($targets.Count - 1)) {
            $target = $targets[$index]
            $tcp = [Net.Sockets.TcpClient]::new()
            $tcpStatus = 'failed'
            try { if ($tcp.ConnectAsync($target.host, $target.port).Wait(5000)) { $tcpStatus = 'connected' } else { $tcpStatus = 'timeout' } } catch { $tcpStatus = 'failed' } finally { $tcp.Dispose() }
            foreach ($scheme in $schemes) {
                $row = [ordered]@{ proxy = 'proxy-' + ($index + 1); scheme = $scheme; tcp = $tcpStatus; curl = $null; adapter = $null; passed = $false }
                if ($tcpStatus -eq 'connected') {
                    $curlScheme = if ($scheme -eq 'socks5') { 'socks5h' } else { 'http' }
                    $configuration = New-CurlProbeConfig -ProxyUrl "${curlScheme}://$($target.host):$($target.port)" -Username $username -Password $password
                    $curlResult = Invoke-ProbeProcess -Program $curl -Arguments @('-q', '--config', '-') -InputText $configuration
                    $row.curl = Convert-CurlProbeResult $curlResult
                    $adapterResult = Invoke-ProbeProcess -Program $probe -Arguments @($scheme, $target.host, [string] $target.port) -Variables @{ BROWS_PROXY_PROBE_USERNAME = $username; BROWS_PROXY_PROBE_PASSWORD = $password }
                    $row.adapter = Convert-AdapterProbeResult $adapterResult
                    $row.passed = $row.curl.passed -and $row.adapter.passed
                    $row.sameExitIp = $row.curl.exitIp -and $row.curl.exitIp -eq $row.adapter.exitIp
                }
                $evidence.checks += [pscustomobject] $row
            }
        }
        $evidence.allRequestedChecksPassed = @($evidence.checks | Where-Object { -not $_.passed }).Count -eq 0
        $evidence.status = if ($evidence.allRequestedChecksPassed) { 'passed' } else { 'failed' }
    } catch {
        if ($evidence.status -eq 'not_checked') { $evidence.status = 'configuration_or_runner_error' }
    } finally {
        # Only allowlisted result fields leave memory; never write raw child
        # stderr/stdout, proxy credentials or curl configuration into reports.
        $evidence | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath
    }
    if (-not $evidence.allRequestedChecksPassed) { throw 'Proxy acceptance incomplete or failed; inspect the sanitized report' }
    Write-Output 'All requested authenticated proxy checks passed'
}
if ($MyInvocation.InvocationName -ne '.') { Invoke-AuthenticatedProxyAcceptance }
