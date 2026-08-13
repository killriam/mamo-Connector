use anyhow::Result;
use log::{error, info};
use std::path::PathBuf;
use std::process::Command;

use crate::settings::Settings;

/// Result of a Forge launch operation
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ForgeLaunchResult {
    pub success: bool,
    /// True when Forge was already running and was not launched again
    pub already_running: bool,
    pub message: String,
    pub deck_path: Option<String>,
    pub forge_path: Option<String>,
    pub pid: Option<u32>,
}

impl ForgeLaunchResult {
    pub fn success(message: impl Into<String>, deck_path: Option<String>, forge_path: Option<String>, pid: Option<u32>) -> Self {
        Self {
            success: true,
            already_running: false,
            message: message.into(),
            deck_path,
            forge_path,
            pid,
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            already_running: false,
            message: message.into(),
            deck_path: None,
            forge_path: None,
            pid: None,
        }
    }

    pub fn hint_already_running(deck_name: Option<String>) -> Self {
        let message = match deck_name {
            Some(name) => format!(
                "Forge is already open. The deck '{}' was saved to your Forge decks folder — open it manually.",
                name
            ),
            None => "Forge is already open.".to_string(),
        };
        Self {
            success: true,
            already_running: true,
            message,
            deck_path: None,
            forge_path: None,
            pid: None,
        }
    }
}

/// Adoptium Temurin JRE 17 download page (Windows x64).
pub const JAVA_DOWNLOAD_URL: &str =
    "https://adoptium.net/temurin/releases/?version=17&os=windows&arch=x64&package=jre";

/// Result of probing the system for a usable Java runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JavaStatus {
    /// A Java runtime of major version >= 17 is available (via JAVA_HOME or PATH).
    Ok(u32),
    /// Java is present but its major version is too old (< 17).
    TooOld(u32),
    /// No `java` executable found via JAVA_HOME or PATH.
    Missing,
}

/// Run `java -version` and parse the major version from its output.
///
/// `java -version` prints to **stderr** in formats like:
///   - `openjdk version "17.0.10" 2024-01-16`  -> 17
///   - `java version "1.8.0_381"`              -> 8  (legacy 1.x scheme)
fn parse_java_major(version_output: &str) -> Option<u32> {
    // Find the first quoted version string
    let start = version_output.find('"')?;
    let rest = &version_output[start + 1..];
    let end = rest.find('"')?;
    let version = &rest[..end];

    let mut parts = version.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        // Legacy "1.8.0_381" scheme: the major version is the second component.
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Run `<java_exe> -version` and parse the major version, if it runs at all.
fn probe_java_version(java_exe: impl AsRef<std::ffi::OsStr>) -> Option<u32> {
    let out = Command::new(java_exe).arg("-version").output().ok()?;
    // `java -version` writes to stderr; some distributions use stdout.
    let text = if !out.stderr.is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    parse_java_major(&text)
}

/// True when JAVA_HOME's probed major version is a valid 17+ runtime worth preferring over PATH.
/// Separated as pure logic so it's testable without a real Java install/JAVA_HOME on the test
/// machine.
fn java_home_is_preferred(java_home_major: Option<u32>) -> bool {
    matches!(java_home_major, Some(major) if major >= 17)
}

/// Resolve which `java` command to actually invoke: JAVA_HOME's `java`/`java.exe` if it probes
/// as a valid 17+ runtime, otherwise bare `"java"` (PATH-resolved).
///
/// This is the *single* source of truth for "which Java runs Forge" — used by both the
/// pre-flight version check (`detect_java`) and the real `java -jar ...` launch command in
/// `launch_forge`/`launch_forge_replay`, so the two can never disagree. A bare PATH lookup
/// alone resolves to whatever happens to be *first* on PATH, which on real machines is often an
/// unrelated, older JRE some other application installed (e.g. a stray Java 8 from a legacy
/// tool) shadowing a perfectly good Java 17+ install later in PATH or pointed to by JAVA_HOME.
/// Preferring JAVA_HOME's fully-qualified path sidesteps PATH ordering entirely — no PATH or
/// registry changes (and no admin rights) required.
fn resolve_java_command() -> std::ffi::OsString {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let exe = std::path::Path::new(&home)
            .join("bin")
            .join(if cfg!(windows) { "java.exe" } else { "java" });
        if java_home_is_preferred(probe_java_version(&exe)) {
            return exe.into_os_string();
        }
    }
    std::ffi::OsString::from("java")
}

