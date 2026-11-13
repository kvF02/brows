param([Parameter(Mandatory)][string]$Report, [switch]$RequireReady)
$ErrorActionPreference = 'Stop'
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'source-manifest.json') -Raw | ConvertFrom-Json
$required = $manifest.buildRequirements
$result = [ordered]@{
    schemaVersion = 1
    sourceRevision = $manifest.upstream.revision
    scope = 'Read-only Windows source-build prerequisites; no browser qualification'
    readyForBuild = $false
    compiled = $false
    nativeAcceptancePassed = $false
    complete = $false
    requirements = $required
    blockers = @()
    checkoutNetwork = 'not_checked'
    failure = $null
}

function Read-FileVersion([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($Path)
    "$($info.FileMajorPart).$($info.FileMinorPart).$($info.FileBuildPart).$($info.FilePrivatePart)"
}

try {
    if (-not $IsWindows) { throw 'This preflight requires Windows' }
    $result.architecture = $env:PROCESSOR_ARCHITECTURE
    $result.osVersion = [Environment]::OSVersion.Version.ToString()
    $result.ramGiB = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 2)
    $result.logicalProcessors = (Get-CimInstance Win32_Processor | Measure-Object NumberOfLogicalProcessors -Sum).Sum
    $drives = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType = 3' | ForEach-Object {
        [pscustomobject]@{ drive = $_.DeviceID; filesystem = $_.FileSystem; freeGiB = [math]::Round($_.FreeSpace / 1GB, 2) }
    })
    $result.drives = $drives
    $selected = $drives | Where-Object { $_.filesystem -eq $required.filesystem -and $_.freeGiB -ge $required.minimumFreeDiskGiB } | Sort-Object freeGiB -Descending | Select-Object -First 1
    $result.selectedBuildDrive = if ($selected) { $selected.drive } else { $null }
    if ($result.architecture -ne $required.architecture) { $result.blockers += 'x64-build-host-required' }
    if ($result.ramGiB -lt $required.minimumRamGiB) { $result.blockers += 'insufficient-ram' }
    if (-not $selected) { $result.blockers += 'insufficient-ntfs-free-space' }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $visualStudios = @()
    if (Test-Path -LiteralPath $vswhere) {
        $lower = [string]$required.visualStudioMajor
        $upper = [string]($required.visualStudioMajor + 1)
        $raw = & $vswhere -products '*' -version "[$lower.0,$upper.0)" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 Microsoft.VisualStudio.Component.VC.ATLMFC -format json -utf8
        if ($LASTEXITCODE -ne 0) { throw 'Visual Studio inventory failed' }
        $visualStudios = @($raw | ConvertFrom-Json)
    }
    $result.visualStudioVersions = @($visualStudios | ForEach-Object installationVersion)
    if ($visualStudios.Count -eq 0) { $result.blockers += 'visual-studio-18-cpp-atlmfc-required' }

    $kits = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -ErrorAction SilentlyContinue
    $sdk = $null
    $debuggerVersion = $null
    if ($kits.KitsRoot10) {
        $sdkFolder = ([version]$required.sdkVersion).ToString(3) + '.0'
        $headers = Join-Path $kits.KitsRoot10 "Include/$sdkFolder/um/Windows.h"
        $rc = Join-Path $kits.KitsRoot10 "bin/$sdkFolder/x64/rc.exe"
        $sdk = [pscustomobject]@{ headersPresent = (Test-Path -LiteralPath $headers); resourceCompilerVersion = (Read-FileVersion $rc) }
        $debuggerVersion = Read-FileVersion (Join-Path $kits.KitsRoot10 'Debuggers/x64/cdb.exe')
    }
    $result.windowsSdk = $sdk
    $result.debuggerVersion = $debuggerVersion
    if (-not $sdk -or -not $sdk.headersPresent -or -not $sdk.resourceCompilerVersion -or [version]$sdk.resourceCompilerVersion -lt [version]$required.sdkVersion) { $result.blockers += 'required-windows-sdk-not-observed' }
    if (-not $debuggerVersion -or [version]$debuggerVersion -lt [version]$required.debuggerMinimumVersion) { $result.blockers += 'required-sdk-debugging-tools-not-observed' }
    $result.readyForBuild = $result.blockers.Count -eq 0
} catch {
    $result.failure = $_.Exception.Message
    $result.blockers += 'preflight-error'
} finally {
    $path = [IO.Path]::GetFullPath($Report)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $path -Encoding utf8
    $result | ConvertTo-Json -Depth 8 | Write-Output
    if ($env:GITHUB_OUTPUT) {
        $readyText = ([string]$result.readyForBuild).ToLowerInvariant()
        "ready=$readyText" | Add-Content -LiteralPath $env:GITHUB_OUTPUT
        $canPrepareSdk = -not $result.failure -and $result.blockers.Count -eq 1 -and $result.blockers[0] -eq 'required-windows-sdk-not-observed'
        "can_prepare_sdk=$(([string]$canPrepareSdk).ToLowerInvariant())" | Add-Content -LiteralPath $env:GITHUB_OUTPUT
        if ($result.selectedBuildDrive) { "drive=$($result.selectedBuildDrive)" | Add-Content -LiteralPath $env:GITHUB_OUTPUT }
    }
}
if ($result.failure -or ($RequireReady -and -not $result.readyForBuild)) { exit 1 }
