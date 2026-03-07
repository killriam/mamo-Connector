# MaMo Connector — Copilot Instructions

> Repo-specific instructions for the `mamo-Connector` desktop application.
> See the workspace-level `.github/copilot-instructions.md` for cross-repo architecture.

## Project Overview

Native desktop application (Windows/macOS/Linux) that bridges the MaMo web application
and MTG Forge (game engine). Handles custom URI schemes (`mamoConnector://`), creates
Forge-compatible deck files, imports decks from multiple sources, and monitors game logs
for automatic upload.

## Tech Stack

- **Language**: Rust (2024 edition)
- **UI framework**: eframe/egui (immediate-mode GUI)
- **HTTP client**: reqwest (async, with json feature)
- **Async runtime**: tokio (full features)
- **Serialization**: serde + serde_json
- **File watching**: notify (cross-platform filesystem events)
- **File dialog**: rfd (native file dialogs)
- **Logging**: `log` crate + `env_logger` backend
- **Platform-specific**:
  - Windows: `winreg` (registry), `winapi`
  - macOS: `core-foundation` (LaunchServices)
- **Hashing**: sha2 (checksum verification)
- **Build**: cargo + build.rs (git info embedding)
- **Installer**: Inno Setup (Windows), WiX (MSI alternative)

## Project Structure

```
src/
├── main.rs          # Entry point — single-instance check, URI scheme, app launch
├── ui.rs            # eframe/egui UI (3292 lines) — tabs, forms, status panels
├── commands.rs      # Deeplink command dispatcher (851 lines)
├── deck.rs          # Deck creation from multiple sources (2887 lines)
│                      Moxfield, MaMo API, Archidekt, Deckstats → Forge format
├── deeplink.rs      # mamoConnector:// URL parser (275 lines)
├── forge.rs         # Forge game engine detection & launch (441 lines)
├── gamelog.rs       # Game log file watcher & uploader (1142 lines)
├── registration.rs  # Cross-platform URI scheme registration (207 lines)
└── settings.rs      # Persistent JSON settings in config dir (300 lines)

installer/
└── mamo-connector.iss   # Inno Setup script (Windows installer)

wix/
├── main.wxs             # WiX MSI installer definition
└── License.rtf          # License for installer

build.ps1                # PowerShell build script (release build + installer)
build.rs                 # Cargo build script (embeds git hash, branch, timestamp)
```

## Cross-Repo Dependencies

- **`new-backend`**: Fetches deck data via REST API, uploads game logs. Uses JWT and PAT
  authentication. API contract defined by `@killriam/mamo-types` (referenced, not imported)
- **`mtg-replay-notation`**: Game log files follow this JSON specification. The gamelog module
  validates and uploads files matching the replay notation schema
- **`MaMoFrontend`**: Frontend generates `mamoConnector://` deeplinks that the Connector handles
- **Forge (external)**: MTG game engine — Connector creates `.dck` deck files in Forge's format
  and monitors Forge's game log directory

## Domain Knowledge

### URI Scheme (`mamoConnector://`)

The app registers `mamoConnector://` as a custom URI handler on all platforms:
- **Windows**: HKCU registry key
- **macOS**: LaunchServices via Info.plist
- **Linux**: `.desktop` file + xdg-mime

Deeplink format: `mamoConnector://action?param1=value1&param2=value2&token=JWT`

Supported actions:
- `deck` — create a Forge deck file from MaMo deck ID
- `mamo` — create deck from MaMo API data
- `playtest` — create deck + launch Forge
- `launch-forge` / `launchforge` — start Forge
- `user` — import user's decks from Moxfield/Archidekt

### Forge Deck Format

Forge `.dck` files use a specific text format:
```
[metadata]
Name=Deck Name
[Main]
1 Card Name|SET
1 Another Card|SET
[Commander]
1 Commander Name|SET
```

### Deck Import Sources

| Source | Method | Module |
|--------|--------|--------|
| MaMo API | REST API with JWT/PAT auth | `deck.rs` |
| Moxfield | Public API scraping | `deck.rs` |
| Archidekt | Public API | `deck.rs` |
| Deckstats | Public API | `deck.rs` |

### Game Log Monitoring

