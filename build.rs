use std::process::Command;

fn main() {
    // Tell Cargo to re-run this build script if .git/HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
    
    // Get git commit hash (short)
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    
    // Check if working directory is dirty
    let is_dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false);
    
    let dirty_suffix = if is_dirty { "-dirty" } else { "" };
    
    // Get branch name
    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    
    // Get build timestamp
    let build_time = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
    
    // Export as environment variables for the build
    println!("cargo:rustc-env=GIT_HASH={}{}", git_hash, dirty_suffix);
    println!("cargo:rustc-env=GIT_BRANCH={}", branch);
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);

    bundle_proxy_printing();
}

/// Embed the proxy-printing tool (sibling checkout `../proxy-printing`, top-level files only) so
/// the Connector can install it locally without a network download. Generates
/// `$OUT_DIR/proxy_printing_bundle.rs`; when the folder is missing the bundle is simply empty
/// and the Setup tab reports "not bundled in this build".
fn bundle_proxy_printing() {
    use std::fmt::Write as _;

    let src_dir = std::path::Path::new("..").join("proxy-printing");
    println!("cargo:rerun-if-changed=../proxy-printing");

    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&src_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if path.is_file() && !name.starts_with('.') && matches!(ext.as_str(), "py" | "bat" | "sla" | "txt" | "md") {
                if let Ok(abs) = std::fs::canonicalize(&path) {
                    println!("cargo:rerun-if-changed={}", path.display());
                    entries.push((name, abs));
                }
            }
        }
    }
    entries.sort();

    // FNV-1a over names + contents: lets the Connector refresh an installed copy when it changes.
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[
");
    for (name, abs) in &entries {
        feed(name.as_bytes());
        if let Ok(bytes) = std::fs::read(abs) {
            feed(&bytes);
        }
        // `{:?}` escapes backslashes in the Windows path for the generated string literal.
        let _ = writeln!(code, "    ({:?}, include_bytes!({:?})),", name, abs.to_string_lossy());
    }
    code.push_str("];
");
    let _ = writeln!(code, "pub const HASH: &str = \"{hash:016x}\";");

    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        let _ = std::fs::write(std::path::Path::new(&out_dir).join("proxy_printing_bundle.rs"), code);
    }
}
