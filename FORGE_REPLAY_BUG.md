# Forge Replay Launching Bug & Fix Details

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
