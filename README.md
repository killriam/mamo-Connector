# Mamo Connector Launcher

This Rust desktop helper registers the custom `mamoConnector://` URI scheme and displays any
incoming deep-link arguments in a simple native UI. It is intended as the groundwork for a
secure launcher that can be invoked from the browser and expanded with action handling logic.

## Features

- Cross-platform custom protocol registration (`mamoConnector://`).
- Displays raw command-line arguments and parsed deep-link metadata (action, parameters,
  document ID, token) in a native window.
- Graceful logging with automatic fallbacks if registration is not supported or fails.
- Linux support writes a `.desktop` file and invokes `xdg-mime` to register the handler.
- macOS support requests LaunchServices to bind the scheme to the current bundle identifier.
- Windows support populates the per-user registry keys under `HKCU\\Software\\Classes`.

## Building

```bash
cargo build --release
```

The resulting binary can be found at `target/release/mamo-connector` (or
`mamo-connector.exe` on Windows).

## Running Locally

Execute the binary directly. The window will report whether registration succeeded and list
any arguments that were passed. To simulate a deep link, pass it on the command line:

```bash
./target/release/mamo-connector "mamoConnector://open?doc=123&token=abc123"
```

## Platform Notes

### Windows

- Registration writes to `HKCU\Software\Classes\mamoConnector` so administrative privileges are
  not required.
- Unregister the scheme by removing the key or running the binary with uninstall logic (to be
  implemented in a future iteration).

### macOS

- The executable must be part of a bundled app with a valid bundle identifier in order for
  LaunchServices to accept the registration call.
- Ensure the bundle also declares the scheme in its `Info.plist` for best compatibility.

### Linux

- The launcher creates `~/.local/share/applications/mamoConnector.desktop` (or the equivalent
  XDG data directory) and attempts to call `xdg-mime` and `update-desktop-database`.
- If these tools are unavailable, the file is still created and the app reports that manual
  steps may be required.

## Next Steps

- Replace the placeholder UI with navigation into the full application experience.
- Integrate secure short-lived token validation and document loading.
- Implement uninstall/cleanup flows for each platform.
- Extend the deep-link router beyond the initial `open` action.
