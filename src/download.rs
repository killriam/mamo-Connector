use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FORGE_RELEASES_API: &str =
    "https://api.github.com/repos/killriam/forge/releases/tags/replay-features-latest";

/// Progress update sent from the download thread to the UI on each chunk.
pub struct DownloadUpdate {
    pub bytes_done: u64,
    pub total_bytes: Option<u64>,
}

/// A resolved GitHub release asset — enough to download it and, later, tell whether the server
/// has since republished a different build under the same name.
#[derive(Debug, Clone)]
pub struct ForgeAsset {
    pub download_url: String,
    pub name: String,
    /// GitHub's `updated_at` timestamp for this specific asset (ISO 8601). `replay-features-latest`
    /// is a rolling tag — the release gets re-published from a new commit without the asset
    /// filename ever changing (it's a fixed `-SNAPSHOT-` name), so filename equality can never
    /// detect a newer build. This timestamp is the only field on the asset that actually changes
    /// when the server-side file is replaced. See FORGE_REPLAY_BUG.md for the incident (a fixed
    /// build sat published for a full day while an old local jar was never flagged as stale)
    /// that this was added to prevent from recurring.
    pub updated_at: String,
}

/// Resolves a release asset whose name satisfies `matches`, falling back to the first asset if
/// none match.
async fn resolve_forge_asset_url(matches: impl Fn(&str) -> bool) -> Result<ForgeAsset> {
    let client = reqwest::Client::builder()
        .user_agent("mamo-connector-forge-downloader")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let resp: serde_json::Value = client
        .get(FORGE_RELEASES_API)
        .send()
        .await
        .context("Failed to reach GitHub releases API")?
        .json()
        .await
        .context("Failed to parse GitHub releases API response")?;

    let assets = resp["assets"]
        .as_array()
        .filter(|a| !a.is_empty())
        .context("No release assets found — the forge build may not have run yet")?;

    let mut matching: Vec<&serde_json::Value> = assets
        .iter()
        .filter(|a| a["name"].as_str().map(&matches).unwrap_or(false))
        .collect();

    // Sort descending by updated_at and name so we always pick the newest asset if multiple exist
    matching.sort_by(|a, b| {
        let time_a = a["updated_at"].as_str().unwrap_or("");
        let time_b = b["updated_at"].as_str().unwrap_or("");
        time_b.cmp(time_a).then_with(|| {
            let name_a = a["name"].as_str().unwrap_or("");
            let name_b = b["name"].as_str().unwrap_or("");
            name_b.cmp(name_a)
        })
    });

    let asset = matching
        .first()
        .copied()
        .or_else(|| assets.first())
        .context("No suitable asset found in release")?;

    let download_url = asset["browser_download_url"]
        .as_str()
        .context("Asset is missing browser_download_url")?
        .to_string();

    let name = asset["name"]
        .as_str()
        .context("Asset is missing name")?
        .to_string();

    let updated_at = asset["updated_at"]
        .as_str()
        .context("Asset is missing updated_at")?
        .to_string();

    Ok(ForgeAsset { download_url, name, updated_at })
}

/// Resolves the portable bundle (JAR + `res/` resources) — what a fresh install needs, since
/// Forge looks for `res/` as loose files next to the jar, not on the classpath.
pub async fn resolve_forge_portable_zip_url() -> Result<ForgeAsset> {
    resolve_forge_asset_url(|n| n.ends_with(".zip")).await
}

/// Resolves the standalone JAR — enough to update an install that already has `res/` from a
/// previous portable-bundle extraction (much smaller download than re-fetching the whole zip).
pub async fn resolve_forge_jar_url() -> Result<ForgeAsset> {
    resolve_forge_asset_url(|n| n.ends_with("-jar-with-dependencies.jar")).await
}

/// Path to the sidecar metadata file recording which server-side build a downloaded jar came
/// from — `<jar>.source.json` next to it. Absence (older download, predating this file, or a
/// user-provided Forge path) is treated as "unknown" by the update check, not "up to date".
fn asset_meta_path(jar_path: &Path) -> PathBuf {
    let mut name = jar_path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".source.json");
    jar_path.with_file_name(name)
}

