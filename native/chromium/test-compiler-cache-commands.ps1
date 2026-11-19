param([string]$Cache, [string]$Compiler)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/compiler-cache.ps1"
$source = 'third_party/blink/renderer/core/execution_context/navigator_base.cc'
$object = 'obj/third_party/blink/renderer/core/core/navigator_base.obj'
$command = 'D:/brows-cache-tools/sccache.exe clang-cl.exe /c ../../' + $source + ' /Fo' + $object
Assert-NativeCacheCommand $command $source $object
foreach ($bad in @(
    ($command + ' -Xclang -fmodules-cache-path=/not_exist_dummy_dir'),
    ($command + ' -fmodules'), ($command + ' -Xclang -emit-module'),
    ($command + ' -fmodule-file=libcxx.pcm'),
    $command.Replace('sccache.exe', 'other.exe'),
    $command.Replace('navigator_base.cc', 'other.cc'),
    $command.Replace('navigator_base.obj', 'other.obj'),
    ($command + "`nexit /b 0")
)) {
    $rejected = $false
    try { Assert-NativeCacheCommand $bad $source $object } catch { $rejected = $true }
    if (-not $rejected) { throw 'Unsupported modules or mismatched compiler command was accepted' }
}
Write-Output 'Actual source/object/wrapper binding and unsupported module command boundaries passed'

$directory = Join-Path ([IO.Path]::GetTempPath()) ('brows replay ' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
try {
    $padding = 'x' * 10000
    $python = (Get-Command python -CommandType Application | Select-Object -First 1).Source
    [IO.File]::WriteAllText((Join-Path $directory 'arguments.py'), @'
import sys
from pathlib import Path
assert sys.argv[1:] == ["x" * 10000, "with spaces & %PATH% $"]
Path("observed").touch()
'@)
    $longCommand = '"{0}" arguments.py {1} "with spaces & %PATH% $"' -f $python, $padding
    Invoke-NativeCacheCommand $longCommand $directory 'argument control'
    if (-not (Test-Path -LiteralPath (Join-Path $directory 'observed') -PathType Leaf)) { throw 'Wrong replay working directory' }
    $rejected = $false
    try { Invoke-NativeCacheCommand ('"{0}" -c "import sys; sys.exit(7)"' -f $python) $directory 'failure control' }
    catch { if ($_.Exception.Message -notmatch 'exit 7,') { throw }; $rejected = $true }
    if (-not $rejected) { throw 'Failed replay was accepted' }

    if ($Cache -and $Compiler) {
        $source = 'long-command.cc'
        $object = 'obj/long-command.obj'
        [IO.Directory]::CreateDirectory((Join-Path $directory 'obj')) | Out-Null
        [IO.File]::WriteAllText((Join-Path $directory $source), 'int cache_replay_control() { return 158; }')
        $command = '"{0}" "{1}" /nologo /c {2} /Fo{3} /DBROWS_LONG={4}' -f $Cache, $Compiler, $source, $object, $padding
        Assert-NativeCacheCommand $command $source $object
        Invoke-NativeCacheCommand $command $directory $object
        $path = Join-Path $directory $object
        $expected = (Get-FileHash -LiteralPath $path).Hash
        Remove-Item -LiteralPath $path
        Set-NativeCompilerCacheMode $Cache READ_ONLY
        try {
            $proof = @(Test-NativeCppCacheCommands $Cache $directory @([pscustomobject]@{ source = $source; object = $object; command = $command }))
            if ($proof.Count -ne 1 -or $proof[0].cacheHits -lt 1 -or (Get-FileHash -LiteralPath $path).Hash -ne $expected) {
                throw 'Long compiler replay did not restore the identical object'
            }
            $before = Read-NativeCompilerCacheStats $Cache
            $newSymbol = 'uncached_' + [guid]::NewGuid().ToString('N')
            [IO.File]::WriteAllText((Join-Path $directory $source), "int $newSymbol() { return 1; }")
            1..2 | ForEach-Object { Invoke-NativeCacheCommand $command $directory $object }
            $after = Read-NativeCompilerCacheStats $Cache
            if ($after.writes -ne $before.writes -or $after.hits -ne $before.hits -or $after.requests - $before.requests -ne 2) {
                throw 'Read-only misses contaminated restored-hit evidence'
            }
        } finally { Set-NativeCompilerCacheMode $Cache READ_WRITE }
    }
} finally { Remove-Item -LiteralPath $directory -Recurse -Force }
