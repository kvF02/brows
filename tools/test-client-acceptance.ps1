# Run with Windows PowerShell 5.1, the oldest supported collector host.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/client-acceptance/support.ps1"
function Assert-True([bool]$Value, [string]$Message) { if (-not $Value) { throw $Message } }
function Assert-Rejected([scriptblock]$Action, [string]$Message) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert-True $rejected $Message
}
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('brows-client-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null
try {
    $files = @(Get-ChildItem -LiteralPath "$PSScriptRoot/client-acceptance" -Filter '*.ps1') + @(
        Get-Item "$PSScriptRoot/package-client-acceptance.ps1"
        Get-Item "$PSScriptRoot/windows-client-smoke.ps1"
        Get-Item "$PSScriptRoot/windows-beta-smoke.ps1"
        Get-Item "$PSScriptRoot/windows-beta-build-info.ps1"
        Get-Item "$PSScriptRoot/windows-bundler-tools.ps1"
    )
    foreach ($file in $files) {
        $parseErrors = $null; $tokens = $null
        [Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$parseErrors) | Out-Null
        Assert-True ($parseErrors.Count -eq 0) 'PowerShell 5.1 parse failed'
    }
    $bytes = [byte[]]::new(160)
    $bytes[0] = 0x4d; $bytes[1] = 0x5a; $bytes[0x3c] = 0x80
    $bytes[0x80] = 0x50; $bytes[0x81] = 0x45; $bytes[0x84] = 0x64; $bytes[0x85] = 0x86
    $portable = Join-Path $fixture 'brows-desktop.exe'
    [IO.File]::WriteAllBytes($portable, $bytes)
    Assert-True ((Get-BrowsPeMachine $portable) -eq 'x64') 'x64 PE not recognized'
    $bytes[0x84] = 0x64; $bytes[0x85] = 0xaa
    [IO.File]::WriteAllBytes($portable, $bytes)
    Assert-True ((Get-BrowsPeMachine $portable) -eq 'arm64') 'ARM64 PE not recognized'
    $bytes[0x3c] = 0xff
    [IO.File]::WriteAllBytes($portable, $bytes)
    Assert-Rejected { Get-BrowsPeMachine $portable } 'Out-of-file PE offset accepted'
    $bytes[0x3c] = 0x80; $bytes[0x85] = 0x86
    [IO.File]::WriteAllBytes($portable, $bytes)
    $installer = Join-Path $fixture 'Brows_0.1.0_x64-setup.exe'
    [IO.File]::WriteAllText($installer, 'test-only-not-an-installer')
    $valid = @(
        "$(Get-BrowsSha256 $portable)  brows-desktop.exe"
        "$(Get-BrowsSha256 $installer)  Brows_0.1.0_x64-setup.exe"
        "$('a' * 64)  installed/brows-desktop.exe"
    )
    $manifest = Join-Path $fixture 'SHA256SUMS.txt'
    $valid | Set-Content -LiteralPath $manifest
    @{ commit = ('b' * 40); channel = 'internal-beta' } | ConvertTo-Json | Set-Content (Join-Path $fixture 'beta-build.json')
    Assert-True ((Test-BrowsKit $fixture).installedSha256 -ceq ('a' * 64)) 'Valid kit rejected'
    @($valid + $valid[0]) | Set-Content -LiteralPath $manifest
    Assert-Rejected { Test-BrowsKit $fixture } 'Duplicate hash accepted'
    @($valid + "$('a' * 64)  ../escape-setup.exe") | Set-Content -LiteralPath $manifest
    Assert-Rejected { Test-BrowsKit $fixture } 'Path traversal entry accepted'
    $valid | Set-Content -LiteralPath $manifest
    [IO.File]::WriteAllText($installer, 'tampered')
    Assert-Rejected { Test-BrowsKit $fixture } 'Tampered installer accepted'
    $pass = @(@{result='pass'}) * 11
    Assert-True ((Get-BrowsOperatorStatus $pass) -eq 'operator-accepted') 'Complete pass rejected'
    Assert-True ((Get-BrowsOperatorStatus @(@{result='pass'})) -eq 'incomplete') 'Partial report accepted'
    Assert-True ((Get-BrowsOperatorStatus @()) -eq 'incomplete') 'Empty report accepted'
    Assert-True ((Get-BrowsOperatorStatus @($pass + @{result='not-checked'})) -eq 'incomplete') 'Skipped check accepted'
    Assert-True ((Get-BrowsOperatorStatus @($pass + @{result='fail'})) -eq 'failed') 'Failure hidden'
    Write-Output 'Client kit tests passed (format/integrity/report logic only; no operator or website evidence).'
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
