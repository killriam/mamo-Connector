# Forge Replay Launching Bug & Fix Details

> **ACTUALLY RESOLVED — confirmed 2026-08-15, corrects the addendum directly below.** The Forge
> side traced this precisely: `replay-features-latest` as published 2026-08-14T08:55:25Z (built
> from commit `0c4e5c556df`) genuinely contains the `case "replay"` patch — confirmed by direct
> binary inspection, not by trusting the release notes' "Built from" text. The build this repo's
> addendum tested (file-dated 2026-08-13 20:20) was from the very first push of the fix
> (`2b0beea`) and genuinely didn't have it yet; a second release went out the next day from a
> later commit and that one does. So there was no CI/distribution-pipeline problem as speculated
> below — just an old local jar that predated the fix by about half a day, on a rolling tag with
> no way to ask for "the version from before" once a newer one replaces it.
>
> **The real remaining bug was entirely on the mamo-Connector side**, and *is* now fixed here:
> `check_forge_update_available` (`src/ui.rs`) compared the local and remote jar by **filename
> only**. Since `replay-features-latest`'s asset name is a fixed `-SNAPSHOT-` string that never
> changes between builds, that comparison could **never** detect a same-named republish — it's
> why the fixed 2026-08-14 build sat published for a full day while this install's stale
> 2026-08-13 jar was never flagged as out of date, and would have stayed silently stale
> indefinitely. Fixed by recording each download's GitHub `updated_at` asset timestamp in a
> sidecar file (`download.rs`'s `write_asset_meta`/`read_asset_meta_updated_at`,
> `<jar>.source.json`) and comparing *that* instead of the filename. A jar with no sidecar
> (downloaded before this fix, or a user-provided Forge path) is treated as "unknown, might be
> stale" rather than silently assumed current. Regression-covered by
> `download::tests::asset_meta_round_trips_through_sidecar_file` /
> `asset_meta_is_none_when_sidecar_missing`.
>
> **Anyone still hitting the original crash just needs a fresh download** — delete the local jar
> (or use the Settings tab's Forge update flow, once it picks up the corrected check above) so
> `download_forge_jar`/`download_forge_portable` re-fetches the current, genuinely-patched build.

> **REGRESSED IN THE DISTRIBUTED BUILD — 2026-08-15.** Despite the "RESOLVED" note directly
> below, a real replay-mode launch on a live install still failed exactly as originally
> described: the Connector logged `"Forge launched in replay mode."` (green checkmark, PID
> returned) but no Forge window ever appeared.
>
> **Root cause, verified directly:** mamo-Connector downloads Forge from
> `https://api.github.com/repos/killriam/forge/releases/tags/replay-features-latest`
> (`src/download.rs`'s `FORGE_RELEASES_API`) — a rolling release tag that's supposed to always
> be the latest build of the `replay-Features` branch. The jar actually installed
> (`forge-gui-desktop-2.0.14-SNAPSHOT-jar-with-dependencies.jar`, file-dated 2026-08-13 20:20 —
> suspiciously close to the "resolved" timestamp above) does **not** contain the `case "replay"`
> patch: extracting `forge/view/Main.class` from it and scanning the compiled bytecode finds
> the literal strings `"Unknown mode"` and `"Known modes"` (this build's fallback-error path —
> the exact text from the "The Problem" section below) but **zero** occurrences of the string
> `"replay"` anywhere in the class. So `case "replay"` was never compiled into whatever built
> this release asset — either the fix commit never reached the branch that
> `replay-features-latest`'s CI builds from, the CI run that should have picked it up didn't
> fire/succeeded before the fix landed, or the release tag wasn't re-pointed at a newer build
> after the fact. Which of those it is can't be determined from the mamo-Connector repo alone —
> needs checking in the `killriam/forge` repo's Actions history for that tag.
>
> **Effect:** launching `java -jar forge-....jar replay <path>` against this build hits
> `Main.java`'s default case, prints `Unknown mode. Known modes are 'sim', 'parse', 'gui'.` to
> stderr, and the JVM exits within milliseconds — before any window opens. mamo-Connector's own
> `cmd.spawn()` still returns `Ok` with a real PID (the OS *did* start the process; it just died
> instantly), and — until the fix below — nothing checked whether that process was still alive
> a moment later or looked at what it printed to stderr, so every caller reported success anyway.
> **This will keep happening for every replay-mode launch until a `replay-features-latest`
> release actually containing the `Main.java` patch is published** — no mamo-Connector-side
> change can fix a Forge build that's missing the CLI handling entirely.
>
> **mamo-Connector-side fix applied 2026-08-15** (independent of the above, and worth keeping
> regardless of when/whether the Forge-side release gets corrected): `launch_forge` and
> `launch_forge_replay` (`src/forge.rs`) now go through a shared `spawn_and_verify` helper that
> pipes the child's stderr and waits a short grace period (400ms) before declaring success,
> `try_wait()`-checking whether the process already exited. A crash in that window is now
> reported as a real failure carrying Forge's actual stderr text (e.g. the "Unknown mode..."
> line above), instead of a false "launched" success with the error silently discarded. This
> turns *any* future CLI-arg mismatch — not just this specific replay-mode case — into a
> visible, honest error in the Activity log.

> **RESOLVED 2026-08-13.** Implemented on both sides (Forge `replay-Features` branch,
> mamo-Connector `src/forge.rs`) and verified end-to-end with a real CLI launch against an
> actual replay log: no crash, deck/library reconstruction ran, and the interactive match
> screen opened correctly (Commander-upgrade fallback and forced draw-order reorder both
> fired as designed). One correction to the plan below: `Main.java`'s CLI `case "replay"`
> stores the path and calls `startGui()`, which — after `Singletons.getControl().initialize()`
> completes, not before — calls `CSubmenuReplay.SINGLETON_INSTANCE.startReplayFromPath(path)`
> directly (wrapped in `SwingUtilities.invokeLater`, required or a live `IllegalStateException`
> is thrown deep in match-screen Swing construction, which asserts EDT). This was simpler and
> more reliable than routing through `CSubmenuReplay.setPendingReplayPath()` +
> `update()`, which only auto-fires if the Replay Game submenu happens to be the user's
> last-selected tab (persisted preference) — not guaranteed for a fresh CLI/deeplink launch.

## The Problem

When starting a game replay via the `mamoConnector://replay-game` deeplink, Forge fails to start and exits with the following output:
```
Unknown mode.
Known modes are 'sim', 'parse', 'gui'.
```

This occurs because:
1. The CLI `replay` mode is documented but is **completely unimplemented** in the command-line parsing logic inside Forge's [Main.java](file:///C:/SWProjects/Forge/forge-gui-desktop/src/main/java/forge/view/Main.java).
2. The connector attempted to work around this by prepending `"gui"` (running `gui replay <path>`), but `"replay"` is not recognized as a GUI option either, throwing:
   `Unknown GUI option: replay`
3. Referencing `CSubmenuReplay` directly during parsing in `Main.java` triggers view class loading before singletons and skin assets are initialized, causing a `NullPointerException` crash regarding `IMG_BTN_START_OVER`.

---

## How to Fix It

### 1. In Forge Repository

File: [Main.java](file:///C:/SWProjects/Forge/forge-gui-desktop/src/main/java/forge/view/Main.java)

1. Add `pendingReplayPath` to the `GuiLaunchOptions` class (around line 42):
   ```java
   private static final class GuiLaunchOptions {
       private String playerOneDeck;
       private String playerTwoDeck;
       private GuiDeckFormat format = GuiDeckFormat.COMMANDER;
       private String pendingReplayPath; // <-- Add this
   }
   ```

2. Add `case "replay"` inside `Main.java`'s switch statement (around line 135) to parse the CLI mode:
   ```java
   case "replay":
       if (args.length < 2) {
           System.err.println("Error: Missing replay file path.");
           System.exit(1);
       }
       final GuiLaunchOptions replayOptions = new GuiLaunchOptions();
       replayOptions.pendingReplayPath = args[1];
       startGui(replayOptions);
       break;
   ```

3. Update `startGui(options)` to set the pending replay path **after** the GUI and skins have finished initialization (around line 170) to prevent the classloading crash:
   ```java
   private static void startGui(final GuiLaunchOptions options) {
       Singletons.initializeOnce(true);

       if (options != null) {
           applyGuiLaunchOptions(options);
           if (options.pendingReplayPath != null) {
               forge.screens.home.replay.CSubmenuReplay.setPendingReplayPath(options.pendingReplayPath);
           }
       }

       Singletons.getControl().initialize();
   }
   ```

### 2. In Mamo Connector Repository

File: [forge.rs](file:///c:/SWProjects/MaMo-Base/mamo-Connector/src/forge.rs)

Revert the arguments to pass `replay <path>` directly instead of prepending `"gui"` (revert all occurrences of `gui replay` back to `replay`):
```rust
cmd.arg("replay").arg(replay_path);
```
