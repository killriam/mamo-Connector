# Full fresh-install end-to-end test: MaMo Connector + Forge, from a genuinely clean machine
# state, through to launching a real deck via the actual website.
#
# Drives everything a script can drive automatically (wipe, build, download-simulation,
# self-relocation) and pauses at the points that need a real human click (the setup wizard,
# Connect Connector, clicking Playtest on the site) — auto-detecting when each is done by
# polling settings.json / running processes, so you just follow the on-screen prompts instead
# of a script babysitting itself.
#
# This WIPES your current Connector settings/connection (that's the point — testing the fresh
# experience). A timestamped backup of settings.json is taken first; restore it yourself
# afterward with:
#   Copy-Item <printed backup path> "$env:APPDATA\MamoConnector\settings.json" -Force
# then re-run .\dev-build.ps1 to point the registry back at your debug build for normal dev work
# (this test's final state points the registry at the release build's relocated copy instead).
#
# Usage:
#   .\test-fresh-install.ps1 -DeckId <mamo-deck-uuid>

param(
    [Parameter(Mandatory=$true)]
    [string]$DeckId
)

$ErrorActionPreference = "Stop"
$settingsPath = Join-Path $env:APPDATA "MamoConnector\settings.json"

function Write-Step($n, $text) {
    Write-Host "`n[$n] $text" -ForegroundColor Cyan
}

function Wait-ForCondition($description, [scriptblock]$condition, $timeoutSeconds) {
    Write-Host "  Waiting for: $description (timeout: $([math]::Round($timeoutSeconds/60,1)) min)" -ForegroundColor Yellow
    $elapsed = 0
    while ($elapsed -lt $timeoutSeconds) {
        if (& $condition) {
            Write-Host "  Detected." -ForegroundColor Green
            return $true
        }
        Start-Sleep -Seconds 3
        $elapsed += 3
    }
    Write-Host "  Timed out waiting for: $description" -ForegroundColor Red
    return $false
}

function Get-SettingsValue($key) {
    if (-not (Test-Path $settingsPath)) { return $null }
    try {
        $json = Get-Content $settingsPath -Raw | ConvertFrom-Json
        return $json.$key
    } catch { return $null }
}

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  MaMo Connector - Fresh Install E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "This WIPES your current Connector settings/connection to test the brand-new-user" -ForegroundColor White
Write-Host "experience. It pauses where a real click is needed. Ctrl+C to abort anytime." -ForegroundColor White

# ── Step 0: Back up current settings before wiping anything ────────────────
if (Test-Path $settingsPath) {
    $backupPath = Join-Path $env:TEMP "mamo-settings-backup-$(Get-Date -Format 'yyyyMMdd-HHmmss').json"
    Copy-Item $settingsPath $backupPath
    Write-Host "`nBacked up current settings.json to:`n  $backupPath" -ForegroundColor Gray
}

# ── Step 1: Wipe to a clean slate ──────────────────────────────────────────
Write-Step 1 "Wiping current install (uninstall.ps1)"
& "$PSScriptRoot\uninstall.ps1"

# ── Step 2: Build the real release binary ──────────────────────────────────
Write-Step 2 "Building release binary"
Push-Location $PSScriptRoot
cargo build --release
if ($LASTEXITCODE -ne 0) { Write-Host "Build failed." -ForegroundColor Red; exit 1 }
Pop-Location

# ── Step 3: Simulate a real download and first run ──────────────────────────
Write-Step 3 "Simulating a real download (copy to Downloads, run from there)"
$downloadCopy = Join-Path $env:USERPROFILE "Downloads\mamo-connector-fresh-test.exe"
Copy-Item "$PSScriptRoot\target\release\mamo-connector.exe" $downloadCopy -Force
Write-Host "  Launching $downloadCopy ..." -ForegroundColor Gray
try {
    Start-Process -FilePath $downloadCopy
} catch {
    Write-Host "  Could not launch: $_" -ForegroundColor Red
    Write-Host "  Known issue: Smart App Control can hard-block a freshly-built unsigned exe" -ForegroundColor Yellow
    Write-Host "  with no override at all. If that's what happened here, this is a real finding" -ForegroundColor Yellow
    Write-Host "  about distribution, not a bug in the app - see the code-signing discussion." -ForegroundColor Yellow
    exit 1
}

$stableExe = Join-Path $env:LOCALAPPDATA "MamoConnector\app\mamo-connector.exe"
$relocated = Wait-ForCondition "self-relocation to $stableExe" { Test-Path $stableExe } 15
if (-not $relocated) { exit 1 }
Remove-Item $downloadCopy -Force -ErrorAction SilentlyContinue
Write-Host "  Deleted the original Downloads copy — mamoConnector:// links must keep working without it." -ForegroundColor Gray

# ── Step 4: Human step — the setup wizard ───────────────────────────────────
Write-Step 4 "SETUP WIZARD — your turn"
Write-Host "  The Connector window should be open now. Please:" -ForegroundColor White
Write-Host "    1. Click 'Get Started ->' (the Forge download should start immediately, no" -ForegroundColor White
Write-Host "       second click needed)" -ForegroundColor White
Write-Host "    2. Wait for the ~100-300MB download to finish" -ForegroundColor White
Write-Host "    3. Click through Configure Forge -> Test Launch -> Done" -ForegroundColor White
$wizardDone = Wait-ForCondition "forge_path to be set in settings.json" { Get-SettingsValue "forge_path" } 900
if (-not $wizardDone) { exit 1 }

# ── Step 5: Human step — Connect to MaMo ────────────────────────────────────
Write-Step 5 "CONNECT TO MAMO — your turn"
Write-Host "  On the real MaMo site, click 'Connect Connector' (profile menu, top-right)." -ForegroundColor White
$connected = Wait-ForCondition "auth_token to be set in settings.json" { Get-SettingsValue "auth_token" } 180
if (-not $connected) { exit 1 }

# ── Step 6: Human step — launch a deck from the real frontend ──────────────
Write-Step 6 "LAUNCH A DECK FROM THE WEBSITE — your turn"
Write-Host "  Open deck $DeckId's Evaluation tab on the real site and click 'Playtest in Forge'." -ForegroundColor White
$launched = Wait-ForCondition "Forge (javaw.exe) to start" { Get-Process -Name 'javaw' -ErrorAction SilentlyContinue } 120
if (-not $launched) { exit 1 }

Start-Sleep -Seconds 2
$proc = Get-CimInstance Win32_Process -Filter "Name='javaw.exe'" -ErrorAction SilentlyContinue
$deckSelected = $proc -and ($proc.CommandLine -match '--deck "')

Write-Host "`n========================================" -ForegroundColor Cyan
if ($deckSelected) {
    Write-Host "  PASS — Forge launched with a deck actually selected:" -ForegroundColor Green
    Write-Host "  $($proc.CommandLine)" -ForegroundColor Gray
} else {
    Write-Host "  FAIL — Forge started but no --deck argument was found" -ForegroundColor Red
}
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "`nTo restore your previous dev setup: copy the backup printed above back over" -ForegroundColor Gray
Write-Host "settings.json, then run .\dev-build.ps1 to re-point the registry at the debug build." -ForegroundColor Gray
