use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FORGE_RELEASES_API: &str =
    "https://api.github.com/repos/killriam/forge/releases/tags/replay-features-latest";

/// Progress update sent from the download thread to the UI on each chunk.
pub struct DownloadUpdate {
    pub bytes_done: u64,
    pub total_bytes: Option<u64>,
}

/// Resolves the actual JAR download URL and filename from the GitHub releases API.
/// Returns `(browser_download_url, asset_name)`.
pub async fn resolve_forge_download_url() -> Result<(String, String)> {
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

    // Prefer the jar-with-dependencies asset; fall back to the first asset
    let asset = assets
        .iter()
        .find(|a| {
            a["name"]
                .as_str()
                .map(|n| n.ends_with("-jar-with-dependencies.jar"))
                .unwrap_or(false)
        })
        .or_else(|| assets.first())
        .context("No suitable JAR asset found in release")?;

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

/// Downloads the MaMo Forge JAR into `dest_dir`.
///
/// Calls `on_progress` after each received chunk. Checks `cancelled` before
/// each chunk — if set, deletes the partial file and returns an error.
///
/// Returns the path to the downloaded JAR on success.
pub async fn download_forge_jar(
    dest_dir: &std::path::Path,
    on_progress: impl Fn(DownloadUpdate) + Send + 'static,
    cancelled: Arc<AtomicBool>,
) -> Result<PathBuf> {
    let (download_url, filename) = resolve_forge_download_url()
        .await
        .context("Could not resolve Forge download URL")?;

    std::fs::create_dir_all(dest_dir)
        .context("Failed to create Forge download directory")?;

    let dest_file = dest_dir.join(&filename);

    let client = reqwest::Client::builder()
        .user_agent("mamo-connector-forge-downloader")
        // Large timeout: JAR is 100-300 MB on a typical connection
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;

    let response = client
        .get(&download_url)
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