/// Detect whether the Java command that will actually be used to launch Forge (see
/// `resolve_java_command`) is a usable 17+ runtime.
pub fn detect_java() -> JavaStatus {
    match probe_java_version(resolve_java_command()) {
        Some(major) if major >= 17 => JavaStatus::Ok(major),
        Some(major) => JavaStatus::TooOld(major),
        None => JavaStatus::Missing,
    }
}

/// Get the default Forge installation path based on OS
pub fn get_default_forge_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // Common Windows installation paths
        let paths = [
            // User's AppData
            dirs::data_local_dir().map(|p| p.join("Forge")),
            // Program Files
            Some(PathBuf::from("C:\\Program Files\\Forge")),
            Some(PathBuf::from("C:\\Program Files (x86)\\Forge")),
            // Common user installation locations
            dirs::home_dir().map(|p| p.join("Forge")),
            dirs::desktop_dir().map(|p| p.join("Forge")),
            dirs::document_dir().map(|p| p.join("Forge")),
        ];

        for path_opt in paths {
            if let Some(path) = path_opt {
                // Check for forge.exe or forge-gui-desktop.jar
                let exe_path = path.join("forge.exe");
                if exe_path.exists() {
                    return Some(exe_path);
                }
                let jar_path = path.join("forge-gui-desktop.jar");
                if jar_path.exists() {
                    return Some(jar_path);
                }
                // Check in bin subfolder
                let bin_exe = path.join("bin").join("forge.exe");
                if bin_exe.exists() {
                    return Some(bin_exe);
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let paths = [
            Some(PathBuf::from("/Applications/Forge.app")),
            dirs::home_dir().map(|p| p.join("Applications").join("Forge.app")),
        ];

        for path_opt in paths {
            if let Some(path) = path_opt {
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let paths = [
            dirs::home_dir().map(|p| p.join("Forge")),
            dirs::home_dir().map(|p| p.join(".local").join("share").join("Forge")),
            Some(PathBuf::from("/opt/forge")),
            Some(PathBuf::from("/usr/local/forge")),
        ];

        for path_opt in paths {
            if let Some(path) = path_opt {
                let jar_path = path.join("forge-gui-desktop.jar");
                if jar_path.exists() {
                    return Some(jar_path);
                }
                let sh_path = path.join("forge.sh");
                if sh_path.exists() {
                    return Some(sh_path);
                }
            }
        }
    }

    None
}

/// Scan a directory for the latest `forge-gui-desktop-*-jar-with-dependencies.jar`.
/// Returns the JAR with the most-recent modification time, or `None` if none found.
pub fn resolve_latest_forge_jar(dir: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            if name.starts_with("forge-gui-desktop-")
                && name.ends_with("-jar-with-dependencies.jar")
            {
                let modified = e.metadata().ok()?.modified().ok()?;
                Some((path, modified))
            } else {
                None
            }
        })
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
    candidates.into_iter().next().map(|(p, _)| p)
}

/// Validate that a Forge path is valid
pub fn validate_forge_path(path: &str) -> bool {
    let path = PathBuf::from(path);
    
    if !path.exists() {
        return false;
    }

    // If it's a directory, check whether it contains a forge-gui-desktop JAR
    if path.is_dir() {
        return resolve_latest_forge_jar(&path).is_some();
    }

    // Check if it's a valid executable
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    
    #[cfg(windows)]
    {
        // On Windows, accept .exe, .jar, or .bat files
        matches!(extension.to_lowercase().as_str(), "exe" | "jar" | "bat")
    }
    
    #[cfg(target_os = "macos")]
    {
        // On macOS, accept .app bundles or .jar files
        path.is_dir() && path.extension().map(|e| e == "app").unwrap_or(false)
            || extension.to_lowercase() == "jar"
    }
    
    #[cfg(target_os = "linux")]
    {
        // On Linux, accept .jar or .sh files, or executables
        extension.to_lowercase() == "jar" 
            || extension.to_lowercase() == "sh"
            || is_executable(&path)
    }
    
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        path.is_file()
    }
}

#[cfg(target_os = "linux")]
fn is_executable(path: &PathBuf) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

use log::debug;

