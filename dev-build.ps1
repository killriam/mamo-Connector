# Dev build script for Mamo Connector
#
# Usage:
#   .\dev-build.ps1           # debug build (~5s), registers debug binary
#   .\dev-build.ps1 -Release  # release build (~30s), keeps release binary registered
#   .\dev-build.ps1 -Watch    # cargo-watch: auto-rebuild on every file save (debug)
#
# The mamoConnector:// registry key is updated to point at whichever binary was built.
# After the first run you can just: cargo build  (the registry already points at debug)

param(
    [switch]$Release,
    [switch]$Watch
)

$ErrorActionPreference = "Stop"
$projectRoot = $PSScriptRoot

# Kill running instance
$running = Get-Process -Name "mamo-connector" -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "Stopping running mamo-connector..." -ForegroundColor Yellow
    $running | Stop-Process -Force
    Start-Sleep -Milliseconds 400
    Write-Host "  Stopped." -ForegroundColor Gray
}

# Watch mode
if ($Watch) {
    Write-Host "Watch mode - auto-rebuild on save (Ctrl+C to stop)" -ForegroundColor Cyan
    Write-Host "Using: cargo watch -x build" -ForegroundColor Gray
    Push-Location $projectRoot
    cargo watch -x "build"
    Pop-Location
    exit 0
}

# Build
Push-Location $projectRoot

if ($Release) {
    Write-Host "Building release..." -ForegroundColor Cyan
    cargo build --release
    $exePath = Join-Path $projectRoot "target\release\mamo-connector.exe"
} else {
    Write-Host "Building debug (faster)..." -ForegroundColor Cyan
    cargo build
    $exePath = Join-Path $projectRoot "target\debug\mamo-connector.exe"
}

$buildExit = $LASTEXITCODE
Pop-Location

if ($buildExit -ne 0) {
    Write-Host "Build FAILED." -ForegroundColor Red
    exit 1
}

# Register the built binary as the URL handler
$regKey   = "HKCU:\Software\Classes\mamoConnector\shell\open\command"
$regValue = """$exePath"" ""%1"""

New-Item -Path $regKey -Force | Out-Null
Set-ItemProperty -Path $regKey -Name "(default)" -Value $regValue

Write-Host ""
Write-Host "Build complete." -ForegroundColor Green
Write-Host "  Binary : $exePath" -ForegroundColor Gray
Write-Host "  Handler: $regValue" -ForegroundColor Gray
Write-Host ""
Write-Host "Click 'AI Sim' in the browser - the new binary will be invoked." -ForegroundColor Cyan
