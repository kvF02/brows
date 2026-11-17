# Pin the compiler wrapper independently of the Chromium source/toolchain pins.
function Install-NativeCompilerCache([string]$Directory) {
    if ($Directory -notmatch '^[A-Z]:\\[A-Za-z0-9_-]+$') { throw 'Use a fixed, simple compiler-cache tool directory' }
    if (Test-Path -LiteralPath $Directory) { throw 'Compiler-cache tool directory must be fresh' }
    New-Item -ItemType Directory -Path $Directory | Out-Null
    $archive = Join-Path $Directory 'sccache.zip'
    Invoke-WebRequest 'https://github.com/mozilla/sccache/releases/download/v0.19.0/sccache-v0.19.0-x86_64-pc-windows-msvc.zip' -OutFile $archive
    if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne '28be8c61f6b2c89b9eb2869f6af7f78e6f992e78a8abae0ce855fcc129cbeac1') { throw 'Compiler-cache archive digest mismatch' }
    Expand-Archive -LiteralPath $archive -DestinationPath $Directory
    $files = @(Get-ChildItem -LiteralPath $Directory -Filter sccache.exe -Recurse -File)
    if ($files.Count -ne 1) { throw 'Expected exactly one pinned compiler-cache executable' }
    $exe = Join-Path $Directory 'sccache.exe'
    Move-Item -LiteralPath $files[0].FullName -Destination $exe
    if ([string](& $exe --version) -ne 'sccache 0.19.0' -or $LASTEXITCODE -ne 0) { throw 'Compiler cache is not executable' }
    $exe
}

function Test-NativeCompilerCache([string]$Cache, [string]$Compiler, [string]$Directory) {
    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    $source = Join-Path $Directory 'cache-smoke.cc'
    $object = Join-Path $Directory 'cache-smoke.obj'
    [IO.File]::WriteAllText($source, 'int brows_cache_smoke() { return 158; }')
    $before = (& $Cache --show-stats --stats-format=json | ConvertFrom-Json).stats.cache_hits.counts
    $beforeCount = [long](($before.PSObject.Properties.Value | Measure-Object -Sum).Sum)
    1..2 | ForEach-Object {
        Remove-Item -LiteralPath $object -Force -ErrorAction SilentlyContinue
        & $Cache $Compiler /nologo /c $source "/Fo$object"
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $object -PathType Leaf)) { throw 'Compiler-cache smoke compilation failed' }
    }
    $after = (& $Cache --show-stats --stats-format=json | ConvertFrom-Json).stats.cache_hits.counts
    $afterCount = [long](($after.PSObject.Properties.Value | Measure-Object -Sum).Sum)
    if ($afterCount -le $beforeCount) { throw 'The real compiler did not produce any cache hit' }
    [pscustomobject]@{ compiler = $Compiler; cacheHitsObserved = $afterCount - $beforeCount; runtimeAdmitted = $false }
}

function Invoke-NativeCompileCheckpoint([string]$Depot, [int]$Seconds) {
    if ($Seconds -lt 1 -or $Seconds -gt 16800) { throw 'Compile budget must leave time to save the cache before the job deadline' }
    # All paths/arguments here are fixed or validated build-root paths. Start a
    # separately owned tree so a cooperative budget can stop only this build.
    if ($Depot -notmatch '^[A-Z]:\\[A-Za-z0-9_\\-]+$') { throw 'Unsafe depot_tools path' }
    $command = '""' + (Join-Path $Depot 'autoninja.bat') + '" -C out/BrowsNative headless_shell"'
    $process = Start-Process -FilePath $env:ComSpec -ArgumentList @('/d', '/s', '/c', $command) -NoNewWindow -PassThru
    try {
        if (-not $process.WaitForExit($Seconds * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            return $false
        }
        if ($process.ExitCode -ne 0) { throw "Native compilation failed with exit $($process.ExitCode)" }
        return $true
    } finally { $process.Dispose() }
}
