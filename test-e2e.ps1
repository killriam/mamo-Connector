# End-to-end smoke test for MaMo Connector
#
# Automates the checks that previously had to be done by hand (fire a deeplink, inspect the
# resulting Forge process). Useful as a quick regression check after touching forge.rs,
# commands.rs, or ui.rs's deeplink routing.
#
# Usage:
#   .\test-e2e.ps1 -DeckId <mamo-deck-uuid>              # test playtest with a real deck
#   .\test-e2e.ps1 -DeckId <uuid> -Action simulate        # test the simulate deeplink instead
#   .\test-e2e.ps1                                        # deck-less checks only (no download)
#
# What it checks:
#   1. settings.json parses as valid JSON (a parse failure silently breaks auth/most commands)
#   2. mamoConnector:// is registered and points at an existing exe
#   3. A deck-less playtest/launch-forge deeplink switches to the Home tab picker instead of
#      launching Forge with nothing loaded (regression check for the deckless-routing fix)
#   4. (If -DeckId given) A real playtest deeplink downloads the deck and launches Forge with
#      --deck "<deck name>" actually present in the spawned java process's command line —
#      the core fix this test exists to catch regressions in.
#
# This does NOT test: the first-run wizard (Get Started click, Forge auto-download), the
# self-relocating install (release builds only), or the account-deck picker UI — those need a
# human clicking through the GUI. See MANUAL_TESTING.md for those.

param(
    [string]$DeckId,
    [ValidateSet("playtest", "simulate", "launch-forge")]
    [string]$Action = "playtest"
)

$ErrorActionPreference = "Stop"
$pass = 0
$fail = 0

function Test-Step($name, [scriptblock]$check) {
    Write-Host "`n[TEST] $name" -ForegroundColor Cyan
    try {
        $result = & $check
        if ($result) {
            Write-Host "  PASS" -ForegroundColor Green
            $script:pass++
        } else {
            Write-Host "  FAIL" -ForegroundColor Red
            $script:fail++
        }
    } catch {
        Write-Host "  FAIL: $_" -ForegroundColor Red
        $script:fail++
    }
}

function Stop-ConnectorAndForge {
    Get-Process -Name 'mamo-connector', 'javaw' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  MaMo Connector - End-to-End Smoke Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Stop-ConnectorAndForge

# ── 1. settings.json is valid JSON ──────────────────────────────────────────
Test-Step "settings.json parses as valid JSON" {
    $path = Join-Path $env:APPDATA "MamoConnector\settings.json"
    if (-not (Test-Path $path)) {
        Write-Host "  (no settings.json yet - counts as pass, nothing to corrupt)" -ForegroundColor Gray
        return $true
    }
    $null = Get-Content $path -Raw | ConvertFrom-Json
    return $true
}

# ── 2. Protocol registration points at an exe that actually exists ──────────
Test-Step "mamoConnector:// is registered and target exe exists" {
    $key = Get-ItemProperty -Path "HKCU:\Software\Classes\mamoConnector\shell\open\command" -ErrorAction Stop
    $command = $key.'(default)'
    Write-Host "  Registered command: $command" -ForegroundColor Gray
    if ($command -match '"([^"]+\.exe)"') {
        $exePath = $matches[1]
        return Test-Path $exePath
    }
    return $false
}

# ── 3. Deck-less evaluation deeplink routes to the picker, not a blank Forge launch ──
Test-Step "Deck-less '$Action' deeplink does not launch Forge with nothing loaded" {
    Start-Process "mamoConnector://$Action"
    Start-Sleep -Seconds 4
    $forgeRunning = Get-Process -Name 'javaw' -ErrorAction SilentlyContinue
    Stop-ConnectorAndForge
    if ($forgeRunning) {
        Write-Host "  Forge launched with no deck selected - this should not happen" -ForegroundColor Red
        return $false
    }
    return $true
}

# ── 4. Real deck playtest actually pre-selects the deck in Forge ────────────
if ($DeckId) {
    Test-Step "'$Action/$DeckId' downloads the deck and passes --deck to Forge" {
        Start-Process "mamoConnector://$Action/$DeckId"

        $deckArg = $null
        for ($i = 0; $i -lt 20; $i++) {
            Start-Sleep -Seconds 1
            $proc = Get-CimInstance Win32_Process -Filter "Name='javaw.exe'" -ErrorAction SilentlyContinue
            if ($proc) {
                $deckArg = $proc.CommandLine
                break
            }
        }

        Stop-ConnectorAndForge

        if (-not $deckArg) {
            Write-Host "  No javaw.exe process ever appeared (deck download or launch failed - check Activity Log)" -ForegroundColor Red
            return $false
        }
        Write-Host "  Forge command line: $deckArg" -ForegroundColor Gray
        return $deckArg -match '--deck "'
    }
} else {
    Write-Host "`n[SKIP] Real-deck playtest test (pass -DeckId <uuid> to run it)" -ForegroundColor Yellow
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "  Results: $pass passed, $fail failed" -ForegroundColor $(if ($fail -eq 0) { "Green" } else { "Red" })
Write-Host "========================================" -ForegroundColor Cyan

if ($fail -gt 0) { exit 1 }
