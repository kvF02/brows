param([Parameter(Mandatory)][string] $Directory)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or -not $IsWindows) { throw 'Hosted Windows CI only' }
$directoryPath = [IO.Path]::GetFullPath($Directory)
$temporaryRoot = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\') + '\'
if (-not $directoryPath.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($directoryPath) -notmatch '^brows-ui-tls-[a-f0-9]{32}$') { throw 'Expected the dedicated CI TLS directory' }
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'The disposable CI trust fixture requires an administrator' }
$subject = 'CN=Brows disposable UI test root ' + [IO.Path]::GetFileName($directoryPath)
$certificates = @()
$rootThumbprint = $null
function Save-Manifest {
    @{ subject = $subject; root = $rootThumbprint; my = @($certificates | ForEach-Object Thumbprint) } | ConvertTo-Json | Set-Content (Join-Path $directoryPath 'manifest.json')
}
Write-Output 'Creating the short-lived CI root'
$root = New-SelfSignedCertificate -Type Custom -Subject $subject -CertStoreLocation Cert:\CurrentUser\My -Provider 'Microsoft Software Key Storage Provider' -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 -KeyExportPolicy Exportable -KeyUsage CertSign, CRLSign -TextExtension @('2.5.29.19={critical}{text}ca=1&pathlength=0') -NotAfter (Get-Date).AddHours(2)
$certificates += $root
$rootThumbprint = $root.Thumbprint
Save-Manifest
Write-Output 'Creating the trusted and untrusted server identities'
$valid = New-SelfSignedCertificate -Type SSLServerAuthentication -DnsName 'checkip.amazonaws.com' -Signer $root -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable -NotAfter (Get-Date).AddHours(1)
$certificates += $valid
Save-Manifest
$untrusted = New-SelfSignedCertificate -Type SSLServerAuthentication -DnsName 'checkip.amazonaws.com' -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable -NotAfter (Get-Date).AddHours(1)
$certificates += $untrusted
Save-Manifest
Write-Output 'Exporting test identities without interactive key protection'
$password = ConvertTo-SecureString 'brows-ci-fixture-only' -AsPlainText -Force
Export-PfxCertificate -Cert $valid -FilePath (Join-Path $directoryPath 'valid.pfx') -Password $password -CryptoAlgorithmOption AES256_SHA256 | Out-Null
Export-PfxCertificate -Cert $untrusted -FilePath (Join-Path $directoryPath 'untrusted.pfx') -Password $password -CryptoAlgorithmOption AES256_SHA256 | Out-Null
$rootFile = Join-Path $directoryPath 'root.cer'
Export-Certificate -Cert $root -FilePath $rootFile | Out-Null
# CurrentUser Root can invoke a protected-root confirmation UI. This machine is
# disposable; its machine store admits the same CA without that user prompt.
Write-Output 'Installing the temporary CA on this disposable test machine'
Import-Certificate -FilePath $rootFile -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Write-Output 'CI TLS fixture ready'
