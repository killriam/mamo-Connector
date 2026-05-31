use anyhow::{Context, Result};
use cfg_if::cfg_if;

#[derive(Debug, Clone)]
pub struct RegistrationOutcome {
    pub status: RegistrationStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RegistrationStatus {
    Registered,
    Failed,
    Skipped,
}

#[allow(dead_code)]
impl RegistrationOutcome {
    pub fn registered(message: impl Into<String>) -> Self {
        Self {
            status: RegistrationStatus::Registered,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: RegistrationStatus::Failed,
            message: message.into(),
        }
    }

    pub fn skipped(message: impl Into<String>) -> Self {
        Self {
            status: RegistrationStatus::Skipped,
            message: message.into(),
        }
    }
}

/// Remove the custom URL scheme registration created by `ensure_registered`.
pub fn unregister(scheme: &str) -> Result<()> {
    cfg_if! {
        if #[cfg(windows)] {
            unregister_windows(scheme)
        } else if #[cfg(target_os = "linux")] {
            unregister_linux(scheme)
        } else {
            // macOS: LaunchServices has no public removal API; nothing to do
            let _ = scheme;
            Ok(())
        }
    }
}

pub fn ensure_registered(scheme: &str) -> Result<RegistrationOutcome> {
    cfg_if! {
        if #[cfg(windows)] {
            register_windows(scheme)
        } else if #[cfg(target_os = "macos")] {
            register_macos(scheme)
        } else if #[cfg(target_os = "linux")] {
            register_linux(scheme)
        } else {
            Ok(RegistrationOutcome::skipped(format!(
                "Custom scheme registration not supported on this platform (scheme: {scheme})"
            )))
        }
    }
}

#[cfg(windows)]
fn register_windows(scheme: &str) -> Result<RegistrationOutcome> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .create_subkey("Software\\Classes")
        .context("unable to open HKCU\\Software\\Classes")?
        .0;

    let (scheme_key, _disp) = classes
        .create_subkey(scheme)
        .with_context(|| format!("unable to create registry key for scheme {scheme}"))?;

    let exe = std::env::current_exe().context("unable to locate current executable")?;
    let exe_str: String = format_path(exe.as_path());

    scheme_key.set_value("", &format!("URL:{scheme} Protocol"))?;
    scheme_key.set_value("URL Protocol", &"")?;

    let (command_key, _) = scheme_key
        .create_subkey("shell\\open\\command")
        .context("unable to create shell\\open\\command key")?;
    command_key.set_value("", &format!("\"{exe_str}\" \"%1\""))?;

    Ok(RegistrationOutcome::registered(format!(
        "Registered custom scheme '{scheme}' for executable {exe_str}"
    )))
}

#[cfg(windows)]
fn format_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(target_os = "macos")]
fn register_macos(scheme: &str) -> Result<RegistrationOutcome> {
    use core_foundation::base::TCFType;
    use core_foundation::bundle::CFBundle;
    use core_foundation::string::CFString;

    let bundle = CFBundle::main_bundle();
    let identifier = bundle
        .and_then(|bundle| bundle.identifier())
        .map(|id| id.to_string());

    let bundle_id = identifier
        .filter(|id| !id.is_empty())
        .context("unable to determine bundle identifier for current executable")?;

    let scheme_cf = CFString::new(scheme);
    let bundle_cf = CFString::new(&bundle_id);

    unsafe {
        let status = ls_set_default_handler_for_url_scheme(
            scheme_cf.as_concrete_TypeRef(),
            bundle_cf.as_concrete_TypeRef(),
        );
        if status == 0 {
            Ok(RegistrationOutcome::registered(format!(
                "Registered custom scheme '{scheme}' for bundle {bundle_id}"
            )))
        } else {
            Err(anyhow::anyhow!(
                "LaunchServices returned status {status} while registering scheme"
            ))
        }
    }
}

#[cfg(target_os = "macos")]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSSetDefaultHandlerForURLScheme(
        inScheme: core_foundation_sys::string::CFStringRef,
        inBundleID: core_foundation_sys::string::CFStringRef,
    ) -> i32;
}

#[cfg(target_os = "macos")]
unsafe fn ls_set_default_handler_for_url_scheme(
    scheme: core_foundation_sys::string::CFStringRef,
    bundle: core_foundation_sys::string::CFStringRef,
) -> i32 {
    LSSetDefaultHandlerForURLScheme(scheme, bundle)
}

#[cfg(target_os = "linux")]
fn register_linux(scheme: &str) -> Result<RegistrationOutcome> {
    use directories::BaseDirs;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let base_dirs = BaseDirs::new().context("unable to locate home directory")?;
    let applications_dir = base_dirs.data_local_dir().join("applications");
    fs::create_dir_all(&applications_dir)
        .with_context(|| format!("unable to create {:?}", applications_dir))?;

    let desktop_file_path = applications_dir.join(format!("{scheme}.desktop"));
    let exe = std::env::current_exe().context("unable to locate current executable")?;
    let exe_str = exe.to_string_lossy();

    let desktop_entry = format!(
        "[Desktop Entry]\nType=Application\nName={scheme}\nExec={path} %u\nTerminal=false\nMimeType=x-scheme-handler/{scheme};\n",
        path = exe_str,
        scheme = scheme
    );

    fs::write(&desktop_file_path, desktop_entry)
        .with_context(|| format!("unable to write {:?}", desktop_file_path))?;

    let mut permissions = fs::metadata(&desktop_file_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&desktop_file_path, permissions)?;

    let status = match Command::new("xdg-mime")
        .arg("default")
        .arg(desktop_file_path.file_name().unwrap())
        .arg(format!("x-scheme-handler/{scheme}"))
        .status()
    {
        Ok(status) => status,
        Err(err) => {
            return Ok(RegistrationOutcome::skipped(format!(
                "Wrote desktop file but failed to invoke xdg-mime: {err}"
            )));
        }
    };

    if !status.success() {
        return Ok(RegistrationOutcome::skipped(
            "xdg-mime reported failure; custom scheme may require manual setup".to_string(),
        ));
    }

    if let Err(err) = Command::new("update-desktop-database")
        .arg(&applications_dir)
        .status()
    {
        log::warn!("update-desktop-database failed: {err}. Desktop cache may need manual refresh");
    }

    Ok(RegistrationOutcome::registered(format!(
        "Registered custom scheme '{scheme}' via {:?}",
        desktop_file_path
    )))
}

#[cfg(windows)]
fn unregister_windows(scheme: &str) -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey("Software\\Classes")
        .context("unable to open HKCU\\Software\\Classes")?;

    // delete_subkey_all removes the key and all its children
    match classes.delete_subkey_all(scheme) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // already gone
        Err(e) => Err(e).context(format!("unable to remove registry key for scheme {scheme}")),
    }
}

#[cfg(target_os = "linux")]
fn unregister_linux(scheme: &str) -> Result<()> {
    use directories::BaseDirs;
    use std::fs;
    use std::process::Command;

    let base_dirs = BaseDirs::new().context("unable to locate home directory")?;
    let desktop_file_path = base_dirs
        .data_local_dir()
        .join("applications")
        .join(format!("{scheme}.desktop"));

    if desktop_file_path.exists() {
        fs::remove_file(&desktop_file_path)
            .with_context(|| format!("unable to remove {:?}", desktop_file_path))?;
        let _ = Command::new("update-desktop-database")
            .arg(desktop_file_path.parent().unwrap())
            .status();
    }
    Ok(())
}