/// Records which build a just-downloaded jar came from, so a later `check_forge_update_available`
/// can tell a same-named-but-newer republish apart from a genuinely up-to-date local copy.
/// Best-effort: a write failure here just means the next update check treats this jar as
/// "unknown" (see `asset_meta_path`'s doc) rather than failing the download itself.
fn write_asset_meta(jar_path: &Path, asset: &ForgeAsset) {
    let meta = serde_json::json!({ "updated_at": asset.updated_at });
    if let Ok(content) = serde_json::to_string(&meta) {
        if let Err(e) = std::fs::write(asset_meta_path(jar_path), content) {
            log::warn!("Failed to write Forge asset metadata sidecar: {e}");
        }
    }
}

/// Reads back what `write_asset_meta` recorded for `jar_path`, if anything.
pub fn read_asset_meta_updated_at(jar_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(asset_meta_path(jar_path)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value["updated_at"].as_str().map(|s| s.to_string())
}

/// Streams `download_url` into `dest_dir/filename`, reporting progress and honoring
/// cancellation. Returns the path to the downloaded file.
async fn download_to_file(
    download_url: &str,
    dest_dir: &Path,
    filename: &str,
    on_progress: &(impl Fn(DownloadUpdate) + Send + Sync),
    cancelled: &Arc<AtomicBool>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_dir).context("Failed to create Forge download directory")?;
    let dest_file = dest_dir.join(filename);

    let client = reqwest::Client::builder()
        .user_agent("mamo-connector-forge-downloader")
        // Large timeout: the portable bundle is ~400 MB on a typical connection
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;

    let response = client
        .get(download_url)
        .send()
        .await
        .context("Failed to start download")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let total_bytes: Option<u64> = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let mut file = std::fs::File::create(&dest_file)
        .with_context(|| format!("Failed to create {:?}", dest_file))?;

    let mut bytes_done: u64 = 0;
    let mut stream = response.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        if cancelled.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&dest_file);
            anyhow::bail!("Download cancelled");
        }
        let chunk = chunk_result.context("Error reading download stream")?;
        bytes_done += chunk.len() as u64;
        file.write_all(&chunk).context("Failed to write chunk to file")?;
        on_progress(DownloadUpdate { bytes_done, total_bytes });
    }

    file.flush().context("Failed to flush download file")?;
    Ok(dest_file)
}

/// Returns the single top-level path component shared by every entry in the archive, if there
/// is one (e.g. `MaMoForge-portable/` when everything is nested under one wrapper folder).
/// Returns `None` for a flat zip (entries with differing or no top-level component), in which
/// case extraction proceeds unprefixed exactly as the archive lays out.
fn common_top_level_dir(archive: &mut zip::ZipArchive<std::fs::File>) -> Result<Option<PathBuf>> {
    let mut common: Option<std::ffi::OsString> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let Some(relative_path) = entry.enclosed_name() else { continue };
        let Some(first) = relative_path.components().next() else { return Ok(None) };
        let first = first.as_os_str().to_os_string();
        match &common {
            None => common = Some(first),
            Some(existing) if *existing == first => {}
            Some(_) => return Ok(None),
        }
    }
    Ok(common.map(PathBuf::from))
}

