$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'compile-changes.ps1')
if ((ConvertTo-NativeCppObject 'obj\blink\navigator_base.obj') -ne 'obj/blink/navigator_base.obj') { throw 'Object normalization failed' }
foreach ($path in @('', '../obj/escape.obj', 'obj/../escape.obj', '/outside.obj', 'C:/outside.obj',
    'obj/core/header.h', 'obj/a/./unit.obj', 'obj/a b.obj', "obj/a`nunit.obj")) {
    $rejected = $false
    try { ConvertTo-NativeCppObject $path | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'Unsafe or non-object GN output was accepted' }
}
$root = Join-Path ([IO.Path]::GetTempPath()) ('brows-cpp-evidence-' + [guid]::NewGuid().ToString('N'))
try {
    [IO.Directory]::CreateDirectory((Join-Path $root 'obj/blink')) | Out-Null
    $plan = @([pscustomobject]@{ source = 'owned-fixture.cc'; generatedProbe = $false; outputs = @('obj/blink/fixture.obj') })
    $rejected = $false
    try { Get-NativeCppObjectEvidence $root $plan | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'Missing output counted as compilation evidence' }
    [IO.File]::WriteAllBytes((Join-Path $root 'obj/blink/fixture.obj'), [byte[]]@())
    $rejected = $false
    try { Get-NativeCppObjectEvidence $root $plan | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw 'Empty output counted as compilation evidence' }
    [IO.File]::WriteAllBytes((Join-Path $root 'obj/blink/fixture.obj'), [Text.Encoding]::UTF8.GetBytes('unit fixture, not C++ compilation'))
    $observed = @(Get-NativeCppObjectEvidence $root $plan)
    if ($observed.Count -ne 1 -or $observed[0].objects[0].sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Output evidence was not hashed' }
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
Write-Output 'GN output boundaries and missing/empty object evidence checks passed; no Chromium compilation claimed'
