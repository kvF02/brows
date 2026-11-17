$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/compiler-cache.ps1"
$root = Join-Path $env:RUNNER_TEMP ('brows-budget-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $bat = Join-Path $root 'autoninja.bat'
    '@exit /b 0' | Set-Content $bat -Encoding ascii
    if (-not (Invoke-NativeCompileCheckpoint $root 5)) { throw 'Successful compiler rejected' }
    '@exit /b 7' | Set-Content $bat -Encoding ascii
    $rejected = $false
    try { Invoke-NativeCompileCheckpoint $root 5 | Out-Null } catch { $rejected = $_.Exception.Message.Contains('exit 7') }
    if (-not $rejected) { throw 'Compiler failure was mistaken for a saved checkpoint' }
    # Prove the separately owned child is also stopped at the budget boundary.
    $child = Join-Path $root 'child.ps1'
    '$PID | Set-Content child.pid; Start-Sleep -Seconds 120' | Set-Content $child
    '@pwsh -NoProfile -File "%~dp0child.ps1"' | Set-Content $bat -Encoding ascii
    Push-Location $root
    try {
        if (Invoke-NativeCompileCheckpoint $root 3) { throw 'Budget did not stop the compiler tree' }
        $childId = [int](Get-Content child.pid)
        if (Get-Process -Id $childId -ErrorAction SilentlyContinue) { throw 'Compiler child survived the budget' }
    } finally { Pop-Location }
    Write-Output 'Compiler success, real failure and owned-tree timeout checks passed'
} finally { Remove-Item -LiteralPath $root -Recurse -Force }
