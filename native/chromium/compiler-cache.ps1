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

function Read-NativeCompilerCacheStats([string]$Cache) {
    $value = & $Cache --show-stats --stats-format=json | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $null -eq $value.stats.compile_requests) { throw 'Compiler-cache statistics unavailable' }
    [pscustomobject]@{
        hits = [long](($value.stats.cache_hits.counts.PSObject.Properties.Value | Measure-Object -Sum).Sum)
        writes = [long]$value.stats.cache_writes
        requests = [long]$value.stats.compile_requests
        rejected = [long]$value.stats.requests_not_cacheable
    }
}

function Set-NativeCompilerCacheMode([string]$Cache, [ValidateSet('READ_ONLY', 'READ_WRITE')][string]$Mode) {
    & $Cache --stop-server | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not stop the active compiler cache' }
    $env:SCCACHE_LOCAL_RW_MODE = $Mode
    & $Cache --start-server | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not start the compiler cache in $Mode mode" }
}

function Assert-NativeCacheCommand([string]$Command, [string]$Source, [string]$Object) {
    if ($Command.Length -lt 1 -or $Command.Length -gt 32700 -or $Command -match '[\r\n]' -or
        -not $Command.Contains('sccache.exe') -or
        -not $Command.Replace('\', '/').Contains($Object) -or
        -not $Command.Replace('\', '/').Contains($Source)) { throw 'Cannot bind the actual GN compiler command to its source/object' }
    if ($Command -match '-fmodules(?:-cache-path|\b)|-emit-module|fmodule-file=') {
        throw 'Actual GN compiler command still uses modules unsupported by this compiler cache'
    }
}

function Get-NativeCppCacheCommands([string]$SourceRoot, [string]$Depot, [string]$OutDirectory, [object[]]$Plan) {
    $ninja = @((Join-Path $SourceRoot 'third_party/ninja/ninja.exe'), (Join-Path $Depot 'ninja.bat')) |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    if (-not $ninja) { throw 'Pinned Ninja command introspection executable missing' }
    foreach ($entry in $Plan) {
        foreach ($object in $entry.outputs) {
            $lines = @(& $ninja -C $OutDirectory -t commands $object)
            if ($LASTEXITCODE -ne 0 -or $lines.Count -eq 0) { throw 'Ninja could not describe the actual object compiler command' }
            $command = [string]$lines[-1]
            # Generated input paths in commands are build-relative.
            $source = $entry.source.Replace('out/BrowsNative/', '')
            Assert-NativeCacheCommand $command $source $object
            [pscustomobject]@{ source = $entry.source; object = $object; command = $command }
        }
    }
}

function Invoke-NativeCacheCommand([string]$Command, [string]$OutDirectory, [string]$Object) {
    # Ninja has already rendered the command. Split off only the executable and
    # pass its argument text verbatim. cmd.exe truncates real Blink commands.
    $parts = [regex]::Match($Command, '^\s*(?:"(?<exe>[^"]+)"|(?<exe>[^\s"]+))[ \t]+(?<args>[^\r\n]+)\z')
    if (-not $parts.Success) { throw "Cannot identify compiler executable for $Object" }
    $exe = $parts.Groups['exe'].Value
    if ($exe -match '[/\\]' -and -not [IO.Path]::IsPathFullyQualified($exe)) {
        $exe = [IO.Path]::GetFullPath($exe, $OutDirectory)
    }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $exe
    $start.Arguments = $parts.Groups['args'].Value
    $start.WorkingDirectory = $OutDirectory
    $start.UseShellExecute = $false
    $process = [Diagnostics.Process]::Start($start)
    try {
        if (-not $process.WaitForExit(120000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "Compiler-cache replay timed out for $Object"
        }
        if ($process.ExitCode -ne 0) { throw "Compiler-cache replay failed for $Object (exit $($process.ExitCode), $($Command.Length) characters)" }
    } finally { $process.Dispose() }
}

function Test-NativeCppCacheCommands([string]$Cache, [string]$OutDirectory, [object[]]$Commands) {
    foreach ($entry in $Commands) {
        $before = Read-NativeCompilerCacheStats $Cache
        Invoke-NativeCacheCommand $entry.command $OutDirectory $entry.object
        $after = Read-NativeCompilerCacheStats $Cache
        if ($after.hits -le $before.hits -or $after.rejected -gt $before.rejected) {
            throw "Chromium object command did not hit the compiler cache for $($entry.object)"
        }
        $bytes = [Text.Encoding]::UTF8.GetBytes($entry.command)
        [pscustomobject]@{ source = $entry.source; object = $entry.object; cacheHits = $after.hits - $before.hits;
            commandCharacters = $entry.command.Length;
            commandSha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant() }
    }
}
