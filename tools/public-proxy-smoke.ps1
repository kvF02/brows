param(
    [Parameter(Mandatory)][string] $Candidates,
    [string] $Report = 'target/public-proxy-smoke.json'
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$items = @(Get-Content -LiteralPath $Candidates -Raw | ConvertFrom-Json)
if ($items.Count -lt 1 -or $items.Count -gt 8) { throw 'Provide 1 to 8 explicit public test proxies' }
$adapter = Join-Path $PWD 'target/debug/examples/public_proxy_probe.exe'
if (-not (Test-Path -LiteralPath $adapter)) { throw 'Build the public_proxy_probe example first' }
$reportPath = [IO.Path]::GetFullPath($Report)
New-Item -ItemType Directory -Force -Path (Split-Path $reportPath -Parent) | Out-Null
$scratch = Join-Path $env:RUNNER_TEMP ('brows-public-proxy-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
function Read-PublicIp([string] $Text) {
    $parsed = $null
    if ([Net.IPAddress]::TryParse($Text.Trim(), [ref] $parsed)) { return $parsed.ToString() }
    return $null
}
try {
    $baselineBody = Join-Path $scratch 'baseline.txt'
    & curl.exe --silent --show-error --fail --connect-timeout 5 --max-time 12 --max-filesize 128 --output $baselineBody 'https://ipv4.webshare.io/'
    $baselineCode = $LASTEXITCODE
    $baselineIp = if (Test-Path $baselineBody) { Read-PublicIp (Get-Content $baselineBody -Raw) } else { $null }
    if ($baselineCode -ne 0 -or -not $baselineIp) { throw 'Direct HTTPS IP baseline failed' }
    $results = @()
    foreach ($item in $items) {
        $address = $null
        if ($item.scheme -notin @('http', 'socks5') -or -not [Net.IPAddress]::TryParse([string] $item.host, [ref] $address) -or $address.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork -or [int] $item.port -lt 1 -or [int] $item.port -gt 65535) {
            throw 'Each candidate must specify http/socks5, a numeric IPv4 and a valid port; credentials are unsupported'
        }
        $hostIp = $address.ToString()
        $port = [int] $item.port
        $curlScheme = if ($item.scheme -eq 'socks5') { 'socks5h' } else { 'http' }
        $proxy = "${curlScheme}://${hostIp}:$port"
        $body = Join-Path $scratch 'ip.txt'
        $err = Join-Path $scratch 'curl-error.txt'
        Remove-Item $body, $err -ErrorAction SilentlyContinue
        & curl.exe --silent --show-error --fail --connect-timeout 5 --max-time 12 --max-filesize 128 --proxy $proxy --output $body 'https://ipv4.webshare.io/' 2> $err
        $curlCode = $LASTEXITCODE
        $exitIp = if (Test-Path $body) { Read-PublicIp (Get-Content $body -Raw) } else { $null }
        $curlError = if (Test-Path $err) { (Get-Content $err -Raw).Trim() } else { '' }
        $stdout = Join-Path $scratch 'adapter-out.txt'
        $stderr = Join-Path $scratch 'adapter-error.txt'
        $process = Start-Process -FilePath $adapter -ArgumentList @($item.scheme, $hostIp, "$port") -NoNewWindow -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $finished = $process.WaitForExit(30000)
        if (-not $finished) { $process.Kill(); $process.WaitForExit(); $adapterCode = 124 } else { $adapterCode = $process.ExitCode }
        $adapterOutput = (Get-Content $stdout -Raw).Trim()
        $adapterIp = if ($adapterOutput -match '^OK\|true\|true\|([0-9a-fA-F:.]+)$') { Read-PublicIp $Matches[1] } else { $null }
        $result = [ordered]@{
            scheme = $item.scheme; host = $hostIp; port = $port
            curlExit = $curlCode; curlExitIp = $exitIp; curlError = $curlError
            adapterExit = $adapterCode; adapterResult = $adapterOutput; adapterExitIp = $adapterIp
            verified = ($curlCode -eq 0 -and $adapterCode -eq 0 -and $exitIp -and $adapterIp -and $exitIp -eq $adapterIp -and $exitIp -ne $baselineIp)
        }
        $results += [pscustomobject] $result
        $result | ConvertTo-Json -Compress | Write-Output
    }
    $evidence = [ordered]@{
        capturedAt = [DateTimeOffset]::UtcNow.ToString('o'); commit = $env:GITHUB_SHA
        os = [Environment]::OSVersion.VersionString; source = 'https://geonode.com/free-proxy-list/usa'
        directExitIp = $baselineIp; candidates = $results
        verifiedCount = @($results | Where-Object verified).Count
        scope = 'Anonymous public GET only. Curl verifies HTTPS response with normal certificate validation; Brows tests its production loopback broker, CONNECT and HTTP exit-IP decoder. No browser UI, credentials, DNS-leak or ICE-leak acceptance.'
    }
    $evidence | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $reportPath
    if ($evidence.verifiedCount -eq 0) { throw 'No candidate passed both curl and the production Brows adapter with a matching non-direct exit IP' }
} finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
