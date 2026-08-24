use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Directory where the relocated, stable copy of the Connector executable lives.
///
/// Deliberately under the *Local* app-data root, not the *Roaming* one `settings.rs`/`main.rs`
/// use for settings/lock/pending-command — matching the existing precedent in
/// `forge::get_default_forge_path()`, which already treats `data_local_dir()` as where installed
/// binaries/caches belong.
#[allow(dead_code)]
pub fn stable_app_dir() -> Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("Could not find local app-data directory")?
        .join("MamoConnector")
        .join("app"))
}

/// Pure comparison: is `current` already the stable, canonicalized `target` path? Kept separate
/// from any filesystem/process work so it's directly testable.
#[allow(dead_code)]
fn already_at_target(current: &Path, target: &Path) -> bool {
    current == target
}

/// If the running executable isn't already at its stable home, copy it there and spawn the
/// stable copy as a replacement process (forwarding `args`, so a same-launch deeplink is still
/// handled by the relocated copy). Returns `true` when it relocated and spawned a replacement —
/// the caller must exit immediately without taking the single-instance lock or registering the
/// URL scheme, since the new process does all of that itself, from the stable path.
///
/// `registration::ensure_registered` already re-registers against `current_exe()` on every
/// launch, so once the relocated process starts, its own next call to that function naturally
/// points the registry at the stable path — no separate registration step is needed here.
///
/// Never fails startup: any I/O error is logged and treated as "already home", falling back to
/// running in place exactly as before this existed.
#[cfg(all(windows, not(debug_assertions)))]
pub fn relocate_to_stable_location(args: &[String]) -> bool {
    let current = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Could not determine current executable path: {e}");
            return false;
        }
    };

    let target_dir = match stable_app_dir() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("Could not determine stable app directory: {e}");
            return false;
        }
    };
    let target = target_dir.join("mamo-connector.exe");

    // Only meaningful to compare once the target actually exists on disk.
    if target.exists() {
        if let Ok(canon_target) = target.canonicalize() {
            if already_at_target(&current, &canon_target) {
                return false;
            }
        }
    }

    if let Err(e) = std::fs::create_dir_all(&target_dir) {
        log::warn!("Could not create stable app directory {target_dir:?}: {e}");
        return false;
    }
    if let Err(e) = std::fs::copy(&current, &target) {
        log::warn!("Could not copy executable to stable location {target:?}: {e}");
        return false;
    }

    log::info!("Relocated to stable location: {target:?}");

    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;

    match std::process::Command::new(&target)
        .args(args)
        .creation_flags(DETACHED_PROCESS)
        .spawn()
    {
        Ok(_) => true,
        Err(e) => {
            log::warn!("Could not launch relocated executable: {e}");
            false
        }
    }
}

/// Debug builds and non-Windows targets never relocate — see the Windows-release implementation
/// above for why.
#[cfg(any(not(windows), debug_assertions))]
pub fn relocate_to_stable_location(_args: &[String]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_at_target_true_when_equal() {
        let p = PathBuf::from(r"C:\Users\test\AppData\Local\MamoConnector\app\mamo-connector.exe");
        assert!(already_at_target(&p, &p));
    }

    #[test]
    fn already_at_target_false_when_different() {
        let current = PathBuf::from(r"C:\Users\test\Downloads\mamo-connector.exe");
        let target = PathBuf::from(r"C:\Users\test\AppData\Local\MamoConnector\app\mamo-connector.exe");
        assert!(!already_at_target(&current, &target));
    }

    #[test]
    fn stable_app_dir_ends_with_expected_segments() {
        let dir = stable_app_dir().expect("data_local_dir should resolve in test environment");
        assert!(dir.ends_with(PathBuf::from("MamoConnector").join("app")));
    }
}