The gamelog module watches a configurable directory for `.json` files matching the
MTG Replay Notation spec. When a new file appears:
1. Parse and validate the JSON structure
2. Compute SHA-256 checksum
3. Upload to backend via `POST /api/gamelogs/upload` with PAT auth
4. Track processing status (WatcherStatus enum)

### Single-Instance Model

Only one Connector instance runs at a time:
- Lock file in config directory prevents multiple instances
- If already running, new deeplink args are sent to the existing instance via file
- The running instance polls for new deeplink requests

## Development Guidelines

### Rust Conventions

- **Edition 2024** — use latest Rust idioms
- **Error handling**: Use `Result<T, E>` with descriptive error types, no `unwrap()` in
  production code paths. `unwrap()` only in tests and provably safe contexts
- **Async**: tokio runtime for HTTP requests and file watching. Use `async/await`, not
  manual `Future` implementations
- **Logging**: Use `log` crate macros (`info!`, `warn!`, `error!`, `debug!`) with `env_logger`.
  Initialize in `main.rs` with `env_logger::init()`. Do NOT use raw `println!` for status output.

### Code Patterns

**HTTP requests (reqwest):**
```rust
let client = reqwest::Client::new();
let response = client
    .get(&format!("{}/api/deck/{}", base_url, deck_id))
    .header("Authorization", format!("Bearer {}", token))
    .send()
    .await?;

if response.status().is_success() {
    let deck: DeckResponse = response.json().await?;
    // ...
} else {
    return Err(format!("API error: {}", response.status()));
}
```

**egui UI patterns:**
```rust
impl eframe::App for MamoConnectorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MaMo Connector");
            // Tab bar, content panels, etc.
        });
    }
}
```

**Settings persistence:**
```rust
// Settings stored as JSON in platform config directory
// Windows: %APPDATA%/mamo-connector/settings.json
// macOS: ~/Library/Application Support/mamo-connector/settings.json
// Linux: ~/.config/mamo-connector/settings.json
```

### Platform Considerations

- Use `cfg!(target_os = ...)` for platform-specific code
- Windows paths use `\\`, handle with `std::path::PathBuf`
- Registry operations (Windows) must handle permission errors gracefully
- Forge path detection covers common install locations on all 3 platforms
- Test cross-platform compatibility when modifying registration or forge modules

### Build & Release

```powershell
# Development build
cargo build

# Release build
cargo build --release

# Full release with installer (Windows)
.\build.ps1
```

`build.rs` embeds version info at compile time:
- `GIT_HASH`: Current commit hash
- `GIT_BRANCH`: Current branch
- `GIT_DIRTY`: Whether working tree has uncommitted changes
- `BUILD_TIMESTAMP`: ISO 8601 build time

## Testing

```bash
cargo test              # Run all tests
cargo test -- --nocapture  # With stdout output
```

Tests use `#[cfg(test)]` modules. Mock HTTP responses where possible.
Integration tests may need a running backend instance.

## Documentation Policy

- **No markdown docs** unless explicitly requested
- Use `///` doc comments on public functions and types
- Keep `README.md` updated with feature list and setup instructions

## Environment & Deployment

### Required Environment
- Rust toolchain (stable, latest)
- For Windows installer: Inno Setup 6.x on PATH

### Configuration
The app stores persistent settings in the platform's standard config directory.
No `.env` file needed — configuration is managed through the UI settings tab.

### Authentication
- JWT tokens received via deeplinks (from frontend)
- PAT tokens stored in settings for persistent auth (game log upload)

## AI Agent Guidelines

1. **No `unwrap()` in production paths** — always handle errors explicitly
2. **Platform-aware code** — use `cfg!` macros, test mentions of paths and registries
3. **Forge format is strict** — `.dck` file format must match Forge's parser expectations
4. **File watching is async** — gamelog module runs on tokio, avoid blocking operations
5. **Single-instance matters** — don't break the lock file mechanism
6. **Test on Windows first** — primary platform, then verify macOS/Linux
7. **Keep dependencies minimal** — this is a desktop app, bundle size matters
8. **Deeplink parsing must be robust** — URLs come from web browsers, expect malformed input
9. **Settings migration** — when adding new settings fields, handle old settings files gracefully
10. **English only** — all code, comments, and documentation
