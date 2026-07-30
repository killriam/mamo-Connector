# Manual testing checklist

`test-e2e.ps1` covers everything scriptable (deeplink routing, the `--deck` launch fix, settings
integrity, protocol registration). The steps below need a human clicking through the actual GUI —
things no script can drive.

Run `.\uninstall.ps1` before starting to guarantee a clean slate. Use `-KeepForgeCache` on repeat
runs so you're not re-downloading the ~250MB Forge jar every time.

## 1. First run (release build only — debug builds skip self-relocation)

- [ ] `cargo build --release`, copy `target\release\mamo-connector.exe` to your actual Downloads
      folder (not run in place — the point is testing it like a real download).
- [ ] Double-click it from Downloads.
      - **Known issue**: Smart App Control may hard-block a freshly-built, unsigned exe with no
        "run anyway" option (seen 2026-07-30). If it's blocked, that's a real finding to report,
        not a test failure to work around — see the conversation notes on code signing.
- [ ] Confirm the file disappears from Downloads and a copy is now running from
      `%LOCALAPPDATA%\MamoConnector\app\mamo-connector.exe`.
- [ ] Delete the original Downloads copy. Confirm `mamoConnector://playtest/<any-deck-id>` still
      opens the app (the whole point of self-relocation).

## 2. Setup wizard

- [ ] Welcome screen → click "Get Started →".
- [ ] Confirm the Forge download progress bar starts **immediately**, no second click needed
      (unless a jar is already cached, in which case you should land on "✓ MaMo Forge already
      downloaded" instead).
- [ ] Let it finish (or click "I already have Forge →" if you want to skip and point at an
      existing install).
- [ ] Configure Forge step → Test Launch → Done.

## 3. Connect to MaMo

- [ ] On the real site, click "Connect Connector" (profile menu, or the install prompt).
- [ ] Confirm the Connector shows "● Connected to MaMo" on the Home tab shortly after.
- [ ] Home tab's "Deck:" dropdown should populate with your account's decks automatically —
      no manual "Load my decks" trip to the Decks tab needed.

## 4. Deck picker (Home tab)

- [ ] Pick a deck you haven't played before (labeled "(download)" in the dropdown).
- [ ] Click "🎮 Launch Forge" — confirm it downloads then launches, with that deck actually
      pre-selected in Forge (not just an empty Forge window).

## 5. Playtest from the website

- [ ] Open a deck's Evaluation tab, click "Playtest in Forge".
- [ ] Confirm Forge opens with **that exact deck** already selected — this is the bug that
      prompted most of today's work; it must not regress.

## 6. Forge update banner

- [ ] Only testable once a newer `killriam/forge` build is published than what you have locally.
      To force it: replace the jar in `%APPDATA%\MamoConnector\forge\` with an older one, then
      relaunch — the "⬆ MaMo Forge update available" banner should appear on the Home tab.
- [ ] Click "Update" — confirm it re-downloads in place (no wizard, no settings changes) and the
      banner clears on success.

## 7. Download Connector link (frontend)

- [ ] With the Connector not detected, open the install prompt (Evaluation tab or Playtest
      button) — the "Download Connector" link/button should go straight to a `.exe`, not
      GitHub's release list page.