/// Extracts every entry of the zip at `zip_path` into `dest_dir`, stripping a common top-level
/// wrapper folder if the whole archive is nested under one (so the jar and `res/` land directly
/// in `dest_dir`, matching how the portable bundle is packaged, whether or not GitHub/CI wraps it
/// in a directory named after the release). Runs on a blocking thread since the `zip` crate is
/// synchronous and a ~400MB archive can take a few seconds to unpack.
async fn extract_zip_to_dir(zip_path: PathBuf, dest_dir: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&zip_path)
            .with_context(|| format!("Failed to open downloaded archive {:?}", zip_path))?;
        let mut archive = zip::ZipArchive::new(file).context("Downloaded file is not a valid zip archive")?;

        let strip_prefix = common_top_level_dir(&mut archive)?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            // `enclosed_name()` rejects entries that would escape `dest_dir` (zip-slip
            // protection) — anything unsafe is silently skipped rather than failing the whole
            // extraction over one bad entry.
            let Some(full_path) = entry.enclosed_name() else { continue };
            let relative_path = match &strip_prefix {
                Some(prefix) => full_path.strip_prefix(prefix).unwrap_or(&full_path),
                None => &full_path,
            };
            if relative_path.as_os_str().is_empty() {
                // The entry *is* the wrapper folder itself; dest_dir already exists.
                continue;
            }
            let out_path = dest_dir.join(relative_path);

            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out_file = std::fs::File::create(&out_path)
                    .with_context(|| format!("Failed to create {:?}", out_path))?;
                std::io::copy(&mut entry, &mut out_file)?;
            }
        }
        Ok(())
    })
    .await
    .context("Extraction task panicked")??;
    Ok(())
}

/// Downloads and extracts the MaMo Forge portable bundle (JAR + `res/`) into `dest_dir` — used
/// for a fresh install, since there's no pre-existing `res/` to rely on yet.
///
/// Calls `on_progress` during the download phase; checks `cancelled` before each chunk (deletes
/// the partial zip and returns an error if set). The zip itself is deleted after a successful
/// extraction so it doesn't sit alongside the extracted files duplicating ~400MB.
///
/// Returns the path to the extracted jar (found via `resolve_latest_forge_jar`).
pub async fn download_forge_portable(
    dest_dir: &Path,
    on_progress: impl Fn(DownloadUpdate) + Send + Sync + 'static,
    cancelled: Arc<AtomicBool>,
) -> Result<PathBuf> {
    let asset = resolve_forge_portable_zip_url()
        .await
        .context("Could not resolve MaMo Forge portable bundle URL")?;

    let zip_path =
        download_to_file(&asset.download_url, dest_dir, &asset.name, &on_progress, &cancelled).await?;

    extract_zip_to_dir(zip_path.clone(), dest_dir.to_path_buf())
        .await
        .context("Downloaded MaMo Forge but failed to extract it")?;
    let _ = std::fs::remove_file(&zip_path);

    let jar_path = crate::forge::resolve_latest_forge_jar(dest_dir)
        .context("Extracted MaMo Forge bundle but couldn't find a Forge jar inside it")?;
    let jar_meta_asset = resolve_forge_jar_url().await.unwrap_or_else(|_| asset.clone());
    write_asset_meta(&jar_path, &jar_meta_asset);
    cleanup_old_forge_jars(dest_dir, &jar_path);
    Ok(jar_path)
}

/// Downloads just the standalone JAR into `dest_dir` **under a staging filename**
/// (`<name>.update`) rather than replacing the live jar directly — used by the background
/// auto-updater to update an install that already has `res/` from a previous
/// `download_forge_portable` call. Downloading in place used to risk writing over a jar Forge
/// might currently have open; the caller now finalizes the swap itself (rename staged → live)
/// once it has confirmed Forge isn't running — see `finalize_staged_forge_jar`.
///
/// Calls `on_progress` after each received chunk. Checks `cancelled` before each chunk — if
/// set, deletes the partial file and returns an error.
///
/// Returns the staged file's path and the resolved asset info (needed to write the sidecar
/// metadata once finalized) on success.
pub async fn download_forge_jar_staged(
    dest_dir: &Path,
    on_progress: impl Fn(DownloadUpdate) + Send + Sync + 'static,
    cancelled: Arc<AtomicBool>,
) -> Result<(PathBuf, ForgeAsset)> {
    let asset = resolve_forge_jar_url()
        .await
        .context("Could not resolve Forge JAR download URL")?;
    let staging_name = format!("{}.update", asset.name);
    let staged_path =
        download_to_file(&asset.download_url, dest_dir, &staging_name, &on_progress, &cancelled).await?;
    Ok((staged_path, asset))
}

