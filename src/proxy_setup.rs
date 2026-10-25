//! Local setup of the proxy-printing capability: installs the bundled print tool into
//! `<local app data>/MamoConnector/proxy-printing`, creates a private Python virtualenv there and
//! installs the tool's pip requirements. Used by the Setup tab ("Set up proxy printing") and,
//! without the Python install step, automatically by the `print-proxies` deeplink.
//!
//! The scripts are embedded at build time (see `build.rs`); nothing is downloaded except Python
//! packages from PyPI and, only when the user presses the Setup button, Python itself via winget.

use anyhow::{anyhow, bail, Context, Result};
use log::info;
use std::path::{Path, PathBuf};
use std::process::Command;

mod bundle {
    include!(concat!(env!("OUT_DIR"), "/proxy_printing_bundle.rs"));
}

/// The bundled scripts use `str | None` annotations, which need Python 3.10+.
const MIN_PYTHON: (u32, u32) = (3, 10);
const HASH_FILE: &str = ".bundle-hash";
const DEPS_FILE: &str = ".deps-installed";

pub fn bundle_available() -> bool {
    !bundle::FILES.is_empty()
}

/// Where the Connector installs the print tool.
pub fn default_install_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("MamoConnector").join("proxy-printing"))
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

/// Parse `Python 3.12.4` → (3, 12).
pub fn parse_python_version(output: &str) -> Option<(u32, u32)> {
    let rest = output.trim().strip_prefix("Python ")?;
    let mut parts = rest.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next()?.trim().parse().ok()?;
    Some((major, minor))
}

fn python_meets_minimum(version: (u32, u32)) -> bool {
    version.0 > MIN_PYTHON.0 || (version.0 == MIN_PYTHON.0 && version.1 >= MIN_PYTHON.1)
}

fn probe_python(exe: &str) -> Option<(u32, u32)> {
    let mut cmd = Command::new(exe);
    cmd.arg("--version");
    hide_window(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None; // e.g. the Microsoft Store "python" stub
    }
    // Older Pythons print the version to stderr.
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    parse_python_version(&text)
}

/// A usable system Python (3.10+): PATH launchers first, then the per-user install location.
pub fn find_python() -> Option<String> {
    let mut candidates: Vec<String> = if cfg!(windows) {
        vec!["python".into(), "py".into()]
    } else {
        vec!["python3".into(), "python".into()]
    };
    if let Some(local) = dirs::data_local_dir() {
        let programs = local.join("Programs").join("Python");
        if let Ok(read) = std::fs::read_dir(programs) {
            let mut dirs: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
            dirs.sort();
            dirs.reverse(); // newest first
            candidates.extend(
                dirs.into_iter()
                    .map(|d| d.join("python.exe"))
                    .filter(|p| p.is_file())
                    .map(|p| p.to_string_lossy().to_string()),
            );
        }
    }
    candidates
        .into_iter()
        .find(|c| probe_python(c).map(python_meets_minimum).unwrap_or(false))
}

/// Interpreter inside the private venv, if it exists. `windowed` prefers `pythonw` (no console).
pub fn venv_python(dir: &Path, windowed: bool) -> Option<PathBuf> {
    let scripts = if cfg!(windows) { dir.join(".venv").join("Scripts") } else { dir.join(".venv").join("bin") };
    let names: &[&str] = match (cfg!(windows), windowed) {
        (true, true) => &["pythonw.exe", "python.exe"],
        (true, false) => &["python.exe"],
        (false, _) => &["python3", "python"],
    };
    names.iter().map(|n| scripts.join(n)).find(|p| p.is_file())
}

/// Write the embedded scripts into `dir` (never touching `.venv` or `ready2Print`).
/// Returns true when files were (re)written, false when the installed copy was already current.
pub fn extract_bundle(dir: &Path) -> Result<bool> {
    if !bundle_available() {
        bail!("The print tool is not bundled in this build of the MaMo Connector.");
    }
    std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let stamp = dir.join(HASH_FILE);
    let installed = dir.join("proxy_gui.py").is_file()
        && std::fs::read_to_string(&stamp).map(|h| h.trim() == bundle::HASH).unwrap_or(false);
    if installed {
        return Ok(false);
    }
    for (name, bytes) in bundle::FILES {
        std::fs::write(dir.join(name), bytes).with_context(|| format!("Cannot write {name}"))?;
    }
    std::fs::write(&stamp, bundle::HASH)?;
    // New scripts may need new packages: force the next setup to re-run pip.
    let _ = std::fs::remove_file(dir.join(DEPS_FILE));
    Ok(true)
}

fn run_logged(cmd: &mut Command, what: &str) -> Result<()> {
    hide_window(cmd);
    let out = cmd.output().with_context(|| format!("Failed to run: {what}"))?;
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        bail!("{what} failed ({}): {tail}", out.status);
    }
    Ok(())
}

