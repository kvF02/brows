$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $IsWindows) { throw 'Hosted Windows fixture only' }
$directory = Join-Path $env:RUNNER_TEMP ('brows-ui-tls-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $directory | Out-Null
try {
    $setup = Start-Process -FilePath (Get-Command pwsh.exe).Source -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-File', "$PSScriptRoot/windows-ui-tls-fixture.ps1", '-Directory', $directory, '-DnsName', 'localhost') -NoNewWindow -PassThru
    if (-not $setup.WaitForExit(45000)) { $setup.Kill($true); throw 'Login TLS setup exceeded 45 seconds' }
    if ($setup.ExitCode -ne 0) { throw 'Login TLS setup failed' }
    $env:BROWS_LOGIN_TLS_PFX = Join-Path $directory 'valid.pfx'
    cargo +1.97.1 test -p brows-desktop login_persistence_acceptance -- --ignored --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Login persistence acceptance failed' }
} finally {
    Remove-Item Env:BROWS_LOGIN_TLS_PFX -ErrorAction SilentlyContinue
    $manifestPath = Join-Path $directory 'manifest.json'
    if (Test-Path -LiteralPath $manifestPath) {
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        $subject = 'CN=Brows disposable UI test root ' + [IO.Path]::GetFileName($directory)
        if ($manifest.subject -eq $subject) {
            if ($manifest.root -match '^[A-Fa-f0-9]{40}$') {
                $rootPath = "Cert:\LocalMachine\Root\$($manifest.root)"
                if ((Get-Item -LiteralPath $rootPath -ErrorAction SilentlyContinue).Subject -eq $subject) { Remove-Item -LiteralPath $rootPath -ErrorAction SilentlyContinue }
            }
            foreach ($thumbprint in $manifest.my) {
                if ($thumbprint -match '^[A-Fa-f0-9]{40}$') { Remove-Item -LiteralPath "Cert:\CurrentUser\My\$thumbprint" -DeleteKey -ErrorAction SilentlyContinue }
            }
        }
    }
    Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
}
