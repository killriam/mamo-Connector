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
    pub message: String,
    pub deck_path: Option<String>,
    pub forge_path: Option<String>,
    pub pid: Option<u32>,
}

impl ForgeLaunchResult {
    pub fn success(message: impl Into<String>, deck_path: Option<String>, forge_path: Option<String>, pid: Option<u32>) -> Self {
        Self {
            success: true,
            message: message.into(),
            deck_path,
            forge_path,
            pid,
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            deck_path: None,
            forge_path: None,
            pid: None,
        }
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

/// Launch Forge with an optional deck file
pub fn launch_forge(forge_path: &str, deck_path: Option<&str>) -> Result<ForgeLaunchResult> {
    let forge_path_buf = PathBuf::from(forge_path);
    
    if !forge_path_buf.exists() {
        return Ok(ForgeLaunchResult::failure(format!(
            "Forge executable not found at: {}", forge_path
        )));
    }

    // If a directory was configured, resolve to the latest forge-gui-desktop JAR inside it
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
    info!("Launching Forge from: {}", resolved_path_str);
    if let Some(deck) = deck_path {
        info!("With deck: {}", deck);
    }

    // Get the directory containing Forge - important for finding dependencies
    let forge_dir = forge_path_buf.parent().map(|p| p.to_path_buf());

    let extension = forge_path_buf.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let result = match extension.as_str() {
        "jar" => {
            // Launch JAR file with java - need to set working directory
            let mut cmd = Command::new("java");
            cmd.arg("-Xmx4096m")
               .arg("-Dio.netty.tryReflectionSetAccessible=true")
               .arg("-Dfile.encoding=UTF-8")
               .arg("-jar")
               .arg(&forge_path_buf);
            
            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }
            
            if let Some(deck) = deck_path {
                cmd.arg("--deck").arg(deck);
            }
            
            cmd.spawn()
        }
        "exe" | "cmd" | "bat" => {
            // On Windows, directly launch the executable from its directory
            // The forge.exe launcher needs to run from its directory to find the JAR
            // Note: Forge doesn't support command-line deck loading, 
            // but the deck is saved to the Forge decks directory for manual opening
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x00000008;
                
                let mut cmd = Command::new(&forge_path_buf);
                
                // Critical: Set working directory to forge's directory
                if let Some(dir) = &forge_dir {
                    cmd.current_dir(dir);
                }
                
                // Use DETACHED_PROCESS so forge runs independently
                cmd.creation_flags(DETACHED_PROCESS);
                
                // Note: Forge doesn't support --deck command line argument
                // Deck is available in Forge's deck folder after download
                
                cmd.spawn()
            }
            #[cfg(not(windows))]
            {
                let mut cmd = Command::new(&forge_path_buf);
                
                if let Some(dir) = &forge_dir {
                    cmd.current_dir(dir);
                }
                
                if let Some(deck) = deck_path {
                    cmd.arg("--deck").arg(deck);
                }
                
                cmd.spawn()
            }
        }
        "app" => {
            // Launch macOS app bundle
            let mut cmd = Command::new("open");
            cmd.arg(&forge_path_buf);
            
            if let Some(deck) = deck_path {
                cmd.arg("--args").arg("--deck").arg(deck);
            }
            
            cmd.spawn()
        }
        "sh" => {
            // Launch shell script with working directory
            let mut cmd = Command::new(&forge_path_buf);
            
            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }
            
            if let Some(deck) = deck_path {
                cmd.arg("--deck").arg(deck);
            }
            
            cmd.spawn()
        }
        _ => {
            // Try to launch as executable with working directory
            let mut cmd = Command::new(&forge_path_buf);
            
            if let Some(dir) = &forge_dir {
                cmd.current_dir(dir);
            }
            
            if let Some(deck) = deck_path {
                cmd.arg("--deck").arg(deck);
            }
            
            cmd.spawn()
        }
    };

    match result {
        Ok(child) => {
            let pid = child.id();
            info!("Forge launched successfully with PID: {:?}", pid);
            Ok(ForgeLaunchResult::success(
                format!("Forge launched successfully"),
                deck_path.map(|s| s.to_string()),
                Some(resolved_path_str.clone()),
                Some(pid),
            ))
        }
        Err(e) => {
            error!("Failed to launch Forge: {}", e);
            Ok(ForgeLaunchResult::failure(format!(
                "Failed to launch Forge: {}", e
            )))
        }
    }
}

/// Launch Forge using the path from settings
pub fn launch_forge_from_settings(deck_path: Option<&str>) -> Result<ForgeLaunchResult> {
    let settings = Settings::load()?;
    
    let forge_path = match &settings.forge_path {
        Some(path) if !path.is_empty() => path.clone(),
        _ => {
            // Try to find Forge automatically
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

    launch_forge(&forge_path, deck_path)
}

/// Get the Forge deck directory (where decks should be saved)
#[allow(dead_code)]
pub fn get_forge_deck_directory() -> Option<PathBuf> {
    let settings = Settings::load().ok()?;
    
    if let Some(forge_path) = &settings.forge_path {
        let forge_dir = PathBuf::from(forge_path).parent()?.to_path_buf();
        
        // Forge stores decks in a 'decks' subfolder
        let decks_dir = forge_dir.join("decks");
        if decks_dir.exists() {
            return Some(decks_dir);
        }
        
        // Or in 'decks/constructed'
        let constructed_dir = forge_dir.join("decks").join("constructed");
        if constructed_dir.exists() {
            return Some(constructed_dir);
        }
    }
    
    None
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
        use winapi::um::winuser::{EnumWindows, GetWindowTextW, IsWindowVisible};
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
                        let found = &mut *(lparam as *mut bool);
                        *found = true;
                        return 0; // stop enumeration
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
}
