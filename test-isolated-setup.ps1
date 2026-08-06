# Isolated setup testing script for MaMo Connector
#
# Wipes local caches and settings (backing them up first) to force a brand new
# JRE and Forge version download, and runs the installer to verify it behaves correctly.

param(
    [switch]$UseDownloaded
)

$ErrorActionPreference = "Stop"

function Write-Step($n, $text) {
    Write-Host "`n[$n] $text" -ForegroundColor Cyan
}

$appDataDir = Join-Path $env:APPDATA "MamoConnector"
$localAppDataDir = Join-Path $env:LOCALAPPDATA "MamoConnector"
$backupSuffix = "$(Get-Date -Format 'yyyyMMdd-HHmmss')"
$appDataBackup = "${appDataDir}_dev_backup_${backupSuffix}"
$localAppDataBackup = "${localAppDataDir}_dev_backup_${backupSuffix}"
$downloadedInstallerPath = ""

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  MaMo Connector - Isolated Setup Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Step 1: Back up existing developer configurations
Write-Host "`n[1/5] Backing up existing developer directories..." -ForegroundColor Yellow
if (Test-Path $appDataDir) {
    Write-Host "  Backing up $appDataDir to $appDataBackup" -ForegroundColor Gray
    Rename-Item -Path $appDataDir -NewName (Split-Path $appDataBackup -Leaf)
}
if (Test-Path $localAppDataDir) {
    Write-Host "  Backing up $localAppDataDir to $localAppDataBackup" -ForegroundColor Gray
    Rename-Item -Path $localAppDataDir -NewName (Split-Path $localAppDataBackup -Leaf)
}

try {
    if ($UseDownloaded) {
        # Step 2: Download the latest release installer from GitHub
        Write-Host "`n[2/5] Downloading latest release installer from GitHub..." -ForegroundColor Yellow
        $downloadUrl = "https://github.com/killriam/mamo-Connector/releases/download/v0.3.0/mamo-connector-v0.3.0-windows-x64.exe"
        $downloadedInstallerPath = Join-Path $PSScriptRoot "mamo-connector-v0.3.0-windows-x64.exe"
        Write-Host "  Downloading to $downloadedInstallerPath..." -ForegroundColor Gray
        Invoke-WebRequest -Uri $downloadUrl -OutFile $downloadedInstallerPath -UseBasicParsing
        $installer = Get-Item $downloadedInstallerPath
    } else {
        # Step 2: Build the installer from local source
        Write-Host "`n[2/5] Building release installer..." -ForegroundColor Yellow
        Push-Location $PSScriptRoot
        powershell -ExecutionPolicy Bypass -File .\build.ps1
        Pop-Location

        # Step 3: Run the installer
        Write-Host "`n[3/5] Launching installer..." -ForegroundColor Yellow
        $installer = Get-ChildItem "$PSScriptRoot\target\installer\MamoConnector-*-Setup.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if (-not $installer) {
            throw "Installer executable not found in target/installer!"
        }
    }

    Write-Host "  Running $($installer.Name)..." -ForegroundColor Gray
    
    # We run it normally (not silent) so the user can see the wizard and verify it installs cleanly
    $process = Start-Process -FilePath $installer.FullName -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Installer exited with non-zero code: $($process.ExitCode)"
    }
    Write-Host "  Installation completed." -ForegroundColor Green

    # Step 4: Verify installed app launches and downloads JRE/Forge
    Write-Host "`n[4/5] Testing installed app..." -ForegroundColor Yellow
    # Locate the installed application
    $installedExe = Join-Path $env:LOCALAPPDATA "Programs\Mamo Connector\mamo-connector.exe"
    if (-not (Test-Path $installedExe)) {
        # Fallback to local appdata folder if self-relocation structure was created
        $installedExe = Join-Path $env:LOCALAPPDATA "MamoConnector\app\mamo-connector.exe"
    }

    if (-not (Test-Path $installedExe)) {
        throw "Could not locate installed mamo-connector.exe!"
    }

    Write-Host "  Launching $installedExe..." -ForegroundColor Gray
    Start-Process -FilePath $installedExe

    Write-Host "`n==================================================================" -ForegroundColor Yellow
    Write-Host "  ACTION REQUIRED:" -ForegroundColor Red
    Write-Host "  1. Verify the setup wizard opens in a clean state." -ForegroundColor White
    Write-Host "  2. Confirm it downloads a new JRE and Forge version." -ForegroundColor White
    Write-Host "  3. Close the MaMo Connector window once you have verified this." -ForegroundColor White
    Write-Host "==================================================================" -ForegroundColor Yellow
    
    Read-Host "Press ENTER after you have closed the connector app to clean up and restore your settings"

} finally {
    # Step 5: Restore backups
    Write-Step 5 "Restoring developer configuration backups..."
    
    # Clean up downloaded installer if we used it
    if ($downloadedInstallerPath -and (Test-Path $downloadedInstallerPath)) {
        Remove-Item -Path $downloadedInstallerPath -Force -ErrorAction SilentlyContinue
    }

    # Remove test directories
    if (Test-Path $appDataDir) {
        Remove-Item -Path $appDataDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path $localAppDataDir) {
        Remove-Item -Path $localAppDataDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    # Restore originals
    if (Test-Path $appDataBackup) {
        Write-Host "  Restoring $appDataDir" -ForegroundColor Gray
        Rename-Item -Path $appDataBackup -NewName (Split-Path $appDataDir -Leaf)
    }
    if (Test-Path $localAppDataBackup) {
        Write-Host "  Restoring $localAppDataDir" -ForegroundColor Gray
        Rename-Item -Path $localAppDataBackup -NewName (Split-Path $localAppDataDir -Leaf)
    }

    Write-Host "Developer configuration restored successfully!" -ForegroundColor Green
}
