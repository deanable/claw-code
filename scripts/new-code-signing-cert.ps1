param(
    [string]$Subject = "CN=Dean Kruger",
    [string]$OutputDirectory = (Join-Path $env:USERPROFILE "claw-code-signing"),
    [string]$BaseName = "claw-code-signing",
    [int]$YearsValid = 3,
    [switch]$TrustCurrentUser
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $OutputDirectory)) {
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
}

$notAfter = (Get-Date).AddYears($YearsValid)
$certificate = New-SelfSignedCertificate `
    -Type CodeSigningCert `
    -Subject $Subject `
    -FriendlyName "Claw Code Signing Certificate" `
    -KeyAlgorithm RSA `
    -KeyLength 4096 `
    -HashAlgorithm SHA256 `
    -KeyExportPolicy Exportable `
    -CertStoreLocation "Cert:\CurrentUser\My" `
    -NotAfter $notAfter

$password = Read-Host "Enter a password to protect the exported PFX" -AsSecureString
$pfxPath = Join-Path $OutputDirectory "$BaseName.pfx"
$cerPath = Join-Path $OutputDirectory "$BaseName.cer"

Export-PfxCertificate -Cert $certificate -FilePath $pfxPath -Password $password | Out-Null
Export-Certificate -Cert $certificate -FilePath $cerPath | Out-Null

if ($TrustCurrentUser) {
    Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\CurrentUser\Root" | Out-Null
    Import-Certificate -FilePath $cerPath -CertStoreLocation "Cert:\CurrentUser\TrustedPublisher" | Out-Null
}

Write-Host "Created code signing certificate:"
Write-Host "  Subject: $Subject"
Write-Host "  Expires: $($notAfter.ToString('u'))"
Write-Host "  PFX:     $pfxPath"
Write-Host "  CER:     $cerPath"
if ($TrustCurrentUser) {
    Write-Host "  Trusted: current user root and trusted publisher stores"
}
