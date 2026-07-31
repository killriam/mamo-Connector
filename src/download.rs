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

/// Resolves a release asset whose name satisfies `matches`, falling back to the first asset if
/// none match. Returns `(browser_download_url, asset_name)`.
async fn resolve_forge_asset_url(matches: impl Fn(&str) -> bool) -> Result<(String, String)> {
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

    let url = asset["browser_download_url"]
        .as_str()
        .context("Asset is missing browser_download_url")?
        .to_string();

    let name = asset["name"]
        .as_str()
        .context("Asset is missing name")?
        .to_string();

    Ok((url, name))
}

/// Resolves the portable bundle (JAR + `res/` resources) — what a fresh install needs, since
/// Forge looks for `res/` as loose files next to the jar, not on the classpath.
pub async fn resolve_forge_portable_zip_url() -> Result<(String, String)> {
    resolve_forge_asset_url(|n| n.ends_with(".zip")).await
}

/// Resolves the standalone JAR — enough to update an install that already has `res/` from a
/// previous portable-bundle extraction (much smaller download than re-fetching the whole zip).
pub async fn resolve_forge_jar_url() -> Result<(String, String)> {
    resolve_forge_asset_url(|n| n.ends_with("-jar-with-dependencies.jar")).await
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
    let (download_url, filename) = resolve_forge_portable_zip_url()
        .await
        .context("Could not resolve MaMo Forge portable bundle URL")?;

    let zip_path = download_to_file(&download_url, dest_dir, &filename, &on_progress, &cancelled).await?;

    extract_zip_to_dir(zip_path.clone(), dest_dir.to_path_buf())
        .await
        .context("Downloaded MaMo Forge but failed to extract it")?;
    let _ = std::fs::remove_file(&zip_path);

    crate::forge::resolve_latest_forge_jar(dest_dir)
        .context("Extracted MaMo Forge bundle but couldn't find a Forge jar inside it")
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
    let (download_url, filename) = resolve_forge_jar_url()
        .await
        .context("Could not resolve Forge JAR download URL")?;
    download_to_file(&download_url, dest_dir, &filename, &on_progress, &cancelled).await
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