/// The CLI arguments that launch Forge straight into a commander game with the given deck(s)
/// pre-selected: `gui --format commander [--deck <name>] [--deck2 <name2>]`.
///
/// Shared by every launch branch in `launch_forge` below (JAR, Windows exe/cmd/bat, Linux sh,
/// generic fallback) so they can't drift out of sync with each other — which is exactly how the
/// Windows exe/cmd/bat branch previously ended up launching Forge with none of these args at all,
/// even though Forge's own launcher does forward them through to `forge.view.Main` correctly.
fn force_constructed_view_in_preferences() {
    let Some(mut path) = get_forge_deck_directory() else { return; };
    if path.pop() && path.pop() {
        let pref_file = path.join("preferences").join("forge.preferences");
        if pref_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&pref_file) {
                let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
                let keys_to_update = [
                    ("SUBMENU_CURRENTMENU", "HOME_CONSTRUCTED"),
                    ("SUBMENU_SANCTIONED", "true"),
                    ("SUBMENU_PUZZLE", "false"),
                    ("SUBMENU_QUEST", "false"),
                ];
                let mut modified = false;
                for (key, val) in keys_to_update {
                    let prefix = format!("{}=", key);
                    let mut found = false;
                    for line in lines.iter_mut() {
                        if line.starts_with(&prefix) {
                            let expected = format!("{}={}", key, val);
                            if *line != expected {
                                *line = expected;
                                modified = true;
                            }
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        lines.push(format!("{}={}", key, val));
                        modified = true;
                    }
                }
                if modified {
                    let _ = std::fs::write(&pref_file, lines.join("\n") + "\n");
                }
            }
        }
    }
}

fn forge_launch_args(deck_name: Option<&str>, deck2_name: Option<&str>) -> Vec<String> {
    let mut args = vec!["gui".to_string(), "--format".to_string(), "commander".to_string()];
    if let Some(deck) = deck_name {
        args.push("--deck".to_string());
        args.push(deck.to_string());
    }
    if let Some(deck2) = deck2_name {
        args.push("--deck2".to_string());
        args.push(deck2.to_string());
    }
    args
}

