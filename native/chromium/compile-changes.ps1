# Real GN source-to-object mapping, not guessed Ninja target names.
function ConvertTo-NativeCppObject([string]$Output) {
    $normalized = $Output.Trim().Replace('\', '/')
    if ($normalized -notmatch '^obj/[A-Za-z0-9_./-]+\.obj$' -or
        @($normalized.Split('/') | Where-Object { $_ -in @('.', '..', '') }).Count -gt 0) {
        throw 'GN did not return a safe build-relative C++ object target'
    }
    $normalized
}

function Get-NativeCppCompilePlan([string]$Depot, [string]$OutRelative, [string[]]$Sources) {
    if ($Sources.Count -lt 1 -or $Sources.Count -gt 16) { throw 'Invalid patched C++ source count' }
    $gn = Join-Path $Depot 'gn.bat'
    $plan = [Collections.Generic.List[object]]::new()
    foreach ($source in $Sources) {
        if ($source -notmatch '^third_party/blink/renderer/[A-Za-z0-9_/-]+\.cc$' -or
            $source.Split('/') -contains '..') { throw 'Invalid patched C++ source path' }
        $raw = @(& $gn outputs $OutRelative "//$source")
        if ($LASTEXITCODE -ne 0) { throw 'GN patched-source output discovery failed' }
        $outputs = @($raw | Where-Object { $_.Trim() } | ForEach-Object { ConvertTo-NativeCppObject $_ } | Sort-Object -Unique)
        if ($outputs.Count -lt 1 -or $outputs.Count -gt 4) { throw 'Patched C++ source has no bounded compile outputs' }
        $plan.Add([pscustomobject]@{ source = $source; generatedProbe = $false; outputs = $outputs })
    }

    # The generated observer implementation must also compile. Checking only
    # NavigatorBase and InspectorEmulationAgent could miss a bad generated call.
    $generated = @(& $gn outputs $OutRelative '//third_party/blink/renderer/core/probe/core_probes.pidl')
    if ($LASTEXITCODE -ne 0) { throw 'GN probe generator output discovery failed' }
    $probe = @($generated | Where-Object { $_.Trim().Replace('\', '/').EndsWith('/core_probes_impl.cc') })
    if ($probe.Count -ne 1) { throw 'Expected exactly one generated core probe implementation' }
    $probe = $probe[0].Trim().Replace('\', '/')
    if ($probe -notmatch '^gen/[A-Za-z0-9_./-]+/core_probes_impl\.cc$' -or
        $probe.Split('/') -contains '..') { throw 'Unsafe generated probe source path' }
    $raw = @(& $gn outputs $OutRelative "//$OutRelative/$probe")
    if ($LASTEXITCODE -ne 0) { throw 'GN generated-probe compile output discovery failed' }
    $outputs = @($raw | Where-Object { $_.Trim() } | ForEach-Object { ConvertTo-NativeCppObject $_ } | Sort-Object -Unique)
    if ($outputs.Count -lt 1 -or $outputs.Count -gt 4) { throw 'Generated probe has no bounded compile outputs' }
    $plan.Add([pscustomobject]@{ source = "$OutRelative/$probe"; generatedProbe = $true; outputs = $outputs })
    $plan.ToArray()
}

function Get-NativeCppObjectEvidence([string]$OutDirectory, [object[]]$Plan) {
    $evidence = foreach ($entry in $Plan) {
        $objects = foreach ($relative in $entry.outputs) {
            $relative = ConvertTo-NativeCppObject $relative
            $path = Join-Path $OutDirectory $relative
            $item = Get-Item -LiteralPath $path -ErrorAction Stop
            if ($item.PSIsContainer -or $item.Length -le 0 -or
                ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Compiled object missing or linked' }
            [pscustomobject]@{ path = $relative; bytes = $item.Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
        }
        [pscustomobject]@{ source = $entry.source; generatedProbe = $entry.generatedProbe; objects = @($objects) }
    }
    @($evidence)
}
