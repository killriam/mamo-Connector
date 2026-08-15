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

    let asset = assets
        .iter()
        .find(|a| a["name"].as_str().map(&matches).unwrap_or(false))
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
    write_asset_meta(&jar_path, &asset);
    Ok(jar_path)
}

/// Downloads just the standalone JAR into `dest_dir`, replacing any existing one — used to
/// update an install that already has `res/` from a previous `download_forge_portable` call.
///
/// Calls `on_progress` after each received chunk. Checks `cancelled` before each chunk — if
/// set, deletes the partial file and returns an error.
///
/// Returns the path to the downloaded JAR on success.
pub async fn download_forge_jar(
    dest_dir: &Path,
    on_progress: impl Fn(DownloadUpdate) + Send + Sync + 'static,
    cancelled: Arc<AtomicBool>,
) -> Result<PathBuf> {
    let asset = resolve_forge_jar_url()
        .await
        .context("Could not resolve Forge JAR download URL")?;
    let jar_path =
        download_to_file(&asset.download_url, dest_dir, &asset.name, &on_progress, &cancelled).await?;
    write_asset_meta(&jar_path, &asset);
    Ok(jar_path)
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
}