/// Launch Forge with an optional deck name and optional second deck name.
///
/// For JAR builds the command becomes:
///   java ... -jar <forge.jar> gui --format commander [--deck <name>] [--deck2 <name2>]
pub fn launch_forge(forge_path: &str, deck_name: Option<&str>, deck2_name: Option<&str>) -> Result<ForgeLaunchResult> {
    let forge_path_buf = PathBuf::from(forge_path);
    debug!("[launch_forge] Input forge_path: {}", forge_path);
    debug!("[launch_forge] Input deck_name: {:?}", deck_name);
    debug!("[launch_forge] Input deck2_name: {:?}", deck2_name);

    if !forge_path_buf.exists() {
        error!("[launch_forge] Forge path does not exist: {}", forge_path);
        return Ok(ForgeLaunchResult::failure(format!(
            "Forge executable not found at: {}", forge_path
        )));
    }

    force_constructed_view_in_preferences();

    // If a directory was configured, resolve to the latest forge-gui-desktop JAR inside it
    let forge_path_buf = if forge_path_buf.is_dir() {
        debug!("[launch_forge] Forge path is a directory, attempting to resolve latest JAR");
        match resolve_latest_forge_jar(&forge_path_buf) {
            Some(jar) => {
                info!("Resolved Forge directory to latest JAR: {}", jar.display());
                jar
            }
            None => {
                error!("[launch_forge] No forge-gui-desktop JAR found in directory: {}", forge_path);
                return Ok(ForgeLaunchResult::failure(format!(
                    "No forge-gui-desktop JAR found in directory: {}", forge_path
                )));
            }
        }
    } else {
        forge_path_buf
    };

    let resolved_path_str = forge_path_buf.to_string_lossy().to_string();
    info!("Launching Forge from: {}", resolved_path_str);
    if let Some(deck) = deck_name {
        info!("With deck: {}", deck);
    }
    if let Some(deck2) = deck2_name {
        info!("With deck2: {}", deck2);
    }

    // Get the directory containing Forge - important for finding dependencies
    let forge_dir = forge_path_buf.parent().map(|p| p.to_path_buf());
    debug!("[launch_forge] Forge directory: {:?}", forge_dir);

    let extension = forge_path_buf.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    debug!("[launch_forge] Forge file extension: {}", extension);

    // Forge is a Java app. Launching a .jar with a missing or too-old Java
    // would "succeed" at spawn() but die immediately with Forge's own
    // "requires JRE 17" dialog — a false positive. Guard against that here so
    // Test Launch and deeplink launches report the real, actionable error.
    if extension == "jar" {
        match detect_java() {
            JavaStatus::Ok(major) => {
                debug!("[launch_forge] Detected Java {} (OK)", major);
            }
            JavaStatus::TooOld(major) => {
                return Ok(ForgeLaunchResult::failure(format!(
                    "Forge needs Java 17 or newer, but Java {} was found. \
                     Install Java 17 (Adoptium Temurin) and try again.",
                    major
                )));
            }
            JavaStatus::Missing => {
                return Ok(ForgeLaunchResult::failure(
                    "Forge needs Java 17 to run, but no Java was found. \
                     Install Java 17 (Adoptium Temurin) and try again.",
                ));
            }
        }
    }

    let result = match extension.as_str() {
        "jar" => {
            debug!("[launch_forge] Launching as JAR with java");
            let mut cmd = Command::new(resolve_java_command());
            cmd.arg("-Xmx4096m")
               .arg("-Dio.netty.tryReflectionSetAccessible=true")
               .arg("-Dfile.encoding=UTF-8")
               .arg("-jar")
               .arg(&forge_path_buf);
            debug!("[launch_forge] Java command: java -Xmx4096m ... -jar {} gui --format commander", resolved_path_str);
            if let Some(dir) = &forge_dir {
                debug!("[launch_forge] Setting working directory: {}", dir.display());
                cmd.current_dir(dir);
            }
            cmd.args(forge_launch_args(deck_name, deck2_name));
            cmd.spawn()
        }
        "exe" | "cmd" | "bat" => {
            debug!("[launch_forge] Launching as Windows executable");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x00000008;
                let mut cmd = Command::new(&forge_path_buf);
                debug!("[launch_forge] Executable command: {:?}", &forge_path_buf);
                if let Some(dir) = &forge_dir {
                    debug!("[launch_forge] Setting working directory: {}", dir.display());
                    cmd.current_dir(dir);
                }
                // Forge's native launcher forwards its CLI args straight through to
                // forge.view.Main — confirmed by inspecting the spawned java process's command
                // line — so it needs the same args as every other launch path. This branch
                // previously passed none at all, which is why a specific deck never got
                // pre-selected when Forge was installed as an .exe.
                cmd.args(forge_launch_args(deck_name, deck2_name));
                cmd.creation_flags(DETACHED_PROCESS);
                debug!("[launch_forge] Using DETACHED_PROCESS flag");
                cmd.spawn()
            }
            #[cfg(not(windows))]
            {
                let mut cmd = Command::new(&forge_path_buf);
                debug!("[launch_forge] Executable command (non-windows): {:?}", &forge_path_buf);
                if let Some(dir) = &forge_dir {
                    debug!("[launch_forge] Setting working directory: {}", dir.display());
                    cmd.current_dir(dir);
                }
                if let Some(deck) = deck_name {
                    debug!("[launch_forge] Adding --deck argument: {}", deck);
                    cmd.arg("--deck").arg(deck);
                }
                if let Some(deck2) = deck2_name {
                    debug!("[launch_forge] Adding --deck2 argument: {}", deck2);
                    cmd.arg("--deck2").arg(deck2);
                }
                cmd.spawn()
            }
        }
        "app" => {
            debug!("[launch_forge] Launching as macOS app bundle");
            let mut cmd = Command::new("open");
            cmd.arg(&forge_path_buf);
            if deck_name.is_some() || deck2_name.is_some() {
                cmd.arg("--args");
                if let Some(deck) = deck_name {
                    debug!("[launch_forge] Adding --deck argument for macOS: {}", deck);
                    cmd.arg("--deck").arg(deck);
                }
                if let Some(deck2) = deck2_name {
                    debug!("[launch_forge] Adding --deck2 argument for macOS: {}", deck2);
                    cmd.arg("--deck2").arg(deck2);
                }
            }
            cmd.spawn()
        }
        "sh" => {
            debug!("[launch_forge] Launching as shell script");
            let mut cmd = Command::new(&forge_path_buf);
            if let Some(dir) = &forge_dir {
                debug!("[launch_forge] Setting working directory: {}", dir.display());
                cmd.current_dir(dir);
            }
            if let Some(deck) = deck_name {
                debug!("[launch_forge] Adding --deck argument: {}", deck);
                cmd.arg("--deck").arg(deck);
            }
            if let Some(deck2) = deck2_name {
                debug!("[launch_forge] Adding --deck2 argument: {}", deck2);
                cmd.arg("--deck2").arg(deck2);
            }
            cmd.spawn()
        }
        _ => {
            debug!("[launch_forge] Launching as generic executable");
            let mut cmd = Command::new(&forge_path_buf);
            if let Some(dir) = &forge_dir {
                debug!("[launch_forge] Setting working directory: {}", dir.display());
                cmd.current_dir(dir);
            }
            if let Some(deck) = deck_name {
                debug!("[launch_forge] Adding --deck argument: {}", deck);
                cmd.arg("--deck").arg(deck);
            }
            if let Some(deck2) = deck2_name {
                debug!("[launch_forge] Adding --deck2 argument: {}", deck2);
                cmd.arg("--deck2").arg(deck2);
            }
            cmd.spawn()
        }
    };

    match result {
        Ok(child) => {
            let pid = child.id();
            info!("Forge launched successfully with PID: {:?}", pid);
            debug!("[launch_forge] Child process PID: {:?}", pid);
            Ok(ForgeLaunchResult::success(
                format!("Forge launched successfully"),
                deck_name.map(|s| s.to_string()),
                Some(resolved_path_str.clone()),
                Some(pid),
            ))
        }
        Err(e) => {
            error!("Failed to launch Forge: {}", e);
            debug!("[launch_forge] Command spawn error: {}", e);
            Ok(ForgeLaunchResult::failure(format!(
                "Failed to launch Forge: {}", e
            )))
        }
    }
}