/// Swaps a staged download (from `download_forge_jar_staged`) into place as `dest_dir/<asset
/// name>`, replacing whatever's there, and records the sidecar metadata. Caller is responsible
/// for confirming Forge isn't currently running before calling this — an atomic rename is safe
/// against a *closed* Forge's next launch reading a half-written file, but not against a
/// *currently open* Forge that might still be reading from the path being replaced.
pub fn finalize_staged_forge_jar(dest_dir: &Path, staged_path: &Path, asset: &ForgeAsset) -> Result<PathBuf> {
    let final_path = dest_dir.join(&asset.name);
    std::fs::rename(staged_path, &final_path)
        .with_context(|| format!("Failed to move staged Forge update into place at {:?}", final_path))?;
    write_asset_meta(&final_path, asset);
    cleanup_old_forge_jars(dest_dir, &final_path);
    Ok(final_path)
}

/// Cleans up any old Forge JAR files (matching `forge-gui-desktop-*-jar-with-dependencies.jar`)
/// in `dest_dir` except for the one specified by `keep_path`. Also removes their corresponding
/// `<jar>.source.json` sidecar files.
pub fn cleanup_old_forge_jars(dest_dir: &Path, keep_path: &Path) {
    let entries = match std::fs::read_dir(dest_dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Failed to read Forge directory for cleanup: {e}");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path != keep_path {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let name_lower = name.to_lowercase();
                if name_lower.starts_with("forge-gui-desktop-")
                    && name_lower.ends_with("-jar-with-dependencies.jar")
                {
                    log::info!("Pruning old Forge JAR: {:?}", path);
                    if let Err(e) = std::fs::remove_file(&path) {
                        log::warn!("Failed to delete old Forge JAR {:?}: {e}", path);
                    }
                    let meta_path = asset_meta_path(&path);
                    if meta_path.exists() {
                        let _ = std::fs::remove_file(&meta_path);
                    }
                }
            }
        }
    }
}

const CONNECTOR_RELEASES_API: &str =
    "https://api.github.com/repos/killriam/mamo-Connector/releases/latest";

/// A resolved GitHub release asset for MaMo Connector desktop app.
#[derive(Debug, Clone)]
pub struct ConnectorAsset {
    pub version: String,
    pub download_url: String,
    pub name: String,
    pub size: Option<u64>,
}

/// Resolves the latest release asset for MaMo Connector.
pub async fn resolve_connector_release_asset() -> Result<ConnectorAsset> {
    let client = reqwest::Client::builder()
        .user_agent("mamo-connector-update-check")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let resp: serde_json::Value = client
        .get(CONNECTOR_RELEASES_API)
        .send()
        .await
        .context("Failed to reach GitHub releases API for MaMo Connector")?
        .json()
        .await
        .context("Failed to parse GitHub releases API response for MaMo Connector")?;

    let tag = resp["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No tag_name in release response"))?
        .trim_start_matches('v')
        .to_string();

    let assets = resp["assets"]
        .as_array()
        .filter(|a| !a.is_empty())
        .context("No release assets found in MaMo Connector release")?;

    // Look for Windows executable asset (e.g. mamo-connector-v0.3.8-windows-x64.exe or any .exe)
    let asset = assets
        .iter()
        .find(|a| {
            a["name"]
                .as_str()
                .map(|n| n.ends_with(".exe") && (n.contains("windows") || n.contains("connector")))
                .unwrap_or(false)
        })
        .or_else(|| {
            assets.iter().find(|a| a["name"].as_str().map(|n| n.ends_with(".exe")).unwrap_or(false))
        })
        .context("No suitable executable asset found in release")?;

    let download_url = asset["browser_download_url"]
        .as_str()
        .context("Asset is missing browser_download_url")?
        .to_string();

    let name = asset["name"]
        .as_str()
        .context("Asset is missing name")?
        .to_string();

    let size = asset["size"].as_u64();

    Ok(ConnectorAsset {
        version: tag,
        download_url,
        name,
        size,
    })
}

