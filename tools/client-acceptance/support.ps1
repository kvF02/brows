# Compatible with Windows PowerShell 5.1. No product data or credentials are read.
Set-StrictMode -Version Latest

function Get-BrowsSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-BrowsKit([string]$Directory) {
    $manifest = Join-Path $Directory 'SHA256SUMS.txt'
    $entries = @{}
    foreach ($line in [IO.File]::ReadAllLines($manifest)) {
        if ($line -cnotmatch '^([a-f0-9]{64})  (brows-desktop\.exe|installed/brows-desktop\.exe|[A-Za-z0-9_.-]+-setup\.exe)$') {
            throw 'Unexpected package hash manifest entry'
        }
        $name = $Matches[2]
        if ($entries.ContainsKey($name)) { throw 'Duplicate package hash manifest entry' }
        $entries[$name] = $Matches[1]
    }
    $installers = @($entries.Keys | Where-Object { $_ -like '*-setup.exe' })
    if ($entries.Count -ne 3 -or $installers.Count -ne 1 -or -not $entries.ContainsKey('brows-desktop.exe') -or -not $entries.ContainsKey('installed/brows-desktop.exe')) {
        throw 'Expected exactly one installer, one portable executable and one installed executable hash'
    }
    foreach ($name in @('brows-desktop.exe', $installers[0])) {
        if ((Get-BrowsSha256 (Join-Path $Directory $name)) -cne $entries[$name]) { throw 'Package SHA256 mismatch' }
    }
    $build = [IO.File]::ReadAllText((Join-Path $Directory 'beta-build.json')) | ConvertFrom-Json
    if ($build.commit -cnotmatch '^[a-f0-9]{40}$' -or $build.channel -cne 'internal-beta') { throw 'Invalid beta build identity' }
    return [pscustomobject]@{
        commit = $build.commit
        installerName = $installers[0]
        installedSha256 = $entries['installed/brows-desktop.exe']
    }
}

function Get-BrowsPeMachine([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5a4d) { throw 'Invalid executable header' }
        $stream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        if ($offset -lt 64 -or $offset -gt ($stream.Length - 6)) { throw 'Invalid executable offset' }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw 'Invalid PE signature' }
        $machine = $reader.ReadUInt16()
        switch ($machine) { 0x8664 { return 'x64' }; 0xaa64 { return 'arm64' }; 0x014c { return 'x86' }; default { throw 'Unknown PE architecture' } }
    } finally { $reader.Dispose() }
}

function Get-BrowsClientPlatform {
    if ($env:OS -ne 'Windows_NT') { throw 'Run on Windows 10 or Windows 11' }
    $os = Get-CimInstance Win32_OperatingSystem
    $native = $env:PROCESSOR_ARCHITEW6432
    if (-not $native) { $native = $env:PROCESSOR_ARCHITECTURE }
    $client = [int]$os.ProductType -eq 1
    $build = [int]$os.BuildNumber
    $family = 'Windows Server or unsupported client'
    if ($client -and $build -ge 22000) { $family = 'Windows 11' }
    elseif ($client -and $build -ge 19041) { $family = 'Windows 10' }
    return [pscustomobject]@{
        family = $family; version = $os.Version; build = $build
        productType = [int]$os.ProductType; nativeArchitecture = $native
        powershellVersion = $PSVersionTable.PSVersion.ToString()
        supportedTestTarget = $client -and $build -ge 19041 -and $native -in @('AMD64', 'ARM64')
    }
}

function Get-BrowsOperatorStatus([object[]]$Checks, [int]$ExpectedCount = 11) {
    if (@($Checks | Where-Object { $_.result -eq 'fail' }).Count -gt 0) { return 'failed' }
    if ($Checks.Count -ne $ExpectedCount -or @($Checks | Where-Object { $_.result -ne 'pass' }).Count -gt 0) { return 'incomplete' }
    return 'operator-accepted'
}
