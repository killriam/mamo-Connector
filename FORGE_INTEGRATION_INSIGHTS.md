# Forge Integration — Technical Insights

Accumulated learnings from integrating mamo-Connector with the Forge MTG desktop application.
Use this as a reference when debugging, extending, or onboarding to the Forge integration.

---

## 1. Forge Process Architecture

### The Launcher → Java Pattern

`forge.exe` is **not** the game. It is a thin launcher that:
1. Sets up the JVM environment
2. Spawns `java.exe` (the real Forge process) with the fat JAR
3. **Exits almost immediately** after spawning Java

This creates a two-phase process lifetime:

```
t=0s   forge.exe starts       → PID tracked by mamo-Connector
t=1s   forge.exe exits        → PID goes dead (launcher done)
t=5s   java.exe starts        → spawns a window titled "Forge ..."
t=30s  Forge GUI fully loaded → window fully interactive
```

**Critical implication**: once the launcher exits, `is_process_running(pid)` returns `false`.
The real game is still starting up. Never interpret launcher-PID death as "Forge closed".

### Detection Strategy (Two-Phase)

mamo-Connector uses a two-phase approach in `ui.rs`:

| Phase | Mechanism | Notes |
|-------|-----------|-------|
| Phase 1 | Track launcher PID (`forge_pid`) | PID valid for ~1 s |
| Phase 2 | Scan visible window titles (`is_forge_window_open`) | Looks for `"Forge"` substring in title |

`forge_alive = pid_alive || window_open`

The launcher PID is cleared once it exits (it was just a helper). The window check then
becomes the sole liveness signal for the duration of the game.

---

## 2. Window-Based Liveness Detection

### Implementation

`is_forge_window_open()` in `forge.rs` calls the Win32 `EnumWindows` API, iterating all
visible top-level window handles and checking titles for the substring `"Forge"`.

```rust
// forge.rs
pub fn is_forge_window_open() -> bool { ... }
```

**Platform support**: Windows only. Returns `false` on macOS/Linux (no implementation yet).

### Known Pitfall: Launcher Window vs. Game Window

While `forge.exe` is still alive, it may also briefly show (or own) a window that contains
the word "Forge". Setting `forge_window_seen = true` at that point is wrong — it reduces
the post-close grace period from 120 s to 20 s **before Java has started**.

**Fix applied (2026-03-18):** `forge_window_seen` is only set to `true` after the launcher
PID has already died (`!pid_alive`). This ensures the flag reflects the real Java game
window, not the launcher:

```rust
// Only count the window after the launcher PID has exited
if window_open && !pid_alive {
    self.forge_window_seen = true;
}
```

---

## 3. Grace Periods for Close Detection

Because Java startup can be slow (especially on first run or low-end machines), premature
"Forge closed" signals must be suppressed.

| Scenario | Grace period | Rationale |
|----------|-------------|-----------|
| Forge window never seen | **120 s** | Java still starting up; launcher already gone |
| Forge window was seen at least once | **20 s** | Real game was open; short delay before final scan |

The `forge_window_seen` flag (on `MamoConnectorApp`) persists across poll ticks and is
reset to `false` when monitoring ends.

---

## 4. Deck Delivery to Forge

Forge does **not** support a `--deck <path>` command-line argument when launched via
`forge.exe`. The only reliable delivery mechanism is:

1. Download the deck and write it to Forge's deck folder:
   `%APPDATA%\Forge\decks\commander\<deck-name>.dck`
2. Launch Forge normally — the deck appears in the Commander deck list

The `.dck` format is plain text, one card per line:
```
[metadata]
Name=My Deck
...
[Commander]
1 Atraxa, Praetors' Voice
[Main]
1 Sol Ring
...
```

When launched via JAR directly (`forge-gui-desktop-*.jar`), the `--deck` arg **does** work:
```
java -Xmx4096m -jar forge-gui-desktop-*.jar --deck /path/to/deck.dck
```

---

## 5. Path Resolution

### Executable Types

| Extension | Launch method | Deck arg supported |
|-----------|--------------|-------------------|
| `.jar` | `java -jar` | ✅ `--deck <path>` |
| `.exe` | Direct spawn, `DETACHED_PROCESS`, cwd = forge dir | ❌ |
| `.bat` / `.cmd` | Direct spawn, cwd = forge dir | ❌ |
| `.sh` | Shell script, cwd = forge dir | ✅ (passed through) |
| `.app` | `open <path> --args` (macOS) | ✅ (passed through) |

**Working directory is critical for `.exe`**: Forge looks for its JAR and config relative
to its own directory. Always set `cmd.current_dir(forge_dir)`.

### Directory Configuration

If a **directory** path is configured instead of a specific executable, the Connector
resolves to the newest `forge-gui-desktop-*-jar-with-dependencies.jar` inside it
(`resolve_latest_forge_jar`). Useful when Forge is updated frequently.

### Auto-Discovery Order (Windows)

1. `%LOCALAPPDATA%\Forge\forge.exe`
2. `C:\Program Files\Forge\forge.exe`
3. `C:\Program Files (x86)\Forge\forge.exe`
4. `~\Forge\forge.exe`
5. Desktop `\Forge\forge.exe`
6. Documents `\Forge\forge.exe`

---

## 6. Game Log Lifecycle

### Default Log Location

```
Windows: %APPDATA%\Forge\games\gamelogs\
macOS/Linux: ~/Forge/games/gamelogs/
```

Files are `.json` in MTG Replay Notation format.

### Auto-Scan Triggers

| Event | Action |
|-------|--------|
| Forge detected as running | Start periodic scan (every 5 min) |
| Forge detected as closed | Trigger one final scan |
| Manual button in UI | Immediate scan |

### Duplicate Prevention

Processed filenames are tracked in `GameLogWatcherState.processed_files` (`HashSet<String>`).
A SHA-256 checksum is also sent to the backend; the DB enforces `UNIQUE(user_id, checksum)`.

### Upload Payload

```json
{
  "filename": "game_2025-01-15_143052.json",
  "content": "...(raw JSON content)...",
  "file_size": 15234,
  "modified_timestamp": 1736951452,
  "checksum": "sha256:abc123..."
}
```

---

## 7. Common Failure Modes

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| "Forge closed" fires ~24 s after launch | `forge_window_seen` set prematurely from launcher window | Guard with `!pid_alive` (fixed 2026-03-18) |
| Forge never detected as running | `is_forge_window_open` returns false | Check window title contains "Forge"; on non-Windows always returns false |
| Deck not appearing in Forge | Deck written to wrong directory or wrong format | Verify path is `%APPDATA%\Forge\decks\commander\` |
| Double upload of same game | Race between periodic scan and close-trigger scan | Backend deduplicates by checksum; safe to ignore |
| Java not found for `.jar` launch | `java` not on PATH | Ensure JRE/JDK is installed and on system PATH |
| Forge exits silently on launch | Working directory not set | Always set `cmd.current_dir(forge_dir)` |