/// Directory where staged MaMo Connector updates are downloaded.
pub fn connector_updates_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("mamo-connector")
        .join("updates")
}

/// Downloads the MaMo Connector binary to a staged file in the updates folder.
pub async fn download_connector_update_staged(
    asset: &ConnectorAsset,
    on_progress: impl Fn(DownloadUpdate) + Send + Sync,
    cancelled: Arc<AtomicBool>,
) -> Result<PathBuf> {
    let dest_dir = connector_updates_dir();
    let temp_name = format!("{}.staged", asset.name);
    let staged_path = download_to_file(
        &asset.download_url,
        &dest_dir,
        &temp_name,
        &on_progress,
        &cancelled,
    )
    .await?;

    let final_path = dest_dir.join(&asset.name);
    if final_path.exists() {
        let _ = std::fs::remove_file(&final_path);
    }
    std::fs::rename(&staged_path, &final_path)
        .with_context(|| format!("Failed to move staged connector update into place at {:?}", final_path))?;

    Ok(final_path)
}

/// Removes stale `.old` backup executables or temporary `.staged` files from previous updates,
/// and cleans up old downloaded release binaries keeping at most the 2 newest versions.
pub fn cleanup_old_connector_backups() {
    if let Ok(current_exe) = std::env::current_exe() {
        let old_exe = current_exe.with_extension("exe.old");
        if old_exe.exists() {
            let _ = std::fs::remove_file(&old_exe);
        }
        let old_exe2 = current_exe.with_extension("old");
        if old_exe2.exists() {
            let _ = std::fs::remove_file(&old_exe2);
        }
    }
    let updates_dir = connector_updates_dir();
    if let Ok(entries) = std::fs::read_dir(&updates_dir) {
        let mut exe_files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "staged" {
                        let _ = std::fs::remove_file(&path);
                    } else if ext == "exe" {
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            if let Ok(modified) = metadata.modified() {
                                exe_files.push((path, modified));
                            }
                        }
                    }
                }
            }
        }

        // Sort by modification time, oldest first
        exe_files.sort_by_key(|x| x.1);

        // Keep at most 2 update executables (the current/latest updates) and prune the rest
        if exe_files.len() > 2 {
            let to_delete_count = exe_files.len() - 2;
            for i in 0..to_delete_count {
                let _ = std::fs::remove_file(&exe_files[i].0);
            }
        }
    }
}