/// Install Python 3.12 for the current user via winget (explicitly user-initiated only).
fn install_python_with_winget(progress: &dyn Fn(&str)) -> Result<()> {
    progress("Installing Python 3.12 via winget (this can take a minute)…");
    run_logged(
        Command::new("winget").args([
            "install",
            "-e",
            "--id",
            "Python.Python.3.12",
            "--scope",
            "user",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ]),
        "winget install Python.Python.3.12",
    )
}

#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub dir: PathBuf,
    pub message: String,
}

/// Install / refresh the print tool. `allow_python_install` lets it call winget when no suitable
/// Python exists (only the Setup button passes true).
pub fn setup(progress: &dyn Fn(&str), allow_python_install: bool) -> Result<SetupOutcome> {
    let dir = default_install_dir().ok_or_else(|| anyhow!("Could not find the local app data folder"))?;

    progress("Installing print tool files…");
    let refreshed = extract_bundle(&dir)?;
    info!("Print tool files in {} (refreshed: {refreshed})", dir.display());

    let python = match find_python() {
        Some(p) => p,
        None if allow_python_install && cfg!(windows) => {
            install_python_with_winget(progress)?;
            find_python().ok_or_else(|| {
                anyhow!("Python was installed but could not be found yet — restart the MaMo Connector and try again")
            })?
        }
        None => bail!(
            "Python 3.10 or newer is required. Open the MaMo Connector → Setup → \"Set up proxy printing\" to install it, or install it from python.org."
        ),
    };

    if venv_python(&dir, false).is_none() {
        progress("Creating Python environment…");
        run_logged(
            Command::new(&python).args(["-m", "venv", ".venv"]).current_dir(&dir),
            "python -m venv",
        )?;
    }
    let venv = venv_python(&dir, false).ok_or_else(|| anyhow!("The Python environment was not created"))?;

    if !dir.join(DEPS_FILE).is_file() {
        progress("Installing Python packages (fpdf2, Pillow, pyStrich, defusedxml)…");
        run_logged(
            Command::new(&venv)
                .args(["-m", "pip", "install", "--disable-pip-version-check", "-r", "requirements.txt"])
                .current_dir(&dir),
            "pip install -r requirements.txt",
        )?;
        std::fs::write(dir.join(DEPS_FILE), bundle::HASH)?;
    }

    Ok(SetupOutcome {
        message: format!(
            "Proxy printing is ready in {}. (A4 sheets and slip-in markers work out of the box; card-stock SLA output also needs Scribus.)",
            dir.display()
        ),
        dir,
    })
}

/// Cheap readiness check for the Setup tab: files extracted, venv and packages in place.
pub fn is_ready() -> bool {
    default_install_dir()
        .map(|d| d.join("proxy_gui.py").is_file() && venv_python(&d, false).is_some() && d.join(DEPS_FILE).is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_python_versions() {
        assert_eq!(parse_python_version("Python 3.12.4\n"), Some((3, 12)));
        assert_eq!(parse_python_version("Python 3.9.0"), Some((3, 9)));
        assert_eq!(parse_python_version("Python 2.7.18"), Some((2, 7)));
        assert_eq!(parse_python_version("command not found"), None);
        assert_eq!(parse_python_version(""), None);
    }

    #[test]
    fn enforces_minimum_python() {
        assert!(python_meets_minimum((3, 10)));
        assert!(python_meets_minimum((3, 13)));
        assert!(python_meets_minimum((4, 0)));
        assert!(!python_meets_minimum((3, 9)));
        assert!(!python_meets_minimum((2, 7)));
    }

    #[test]
    fn bundle_extracts_idempotently() {
        if !bundle_available() {
            return; // built without the sibling proxy-printing checkout
        }
        let dir = std::env::temp_dir().join(format!("mamo_bundle_test_{}", std::process::id()));
        assert!(extract_bundle(&dir).unwrap(), "first extract writes files");
        assert!(dir.join("proxy_gui.py").is_file());
        assert!(dir.join("requirements.txt").is_file());
        assert!(!extract_bundle(&dir).unwrap(), "second extract is a no-op");
        std::fs::write(dir.join(HASH_FILE), "stale").unwrap();
        assert!(extract_bundle(&dir).unwrap(), "a changed bundle hash re-extracts");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Touches the real install dir and PyPI — run manually: `cargo test setup_end_to_end -- --ignored`.
    #[test]
    #[ignore]
    fn setup_end_to_end() {
        let outcome = setup(&|m| println!("{m}"), false).unwrap();
        assert!(is_ready());
        let py = venv_python(&outcome.dir, false).unwrap();
        let out = Command::new(py).args(["-c", "import fpdf, PIL, pystrich, defusedxml"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn venv_python_absent_without_venv() {
        let dir = std::env::temp_dir().join("mamo_no_such_print_dir");
        assert!(venv_python(&dir, true).is_none());
    }
}
