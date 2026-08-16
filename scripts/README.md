# MaMo Connector - Local Code Signing Tools

This directory contains scripts and documentation for code-signing the MaMo Connector executable using a self-signed certificate.

> [!NOTE]
> This self-signed certificate strategy is a **temporary fallback** used to test automated signing in the CI/CD pipeline and local/alpha environments until the official SignPath Foundation application is approved.

---

## 1. Generating a New Certificate

To generate a self-signed certificate, open a PowerShell terminal on a Windows machine and run:

```powershell
cd scripts
.\generate-signing-cert.ps1
```

### Outputs:
1. **`mamo-signing.pfx`**: The private key certificate containing the code-signing credentials. **Do not share this file publicly.**
2. **`mamo-signing.cer`**: The public key file. This is distributed to testers to import/trust on their local machines.
3. **`mamo-signing-base64.txt`**: A Base64-encoded text representation of the PFX file. This is used to copy into GitHub Secrets.

---

## 2. GitHub Actions CI/CD Integration

To enable automated code signing on release builds, add the following Repository Secrets to your GitHub repository (`Settings -> Secrets and variables -> Actions`):

1. **`SIGNING_CERTIFICATE_BASE64`**: Paste the entire contents of the generated `mamo-signing-base64.txt` file.
2. **`SIGNING_CERTIFICATE_PASSWORD`**: Use the password defined during generation (default: `MamoConnectorPassword123!`).

The release workflow (`release.yml`) automatically checks if these secrets are present. If found, it decodes the PFX, locates `signtool.exe` on the runner, signs the compiled executable, and cleans up the key. If the secrets are missing, signing is safely skipped.

---

## 3. Trusting the Certificate Locally

Because the certificate is self-signed, Windows will flag it as untrusted by default. To allow the signed executable to run on your local machine without SmartScreen warnings or Smart App Control (SAC) blocks, you must import the public certificate.

Open PowerShell **as Administrator** and run:

```powershell
Import-Certificate -FilePath .\mamo-signing.cer -CertStoreLocation Cert:\LocalMachine\Root
Import-Certificate -FilePath .\mamo-signing.cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher
```

This imports the public key into:
* **Trusted Root Certification Authorities**: Trusted as a valid root CA.
* **Trusted Publishers**: Allows the publisher "MaMo Connector Self-Signed Signing" to execute signed applications automatically.
