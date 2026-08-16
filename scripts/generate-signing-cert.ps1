# generate-signing-cert.ps1
# Generates a self-signed code-signing certificate (RSA-2048) for local testing and CI/CD.

param(
    [string]$Password = "MamoConnectorPassword123!",
    [string]$CommonName = "MaMo Connector Self-Signed Signing"
)

$ErrorActionPreference = "Stop"

if (-not $IsWindows -and $env:OS -ne "Windows_NT") {
    Write-Error "This script must be run on Windows."
    exit 1
}

Write-Host "========================================================="
Write-Host " Generating Self-Signed Code-Signing Certificate"
Write-Host "========================================================="
Write-Host "Common Name: CN=$CommonName"
Write-Host "Password:    $Password"
Write-Host "========================================================="

# 1. Generate the self-signed Code Signing Certificate in Current User store (no admin required)
$cert = New-SelfSignedCertificate -Type CodeSigningCert `
                                  -Subject "CN=$CommonName" `
                                  -KeyLength 2048 `
                                  -KeyAlgorithm RSA `
                                  -HashAlgorithm SHA256 `
                                  -CertStoreLocation "Cert:\CurrentUser\My" `
                                  -NotAfter (Get-Date).AddYears(5)

Write-Host "Certificate generated in Personal store: $($cert.Thumbprint)"

# 2. Export Private Key to PFX file
$pwdSecure = ConvertTo-SecureString $Password -AsPlainText -Force
$pfxPath = "mamo-signing.pfx"
Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $pwdSecure
Write-Host "Private certificate exported to: $pfxPath"

# 3. Export Public Key to CER file (for users to trust)
$cerPath = "mamo-signing.cer"
Export-Certificate -Cert $cert -FilePath $cerPath
Write-Host "Public certificate exported to:  $cerPath"

# 4. Generate Base64 string for GitHub Secret
$bytes = [System.IO.File]::ReadAllBytes($pfxPath)
$base64 = [System.Convert]::ToBase64String($bytes)
$base64Path = "mamo-signing-base64.txt"
$base64 | Out-File -FilePath $base64Path -Encoding ascii
Write-Host "Base64 PFX representation saved to: $base64Path"

# 5. Clean up from Local Certificate Store (so we don't leave garbage behind)
# The private key and certificate are already safely saved in mamo-signing.pfx.
$store = New-Object System.Security.Cryptography.X509Certificates.X509Store("My", "CurrentUser")
$store.Open("ReadWrite")
$store.Remove($cert)
$store.Close()
Write-Host "Temporary certificate removed from local store."

Write-Host "`nDone!"
Write-Host "---------------------------------------------------------"
Write-Host "GitHub Secrets Setup Instructions:"
Write-Host "1. Add secret: SIGNING_CERTIFICATE_BASE64"
Write-Host "   Value: Content of the generated '$base64Path'"
Write-Host "2. Add secret: SIGNING_CERTIFICATE_PASSWORD"
Write-Host "   Value: $Password"
Write-Host "---------------------------------------------------------"
Write-Host "Local Machine Trust Instructions (to test signatures locally):"
Write-Host "Open PowerShell as Administrator and run:"
Write-Host "Import-Certificate -FilePath .\$cerPath -CertStoreLocation Cert:\LocalMachine\Root"
Write-Host "Import-Certificate -FilePath .\$cerPath -CertStoreLocation Cert:\LocalMachine\TrustedPublisher"
Write-Host "---------------------------------------------------------"
