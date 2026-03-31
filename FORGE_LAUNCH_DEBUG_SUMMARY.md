# Forge Launcher Debug Summary

## Problem

When invoking the Forge launcher via the mamo-Connector deeplink path (mamoConnector://), Forge often did not appear and only an empty terminal window was shown. The connector attempted to use a path that could be a directory or direct executable, and the old logic skipped launching if Forge seemed already-running.

## Root Causes Identified

- `launch_forge()` path handling did not explicitly prefer script wrappers (`forge.cmd`, `forge.sh`), which provide console output and better error diagnostics.
- `debug!()` logs were not being displayed because logging initialization or `RUST_LOG` level was not set to `debug`.
- The process creation logic used `DETACHED_PROCESS` (Windows) but lacked visible stdout/stderr capture for wrappers.
- Protocol handler registration and app path resolution were misaligned in previous setup.

## Changes Applied

1. Added robust debug logging to `src/forge.rs`:
   - Input values
   - Path resolution flow
   - Extension-specific command construction
   - Working directory information
   - Process PID and spawn errors

2. Updated `launch_forge` to prefer wrapper scripts when launching from a Forge folder:
   - `forge.cmd` (Windows)
   - `forge.sh` (Unix)
   - falls back to latest `forge-gui-desktop` JAR when no wrappers exist

3. Ensured `debug` macro is imported from `log`.

4. Rebuilt in debug mode:
   - `cargo build -j 4`
   - Tested launcher `target\debug\mamo-connector.exe` with `RUST_LOG=debug`

## How to run with debug logs

In PowerShell (mamo-Connector folder):

```powershell
$env:RUST_LOG = "debug"
target\debug\mamo-connector.exe
```

## Notes

- If no output appears, verify logger initialization in `main.rs` (e.g., `env_logger::init();`).
- If `forge.cmd`/`forge.sh` print data, the new code path ensures that the connector uses them and you can inspect errors directly.