/// Launch Forge in replay mode with a specific replay JSON file
///
/// Uses the `replay <path>` CLI mode added to Forge's Main.java (see FORGE_REPLAY_BUG.md).
/// Passes `replay <path>` directly to Forge's CLI on every launch path (JAR, Windows EXE,
/// macOS .app, shell script) — Forge's native launchers all forward CLI args straight through
/// to forge.view.Main, same as the deck-preselect args in launch_forge() above.
pub fn launch_forge_replay(replay_path: &str) -> Result<ForgeLaunchResult> {
    // Don't open another Forge window if one is already visible
    if is_forge_window_open() {
        info!("Forge is already running — replay file was saved to gamelogs directory");
        return Ok(ForgeLaunchResult::hint_already_running(Some(
            "Replay file saved. Open Replay Mode in Forge to start.".to_string(),
        )));
    }

    let settings = Settings::load()?;

    let forge_path = match &settings.forge_path {
        Some(path) if !path.is_empty() => path.clone(),
        _ => match get_default_forge_path() {
            Some(path) => path.to_string_lossy().to_string(),
            None => {
                return Ok(ForgeLaunchResult::failure(
                    "Forge path not configured. Please set it in the Settings tab.",
                ));
            }
        },
    };

    let forge_path_buf = PathBuf::from(&forge_path);

    if !forge_path_buf.exists() {
        return Ok(ForgeLaunchResult::failure(format!(
            "Forge executable not found at: {}", forge_path
        )));
    }

    // Resolve directory to latest JAR if needed
    let forge_path_buf = if forge_path_buf.is_dir() {
        match resolve_latest_forge_jar(&forge_path_buf) {
            Some(jar) => {
                info!("Resolved Forge directory to latest JAR: {}", jar.display());
                jar
            }
            None => {
                return Ok(ForgeLaunchResult::failure(format!(
                    "No forge-gui-desktop JAR found in directory: {}", forge_path
                )));
            }
        }
    } else {
        forge_path_buf
    };

    let resolved_path_str = forge_path_buf.to_string_lossy().to_string();
    info!("Launching Forge in replay mode from: {}", resolved_path_str);
    info!("Replay file: {}", replay_path);

    let forge_dir = forge_path_buf.parent().map(|p| p.to_path_buf());
    let extension = forge_path_buf
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let result = match extension.as_str() {
        "jar" => {
            let mut cmd = Command::new(resolve_java_command());
            cmd.arg("-Xmx4096m")
                .arg("-Dio.netty.tryReflectionSetAccessible=true")
                .arg("-Dfile.encoding=UTF-8")
                .arg("-jar")
                .arg(&forge_path_buf);

            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }

            // Forge replay CLI: `replay <path>`
            cmd.arg("replay").arg(replay_path);

            cmd.spawn()
        }
        "exe" | "cmd" | "bat" => {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x00000008;

                let mut cmd = Command::new(&forge_path_buf);
                if let Some(dir) = &forge_dir {
                    cmd.current_dir(dir);
                }
                // Forge's native launcher forwards CLI args straight through to forge.view.Main
                // (confirmed empirically — see launch_forge's Windows branch), so `replay <path>`
                // works here exactly like it does on the JAR/mac/Linux paths below.
                cmd.arg("replay").arg(replay_path);
                cmd.creation_flags(DETACHED_PROCESS);
                cmd.spawn()
            }
            #[cfg(not(windows))]
            {
                let mut cmd = Command::new(&forge_path_buf);
                if let Some(dir) = &forge_dir {
                    cmd.current_dir(dir);
                }
                cmd.arg("replay").arg(replay_path);
                cmd.spawn()
            }
        }
        "app" => {
            let mut cmd = Command::new("open");
            cmd.arg(&forge_path_buf);
            cmd.arg("--args").arg("replay").arg(replay_path);
            cmd.spawn()
        }
        "sh" => {
            let mut cmd = Command::new(&forge_path_buf);
            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }
            cmd.arg("replay").arg(replay_path);
            cmd.spawn()
        }
        _ => {
            let mut cmd = Command::new(&forge_path_buf);
            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }
            cmd.arg("replay").arg(replay_path);
            cmd.spawn()
        }
    };

    match result {
        Ok(child) => {
            let pid = child.id();
            info!("Forge launched in replay mode with PID: {:?}", pid);
            Ok(ForgeLaunchResult::success(
                "Forge launched in replay mode.",
                Some(replay_path.to_string()),
                Some(resolved_path_str),
                Some(pid),
            ))
        }
        Err(e) => {
            error!("Failed to launch Forge for replay: {}", e);
            Ok(ForgeLaunchResult::failure(format!(
                "Failed to launch Forge: {}", e
            )))
        }
    }
}

