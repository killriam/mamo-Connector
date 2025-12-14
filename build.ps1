# Build script for Mamo Connector with Installer
# Run this from the project root directory

param(
    [switch]$SkipBuild,
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  Mamo Connector Build Script" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Step 1: Build Release
if (-not $SkipBuild) {
    Write-Host "`n[1/3] Building release version..." -ForegroundColor Yellow
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Build failed!" -ForegroundColor Red
        exit 1
    }
    Write-Host "Build successful!" -ForegroundColor Green
}
else {
    Write-Host "`n[1/3] Skipping build (using existing)" -ForegroundColor Gray
}

# Step 2: Verify executable exists
Write-Host "`n[2/3] Verifying build output..." -ForegroundColor Yellow
$exePath = ".\target\release\mamo-connector.exe"
if (-not (Test-Path $exePath)) {
    Write-Host "ERROR: Executable not found at $exePath" -ForegroundColor Red
    exit 1
}
$exeInfo = Get-Item $exePath
Write-Host "Found: $($exeInfo.Name) ($([math]::Round($exeInfo.Length/1MB, 2)) MB)" -ForegroundColor Green

# Step 3: Create installer
if (-not $SkipInstaller) {
    Write-Host "`n[3/3] Creating installer..." -ForegroundColor Yellow
    
    # Check if Inno Setup is installed
    $innoSetupPaths = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
        "C:\Program Files\Inno Setup 6\ISCC.exe"
    )
    
    $isccPath = $null
    foreach ($path in $innoSetupPaths) {
        if (Test-Path $path) {
            $isccPath = $path
            break
        }
    }
    
    if ($null -eq $isccPath) {
        Write-Host "WARNING: Inno Setup not found!" -ForegroundColor Yellow
        Write-Host "Download from: https://jrsoftware.org/isdl.php" -ForegroundColor Cyan
        Write-Host ""
        Write-Host "After installing Inno Setup, run:" -ForegroundColor White
        Write-Host '  & "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" .\installer\mamo-connector.iss' -ForegroundColor Gray
    }
    else {
        Write-Host "Found Inno Setup at: $isccPath" -ForegroundColor Gray
        
        # Create output directory
        $outputDir = ".\target\installer"
        if (-not (Test-Path $outputDir)) {
            New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
        }
        
        # Run Inno Setup compiler
        & $isccPath ".\installer\mamo-connector.iss"
        
        if ($LASTEXITCODE -eq 0) {
            Write-Host "`nInstaller created successfully!" -ForegroundColor Green
            $installerPath = Get-ChildItem "$outputDir\*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($installerPath) {
                Write-Host "Output: $($installerPath.FullName)" -ForegroundColor Cyan
                Write-Host "Size: $([math]::Round($installerPath.Length/1MB, 2)) MB" -ForegroundColor Gray
            }
        }
        else {
            Write-Host "ERROR: Installer creation failed!" -ForegroundColor Red
            exit 1
        }
    }
}
else {
    Write-Host "`n[3/3] Skipping installer creation" -ForegroundColor Gray
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "  Build Complete!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Outputs:" -ForegroundColor White
Write-Host "  Executable: .\target\release\mamo-connector.exe" -ForegroundColor Gray
Write-Host "  Installer:  .\target\installer\MamoConnector-*-Setup.exe" -ForegroundColor Gray
