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