/// Launch Forge using the path from settings.
///
/// `deck_path` and `deck2_path` may be full file-system paths (e.g. `.../decks/MyDeck.dck`)
/// or plain deck names. The file stem is extracted and passed to Forge as `--deck` / `--deck2`.
pub fn launch_forge_from_settings(deck_path: Option<&str>, deck2_path: Option<&str>) -> Result<ForgeLaunchResult> {
    // Log if Forge is already running, but do not skip launch
    if is_forge_window_open() {
        info!("Forge is already running (window detected), but will attempt to launch again.");
    }

    let settings = Settings::load()?;

    let forge_path = match &settings.forge_path {
        Some(path) if !path.is_empty() => path.clone(),
        _ => {
            match get_default_forge_path() {
                Some(path) => path.to_string_lossy().to_string(),
                None => {
                    return Ok(ForgeLaunchResult::failure(
                        "Forge path not configured. Please set it in the Settings tab."
                    ));
                }
            }
        }
    };

    // Extract deck names (file stems) from paths so Forge receives the display name.
    let deck_name = deck_path.map(|p| {
        PathBuf::from(p)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string())
    });
    let deck2_name = deck2_path.map(|p| {
        PathBuf::from(p)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string())
    });

    launch_forge(&forge_path, deck_name.as_deref(), deck2_name.as_deref())
}

/// Get the Forge deck directory (where decks should be saved).
///
/// Forge keeps its user profile (decks, preferences, saves) in the OS user-data
/// directory, not next to the installed executable — so this must match
/// `deck::get_deck_directory()` rather than deriving from the configured `forge_path`.
pub fn get_forge_deck_directory() -> Option<PathBuf> {
    crate::deck::get_deck_directory().ok()
}

/// List deck names (file stems of `.dck` files) available in the Forge deck directory.
pub fn list_forge_decks() -> Vec<String> {
    let Some(dir) = get_forge_deck_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut decks: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "dck").unwrap_or(false))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    decks.sort_unstable();
    decks
}

/// Check if a process with the given PID is still running
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use winapi::um::processthreadsapi::OpenProcess;
        use winapi::um::handleapi::CloseHandle;
        
        // PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                false
            } else {
                CloseHandle(handle);
                true
            }
        }
    }
    
    #[cfg(not(windows))]
    {
        // On Unix, check /proc/PID or use kill -0
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
}