/// Swaps the currently running Connector executable with `new_exe_path` and relaunches the app.
pub fn apply_connector_update_and_restart(new_exe_path: &Path) -> Result<()> {
    let current_exe = std::env::current_exe().context("Failed to get current executable path")?;

    #[cfg(windows)]
    {
        let old_backup = current_exe.with_extension("exe.old");
        if old_backup.exists() {
            let _ = std::fs::remove_file(&old_backup);
        }

        // On Windows, a running executable can be renamed even while executing
        if let Err(e) = std::fs::rename(&current_exe, &old_backup) {
            log::warn!("Failed to rename running exe to .old ({e}); falling back to direct launch");
            let args: Vec<String> = std::env::args().skip(1).collect();
            std::process::Command::new(new_exe_path)
                .args(&args)
                .spawn()
                .context("Failed to spawn new connector process from updates dir")?;
            std::process::exit(0);
        }

        // Copy the new binary into the target current_exe path
        if let Err(e) = std::fs::copy(new_exe_path, &current_exe) {
            log::warn!("Failed to copy new executable to {:?}: {e}; launching from updates dir", current_exe);
            // Restore original name if copy fails
            let _ = std::fs::rename(&old_backup, &current_exe);
            let args: Vec<String> = std::env::args().skip(1).collect();
            std::process::Command::new(new_exe_path)
                .args(&args)
                .spawn()
                .context("Failed to spawn new connector process from updates dir")?;
            std::process::exit(0);
        }

        // Launch the updated executable in place
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::Command::new(&current_exe)
            .args(&args)
            .spawn()
            .context("Failed to launch updated executable")?;

        std::process::exit(0);
    }

    #[cfg(not(windows))]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        std::process::Command::new(new_exe_path)
            .args(&args)
            .spawn()
            .context("Failed to launch updated executable")?;
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_test_zip(dest_dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let zip_path = dest_dir.join("test.zip");
        std::fs::create_dir_all(dest_dir).unwrap();
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (name, contents) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
        zip_path
    }

    #[tokio::test]
    async fn extract_zip_strips_common_top_level_dir() {
        let dest = std::env::temp_dir().join("mamo-connector-zip-test-wrapped");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let zip_path = write_test_zip(
            &dest,
            &[
                ("MaMoForge-portable/forge.jar", b"fake jar".as_slice()),
                ("MaMoForge-portable/res/skins/default/bg_splash.png", b"fake png".as_slice()),
            ],
        );

        extract_zip_to_dir(zip_path, dest.clone()).await.expect("extraction should succeed");

        assert!(dest.join("forge.jar").exists(), "jar should be extracted directly into dest_dir, not nested under the wrapper folder");
        assert!(dest.join("res").join("skins").join("default").join("bg_splash.png").exists());
        assert!(!dest.join("MaMoForge-portable").exists(), "wrapper folder itself should not be recreated");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[tokio::test]
    async fn extract_zip_leaves_flat_archive_unprefixed() {
        let dest = std::env::temp_dir().join("mamo-connector-zip-test-flat");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let zip_path = write_test_zip(
            &dest,
            &[("forge.jar", b"fake jar".as_slice()), ("res/howto.txt", b"hi".as_slice())],
        );

        extract_zip_to_dir(zip_path, dest.clone()).await.expect("extraction should succeed");

        assert!(dest.join("forge.jar").exists());
        assert!(dest.join("res").join("howto.txt").exists());

        let _ = std::fs::remove_dir_all(&dest);
    }

    // Regression coverage for the FORGE_REPLAY_BUG.md incident: a same-named rolling-tag
    // asset republished with new content must still be detected as "different" via
    // updated_at, since the filename alone never changes between builds.

    #[test]
    fn asset_meta_round_trips_through_sidecar_file() {
        let dest = std::env::temp_dir().join("mamo-connector-asset-meta-test-roundtrip");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let jar_path = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-jar-with-dependencies.jar");
        std::fs::write(&jar_path, b"fake jar").unwrap();

        let asset = ForgeAsset {
            download_url: "https://example.com/forge.jar".to_string(),
            name: jar_path.file_name().unwrap().to_string_lossy().to_string(),
            updated_at: "2026-08-14T08:55:25Z".to_string(),
        };
        write_asset_meta(&jar_path, &asset);

        assert_eq!(
            read_asset_meta_updated_at(&jar_path),
            Some("2026-08-14T08:55:25Z".to_string())
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn asset_meta_is_none_when_sidecar_missing() {
        // A jar downloaded before this fix shipped (or a user-provided Forge path) has no
        // sidecar — the update check must treat that as "unknown", not "up to date".
        let dest = std::env::temp_dir().join("mamo-connector-asset-meta-test-missing");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let jar_path = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-jar-with-dependencies.jar");
        std::fs::write(&jar_path, b"fake jar").unwrap();

        assert_eq!(read_asset_meta_updated_at(&jar_path), None);

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn finalize_staged_forge_jar_moves_file_into_place_and_writes_meta() {
        let dest = std::env::temp_dir().join("mamo-connector-finalize-test");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let asset = ForgeAsset {
            download_url: "https://example.com/forge.jar".to_string(),
            name: "forge-gui-desktop-2.0.14-SNAPSHOT-jar-with-dependencies.jar".to_string(),
            updated_at: "2026-08-14T08:55:25Z".to_string(),
        };
        let staged_path = dest.join(format!("{}.update", asset.name));
        std::fs::write(&staged_path, b"new jar contents").unwrap();
        // An old "live" jar already sitting at the destination — finalize must replace it.
        let final_path_expected = dest.join(&asset.name);
        std::fs::write(&final_path_expected, b"old jar contents").unwrap();

        let final_path =
            finalize_staged_forge_jar(&dest, &staged_path, &asset).expect("finalize should succeed");

        assert_eq!(final_path, final_path_expected);
        assert!(!staged_path.exists(), "staged file should have been moved, not copied");
        assert_eq!(std::fs::read(&final_path).unwrap(), b"new jar contents");
        assert_eq!(
            read_asset_meta_updated_at(&final_path),
            Some("2026-08-14T08:55:25Z".to_string())
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// Real network test against the live killriam/forge release — not run by default (slow,
    /// downloads ~400MB). Run explicitly with:
    ///   cargo test --bin mamo-connector download::tests::download_and_extract_real_portable_bundle -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn download_and_extract_real_portable_bundle() {
        let dest = std::env::temp_dir().join("mamo-connector-portable-download-test");
        let _ = std::fs::remove_dir_all(&dest);

        let cancelled = Arc::new(AtomicBool::new(false));
        let jar_path = download_forge_portable(
            &dest,
            |update| {
                if let Some(total) = update.total_bytes {
                    let pct = (update.bytes_done as f64 / total as f64) * 100.0;
                    eprintln!("  {:.1}% ({} / {} bytes)", pct, update.bytes_done, total);
                }
            },
            cancelled,
        )
        .await
        .expect("download_forge_portable should succeed against the live release");

        assert!(jar_path.exists(), "resolved jar path should exist: {jar_path:?}");
        assert!(
            dest.join("res").join("skins").join("default").join("bg_splash.png").exists(),
            "extracted bundle should include res/skins/default/bg_splash.png next to the jar"
        );
        // The zip itself should have been cleaned up after extraction, not left duplicating ~400MB.
        let leftover_zips: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "zip").unwrap_or(false))
            .collect();
        assert!(leftover_zips.is_empty(), "zip should be removed after extraction, found: {leftover_zips:?}");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn connector_updates_dir_creates_valid_path() {
        let dir = connector_updates_dir();
        assert!(dir.ends_with("updates"));
    }

    #[test]
    fn cleanup_old_connector_backups_cleans_staged_files() {
        let updates_dir = connector_updates_dir();
        let _ = std::fs::create_dir_all(&updates_dir);
        let test_staged = updates_dir.join("test-mamo-update.exe.staged");
        std::fs::write(&test_staged, b"dummy staged").unwrap();
        assert!(test_staged.exists());

        cleanup_old_connector_backups();
        assert!(!test_staged.exists(), "cleanup should remove stale .staged files");
    }

    #[test]
    fn cleanup_old_forge_jars_removes_other_jars_and_metadata() {
        let dest = std::env::temp_dir().join("mamo-connector-cleanup-forge-jars-test");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        let old_jar = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-08.13-2020-jar-with-dependencies.jar");
        std::fs::write(&old_jar, b"old jar").unwrap();
        let old_meta = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-08.13-2020-jar-with-dependencies.jar.source.json");
        std::fs::write(&old_meta, b"{}").unwrap();

        let new_jar = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-08.17-0710-jar-with-dependencies.jar");
        std::fs::write(&new_jar, b"new jar").unwrap();
        let new_meta = dest.join("forge-gui-desktop-2.0.14-SNAPSHOT-08.17-0710-jar-with-dependencies.jar.source.json");
        std::fs::write(&new_meta, b"{}").unwrap();

        // Also a non-forge jar that should not be deleted
        let random_file = dest.join("random.txt");
        std::fs::write(&random_file, b"should keep").unwrap();

        cleanup_old_forge_jars(&dest, &new_jar);

        assert!(!old_jar.exists(), "old jar should be cleaned up");
        assert!(!old_meta.exists(), "old metadata sidecar should be cleaned up");
        assert!(new_jar.exists(), "new jar should be kept");
        assert!(new_meta.exists(), "new metadata should be kept");
        assert!(random_file.exists(), "unrelated file should be kept");

        let _ = std::fs::remove_dir_all(&dest);
    }
}
