# Uninstall / reset MaMo Connector's test environment
#
# Wipes everything the self-relocating first-run setup creates, so you can re-test the
# "brand new user" experience (download exe -> silently relocate -> auto-download Forge ->
# configure) from a clean slate. This is separate from the in-app Settings > Advanced >
# "Uninstall" button, which requires the app to already be running; this script works from
# the outside, whether or not anything is currently installed.
#
# Usage:
#   .\uninstall.ps1                  # full reset (Forge jar re-downloads next run, ~100-300MB)
#   .\uninstall.ps1 -KeepForgeCache  # keep the already-downloaded MaMo Forge jar (faster re-testing)

param(
    [switch]$KeepForgeCache
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  MaMo Connector - Uninstall / Reset" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# 1. Stop any running instance
$running = Get-Process -Name "mamo-connector" -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "`nStopping running mamo-connector..." -ForegroundColor Yellow
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    Write-Host "  Stopped." -ForegroundColor Gray
}

# 2. Remove the mamoConnector:// protocol registration
Write-Host "`nRemoving protocol registration..." -ForegroundColor Yellow
$regKey = "HKCU:\Software\Classes\mamoConnector"
if (Test-Path $regKey) {
    Remove-Item -Path $regKey -Recurse -Force
    Write-Host "  Removed $regKey" -ForegroundColor Gray
} else {
    Write-Host "  Already absent." -ForegroundColor Gray
}

# 3. Remove the self-relocated stable app copy (%LOCALAPPDATA%\MamoConnector)
Write-Host "`nRemoving relocated app copy..." -ForegroundColor Yellow
$stableAppDir = Join-Path $env:LOCALAPPDATA "MamoConnector"
if (Test-Path $stableAppDir) {
    Remove-Item -Path $stableAppDir -Recurse -Force
    Write-Host "  Removed $stableAppDir" -ForegroundColor Gray
} else {
    Write-Host "  No relocated copy found." -ForegroundColor Gray
}

# 4. Remove settings/cache (%APPDATA%\MamoConnector): settings.json, cached decks, lock file,
#    pending-command file, and the downloaded MaMo Forge jar — unless -KeepForgeCache was passed,
#    since re-fetching a ~100-300MB jar on every test run gets old fast.
Write-Host "`nRemoving settings and cache..." -ForegroundColor Yellow
$settingsDir = Join-Path $env:APPDATA "MamoConnector"
if (Test-Path $settingsDir) {
    if ($KeepForgeCache) {
        Get-ChildItem -Path $settingsDir -Exclude "forge" | Remove-Item -Recurse -Force
        Write-Host "  Cleared settings/cache, kept Forge download at $settingsDir\forge" -ForegroundColor Gray
    } else {
        Remove-Item -Path $settingsDir -Recurse -Force
        Write-Host "  Removed $settingsDir" -ForegroundColor Gray
    }
} else {
    Write-Host "  No settings directory found." -ForegroundColor Gray
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "  Done" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "`nNext run of a release build (e.g. .\dev-build.ps1 -Release, or a downloaded" -ForegroundColor White
Write-Host "release exe) will behave exactly like a brand-new install." -ForegroundColor White
if ($KeepForgeCache) {
    Write-Host "The wizard will offer 'Use Existing' for Forge instead of re-downloading it." -ForegroundColor Gray
}