/// Check if a Forge window is currently open by scanning all visible window titles.
/// This is needed because forge.exe is a launcher that spawns java.exe and exits immediately.
/// The actual Forge game runs as a Java process with a window titled "Forge ...".
pub fn is_forge_window_open() -> bool {
    #[cfg(windows)]
    {
        use winapi::um::winuser::{EnumWindows, GetWindowTextW, IsWindowVisible, GetWindowThreadProcessId};
        use winapi::shared::windef::HWND;
        use winapi::shared::minwindef::{BOOL, LPARAM, TRUE};
        
        unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                if IsWindowVisible(hwnd) == 0 {
                    return TRUE; // continue
                }
                let mut buf = [0u16; 256];
                let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                if len > 0 {
                    let title = String::from_utf16_lossy(&buf[..len as usize]);
                    if title.contains("Forge") {
                        // Get process ID of the window to verify if it's the actual Forge game
                        // (either java.exe/javaw.exe for jar launch, or forge.exe for native launcher).
                        // This prevents false positives from browser tabs, VS Code, or explorer windows
                        // that happen to contain "Forge" in their title.
                        let mut pid = 0;
                        GetWindowThreadProcessId(hwnd, &mut pid);
                        if pid != 0 {
                            use winapi::um::processthreadsapi::OpenProcess;
                            use winapi::um::handleapi::CloseHandle;
                            
                            unsafe extern "system" {
                                fn QueryFullProcessImageNameW(
                                    hProcess: *mut winapi::ctypes::c_void,
                                    dwFlags: u32,
                                    lpExeName: *mut u16,
                                    lpdwSize: *mut u32,
                                ) -> i32;
                            }
                            
                            const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
                            
                            let h_process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                            if !h_process.is_null() {
                                let mut img_buf = [0u16; 1024];
                                let mut size = img_buf.len() as u32;
                                if QueryFullProcessImageNameW(h_process, 0, img_buf.as_mut_ptr(), &mut size) != 0 {
                                    let img_path = String::from_utf16_lossy(&img_buf[..size as usize]);
                                    let exe_name = img_path
                                        .split('\\')
                                        .last()
                                        .unwrap_or("")
                                        .to_lowercase();
                                    
                                    if exe_name == "forge.exe"
                                        || exe_name == "java.exe"
                                        || exe_name == "javaw.exe"
                                        || exe_name == "forge"
                                        || exe_name == "java"
                                        || exe_name == "javaw"
                                    {
                                        let found = &mut *(lparam as *mut bool);
                                        *found = true;
                                        CloseHandle(h_process);
                                        return 0; // stop enumeration
                                    }
                                }
                                CloseHandle(h_process);
                            }
                        }
                    }
                }
                TRUE // continue
            }
        }
        
        let mut found = false;
        unsafe {
            EnumWindows(Some(enum_callback), &mut found as *mut bool as LPARAM);
        }
        found
    }
    
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_forge_path_nonexistent() {
        assert!(!validate_forge_path("/nonexistent/path/forge.exe"));
    }

    #[test]
    fn test_get_default_forge_path() {
        // This just tests that the function doesn't panic
        let _ = get_default_forge_path();
    }

    #[test]
    fn forge_launch_args_no_decks() {
        assert_eq!(
            forge_launch_args(None, None),
            vec!["gui", "--format", "commander"]
        );
    }

    #[test]
    fn forge_launch_args_one_deck() {
        assert_eq!(
            forge_launch_args(Some("Aggro"), None),
            vec!["gui", "--format", "commander", "--deck", "Aggro"]
        );
    }

    #[test]
    fn forge_launch_args_two_decks() {
        assert_eq!(
            forge_launch_args(Some("Aggro"), Some("Control")),
            vec!["gui", "--format", "commander", "--deck", "Aggro", "--deck2", "Control"]
        );
    }

    #[test]
    fn java_home_preferred_when_valid_17_plus() {
        // The exact scenario this fix exists for: JAVA_HOME points at a valid 17+ install, even
        // though whatever's first on PATH might be an unrelated, older JRE (e.g. a stray Java 8).
        assert!(java_home_is_preferred(Some(17)));
        assert!(java_home_is_preferred(Some(21)));
    }

    #[test]
    fn java_home_not_preferred_when_too_old_or_unset() {
        assert!(!java_home_is_preferred(Some(8)));
        assert!(!java_home_is_preferred(None));
    }
}
