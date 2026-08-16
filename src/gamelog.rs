//! Game Log Reader Module
//! 
//! Monitors a configured directory for new MTG game log files (from Forge MTG),
//! parses them, and uploads them to the backend API for storage and analysis.
//! 
//! # Architecture
//! 
//! 1. **File Watcher**: Monitors a directory for new `.json` game log files
//! 2. **File Parser**: Reads and validates JSON game log content (MTG Replay Notation)
//! 3. **API Upload**: Sends parsed logs to backend as structured data
//! 4. **Status Tracking**: Tracks which files have been processed
//!
//! # File Format
//!
//! Only JSON files are supported. Game logs follow the MTG Replay & Learning Notation
//! format (see MTG-REPLAY-NOTATION.md), a machine-readable JSON format containing
//! game metadata, card index, event logs (L1), and learning views (L2).

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

/// Status of the game log watcher
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherStatus {
    Stopped,
    Running,
    Error(String),
}

/// Result of processing a single game log file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameLogProcessResult {
    pub filename: String,
    pub success: bool,
    pub message: String,
    pub file_size: u64,
    pub processed_at: String,
    /// Server-side ID if upload succeeded
    pub server_id: Option<String>,
    /// Deck identifier extracted from the file
    pub deck_identifier: Option<String>,
    /// The authoritative MaMo deck UUID this log was matched to, if the local deck mappings
    /// resolved one (see `DeckMappings::get_mapping`) — lets the UI deep-link straight to that
    /// deck's game analysis instead of just reporting an upload happened.
    #[serde(default)]
    pub resolved_deck_id: Option<String>,
}

impl GameLogProcessResult {
    pub fn success(
        filename: String,
        file_size: u64,
        server_id: Option<String>,
        deck_identifier: Option<String>,
        resolved_deck_id: Option<String>,
    ) -> Self {
        Self {
            filename,
            success: true,
            message: "Successfully processed and uploaded".to_string(),
            file_size,
            processed_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            server_id,
            deck_identifier,
            resolved_deck_id,
        }
    }

    pub fn failed(filename: String, error: String) -> Self {
        Self {
            filename,
            success: false,
            message: error,
            file_size: 0,
            processed_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            server_id: None,
            deck_identifier: None,
            resolved_deck_id: None,
        }
    }
}

/// Summary of a scan operation
#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    pub total_files_found: usize,
    pub new_files: usize,
    pub already_processed: usize,
    pub successfully_uploaded: usize,
    pub failed_uploads: usize,
    pub results: Vec<GameLogProcessResult>,
    /// True when the scan was skipped because no MaMo auth token is configured.
    /// This is a "not connected yet" state, NOT an upload failure.
    pub auth_missing: bool,
}

/// State for the game log watcher
#[derive(Debug, Clone, Default)]
pub struct GameLogWatcherState {
    /// Current status of the watcher
    pub status: Option<WatcherStatus>,
    /// Set of already processed file names (to avoid duplicates)
    pub processed_files: HashSet<String>,
    /// Last scan timestamp
    pub last_scan: Option<String>,
    /// Results from the last scan
    pub last_scan_results: Vec<GameLogProcessResult>,
    /// Total files processed since startup
    pub total_processed: usize,
    /// Is background scanning enabled
    pub background_enabled: bool,
    /// Scan interval in seconds
    pub scan_interval_secs: u64,
}

/// Configuration for the game log reader
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameLogConfig {
    /// Directory to watch for game log files
    pub watch_directory: String,
    /// File extensions to look for
    #[serde(default = "default_extensions")]
    pub file_extensions: Vec<String>,
    /// Whether to enable background scanning
    #[serde(default)]
    pub background_scan_enabled: bool,
    /// Scan interval in seconds (default: 30)
    #[serde(default = "default_scan_interval")]
    pub scan_interval_secs: u64,
    /// Backend API URL for uploading
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// User ID for associating uploads
    #[serde(default)]
    pub user_id: Option<String>,
    /// Authentication token for API requests (JWT)
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_extensions() -> Vec<String> {
    vec!["json".to_string()]
}

fn default_scan_interval() -> u64 {
    30 // 30 seconds
}

fn default_api_url() -> String {
    "https://new-backend-two-eosin.vercel.app".to_string()
}

impl Default for GameLogConfig {
    fn default() -> Self {
        Self {
            watch_directory: get_default_forge_log_directory(),
            file_extensions: default_extensions(),
            background_scan_enabled: false,
            scan_interval_secs: default_scan_interval(),
            api_url: default_api_url(),
            user_id: None,
            auth_token: None,
        }
    }
}

/// Get the default Forge game log directory based on OS
pub fn get_default_forge_log_directory() -> String {
    if cfg!(windows) {
        // Windows: %APPDATA%\Forge\games\gamelogs (Roaming AppData)
        // Use std::env to get APPDATA directly for Windows
        if let Ok(appdata) = std::env::var("APPDATA") {
            return std::path::Path::new(&appdata)
                .join("Forge")
                .join("games")
                .join("gamelogs")
                .to_string_lossy()
                .to_string();
        }
        // Fallback to dirs crate
        if let Some(config) = dirs::config_dir() {
            return config.join("Forge").join("games").join("gamelogs").to_string_lossy().to_string();
        }
    } else if cfg!(target_os = "macos") {
        // macOS: ~/Library/Application Support/Forge/games/gamelogs
        if let Some(home) = dirs::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("Forge")
                .join("games")
                .join("gamelogs")
                .to_string_lossy()
                .to_string();
        }
    } else {
        // Linux: ~/.forge/games/gamelogs
        if let Some(home) = dirs::home_dir() {
            return home.join(".forge").join("games").join("gamelogs").to_string_lossy().to_string();
        }
    }
    
    // Fallback
    "".to_string()
}

/// Scan directory for game log files
pub fn scan_directory(config: &GameLogConfig) -> Result<Vec<PathBuf>> {
    let dir = Path::new(&config.watch_directory);
    
    if !dir.exists() {
        return Err(anyhow::anyhow!("Directory does not exist: {}", config.watch_directory));
    }
    
    if !dir.is_dir() {
        return Err(anyhow::anyhow!("Path is not a directory: {}", config.watch_directory));
    }
    
    let mut files = Vec::new();
    
    for entry in fs::read_dir(dir).context("Failed to read directory")? {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();
        
        if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if config.file_extensions.iter().any(|e| e.to_lowercase() == ext_str) {
                    files.push(path);
                }
            }
        }
    }
    
    // Sort by modification time (newest first)
    files.sort_by(|a, b| {
        let time_a = a.metadata().ok().and_then(|m| m.modified().ok());
        let time_b = b.metadata().ok().and_then(|m| m.modified().ok());
        time_b.cmp(&time_a)
    });
    
    Ok(files)
}

/// Preview info for a file before upload
#[derive(Debug, Clone, Default)]
pub struct FilePreviewInfo {
    pub filename: String,
    pub file_size: u64,
    pub detected_deck: Option<String>,
    pub modified_date: Option<String>,
}

/// Preview scan - shows what files would be uploaded with detected deck names
pub fn preview_scan(
    config: &GameLogConfig,
    processed_files: &HashSet<String>,
    filter: &GameLogFilterOptions,
) -> Result<Vec<FilePreviewInfo>> {
    let files = scan_directory(config)?;
    let mut preview_results = Vec::new();
    
    for file_path in files {
        let filename = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        
        // Skip already processed
        if processed_files.contains(&filename) {
            continue;
        }
        
        // Check days filter
        let metadata = file_path.metadata().ok();
        let modified_time = metadata.as_ref().and_then(|m| m.modified().ok());
        if !filter.passes_days_filter(modified_time) {
            continue;
        }
        
        // Read file to extract deck identifier
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        
        let detected_deck = extract_deck_identifier(&filename, &content);
        
        // Check deck filter
        if !filter.passes_deck_filter(detected_deck.as_deref()) {
            continue;
        }
        
        // Format modified date
        let modified_date = modified_time
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| {
                let secs = d.as_secs() as i64;
                chrono::DateTime::from_timestamp(secs, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default()
            });
        
        let file_size = metadata.map(|m| m.len()).unwrap_or(0);
        
        preview_results.push(FilePreviewInfo {
            filename,
            file_size,
            detected_deck,
            modified_date,
        });
    }
    
    Ok(preview_results)
}

/// Read and parse a game log file (supports plain text/JSON, raw gzip, and base64-encoded gzip)
pub fn read_game_log(path: &Path) -> Result<GameLogContent> {
    // Attempt read, with a brief retry for files temporarily locked by Forge/simulation
    let mut bytes_result = fs::read(path);
    if bytes_result.is_err() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        bytes_result = fs::read(path);
    }
    let bytes = bytes_result.with_context(|| format!("Failed to read file: {:?}", path))?;

    if bytes.is_empty() {
        anyhow::bail!("File {:?} is empty (0 bytes)", path);
    }

    // Check if raw gzip (magic bytes 0x1F, 0x8B)
    let content = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(bytes.as_slice());
        let mut decompressed = String::new();
        match decoder.read_to_string(&mut decompressed) {
            Ok(_) => decompressed,
            Err(_) => String::from_utf8_lossy(&bytes).to_string(),
        }
    } else {
        let text = String::from_utf8_lossy(&bytes);
        if text.trim_start().starts_with("H4sI") {
            let trimmed = text.trim();
            match BASE64_STANDARD.decode(trimmed) {
                Ok(compressed) => {
                    let mut decoder = GzDecoder::new(compressed.as_slice());
                    let mut decompressed = String::new();
                    match decoder.read_to_string(&mut decompressed) {
                        Ok(_) => decompressed,
                        Err(_) => text.to_string(),
                    }
                }
                Err(_) => text.to_string(),
            }
        } else {
            text.to_string()
        }
    };

    let metadata = fs::metadata(path)
        .with_context(|| format!("Failed to get file metadata: {:?}", path))?;

    let filename = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let modified = metadata.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    Ok(GameLogContent {
        filename,
        content,
        file_size: metadata.len(),
        modified_timestamp: modified,
    })
}

/// Content of a game log file
#[derive(Debug, Clone, Serialize)]
pub struct GameLogContent {
    pub filename: String,
    pub content: String,
    pub file_size: u64,
    pub modified_timestamp: Option<u64>,
}

/// Upload a game log to the backend API
pub async fn upload_game_log(
    config: &GameLogConfig,
    log_content: &GameLogContent,
) -> Result<UploadResponse> {
    let client = reqwest::Client::new();
    
    // Check if we have an auth token
    let auth_token = config.auth_token.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No authentication token configured. Please add your MaMo token in Settings."))?;
    
    // Extract deck identifier and deck link from filename or content
    let deck_identifier = extract_deck_identifier(&log_content.filename, &log_content.content);
    let deck_link = extract_deck_link(&log_content.content);

    // Resolve the authoritative MaMo deck id (and, if known, the revision it was played
    // at) from the local deck mappings (log deck name -> MaMo deck id / revision id).
    // This lets the backend associate the log directly instead of relying on fuzzy name
    // matching (see backend F-014), and attribute it to the revision actually played
    // instead of whatever is "latest" at upload time.
    // Best-effort: missing/unreadable mappings simply fall back to name matching.
    let loaded_mappings = match DeckMappings::load() {
        Ok(m) => Some(m),
        Err(e) => {
            log::warn!("Could not load deck mappings for association: {}", e);
            None
        }
    };
    let deck_id = deck_identifier.as_deref().and_then(|name| {
        loaded_mappings.as_ref().and_then(|m| m.get_mapping(name).cloned())
    });
    let revision_id = deck_identifier.as_deref().and_then(|name| {
        loaded_mappings.as_ref().and_then(|m| m.get_revision_mapping(name).cloned())
    });
    if let Some(ref id) = deck_id {
        log::info!("Resolved deck_id {} for log deck name {:?}", id, deck_identifier);
    }
    if let Some(ref rev) = revision_id {
        log::info!("Resolved revision_id {} for log deck name {:?}", rev, deck_identifier);
    }

    // Calculate SHA256 checksum of the original content (before compression)
    let mut hasher = Sha256::new();
    hasher.update(log_content.content.as_bytes());
    let checksum = format!("{:x}", hasher.finalize());

    // Gzip-compress the content to stay within Vercel's 4.5 MB serverless body limit.
    // Large replay files (8+ MB) would be rejected before Express sees them.
    let compressed_content = {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(log_content.content.as_bytes())
            .context("Failed to gzip-compress game log content")?;
        let compressed = encoder.finish().context("Failed to finalise gzip stream")?;
        BASE64_STANDARD.encode(&compressed)
    };

    let upload_payload = GameLogUploadPayload {
        filename: log_content.filename.clone(),
        content: compressed_content,
        content_encoding: Some("gzip".to_string()),
        file_size: log_content.file_size,
        modified_timestamp: log_content.modified_timestamp,
        user_id: config.user_id.clone(),
        uploaded_at: chrono::Utc::now().to_rfc3339(),
        checksum,
        deck_identifier,
        deck_id,
        revision_id,
        deck_link,
    };
    
    let url = format!("{}/api/gamelog/upload", config.api_url);
    
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&upload_payload)
        .send()
        .await
        .context("Failed to send upload request")?;
    
    if response.status().is_success() {
        let result: UploadResponse = response
            .json()
            .await
            .context("Failed to parse upload response")?;
        Ok(result)
    } else {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        Err(anyhow::anyhow!("Upload failed with status {}: {}", status, error_text))
    }
}

/// Extract deck link/URL from JSON content
///
/// Looks for deck link in:
/// - `meta.players.P1.deck_link` or `meta.players.P1.deck_url` (MTG Replay Notation)
/// - `meta.deck_link` or `meta.deck_url`
/// - Top-level `deck_link` or `deck_url`
fn extract_deck_link(content: &str) -> Option<String> {
    let json_value: serde_json::Value = serde_json::from_str(content).ok()?;
    let link_fields = ["deck_link", "deckLink", "deck_url", "deckUrl"];
    
    // Check meta.players.P1
    if let Some(meta) = json_value.get("meta") {
        if let Some(players) = meta.get("players").and_then(|v| v.as_object()) {
            for key in &["P1", "P2"] {
                if let Some(player) = players.get(*key) {
                    for field in &link_fields {
                        if let Some(link) = player.get(field).and_then(|v| v.as_str()) {
                            if !link.is_empty() {
                                return Some(link.to_string());
                            }
                        }
                    }
                }
            }
        }
        // meta-level
        for field in &link_fields {
            if let Some(link) = meta.get(field).and_then(|v| v.as_str()) {
                if !link.is_empty() {
                    return Some(link.to_string());
                }
            }
        }
    }
    
    // Top-level
    for field in &link_fields {
        if let Some(link) = json_value.get(field).and_then(|v| v.as_str()) {
            if !link.is_empty() {
                return Some(link.to_string());
            }
        }
    }
    
    None
}

/// Extract deck identifier from filename or JSON content
/// 
/// Parses JSON game logs following the MTG Replay & Learning Notation format.
/// Looks for deck information in:
/// - `meta.players.P1.deck_hash` or `meta.players.P1.name` (MTG Replay Notation)
/// - `deckName`, `deck_name`, `deck.name` fields
/// - `players[].deck` or `players[].deckName` arrays
/// - Filename patterns (e.g., "game-MyDeckName-2026-02-02.json")
fn extract_deck_identifier(filename: &str, content: &str) -> Option<String> {
    // Pattern 1: Parse JSON content (primary method)
    if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(deck_name) = extract_deck_from_json(&json_value) {
            return Some(deck_name);
        }
    }
    
    // Pattern 2: Fall back to filename extraction
    if let Some(deck_name) = extract_deck_from_filename(filename) {
        return Some(deck_name);
    }
    
    None
}

/// Extract deck name from JSON game log content
///
/// Supports the MTG Replay & Learning Notation format:
/// ```json
/// { "meta": { "players": { "P1": { "name": "Alice", "deck_hash": "abc123" } } } }
/// ```
/// Also handles other common JSON game log structures.
fn extract_deck_from_json(value: &serde_json::Value) -> Option<String> {
    // MTG Replay Notation: meta.players.P1 (player map with deck_hash/name)
    if let Some(meta) = value.get("meta") {
        if let Some(players) = meta.get("players").and_then(|v| v.as_object()) {
            // Find P1 (human player) first, then fall back to any player
            let player_keys = ["P1", "P2"];
            for key in &player_keys {
                if let Some(player) = players.get(*key) {
                    // Prefer human-readable deck_name over the internal deck_hash,
                    // because deck_hash cannot be matched against MaMo deck names.
                    for field in &["deck_name", "deckName"] {
                        if let Some(name) = player.get(field).and_then(|v| v.as_str()) {
                            if !name.is_empty() {
                                return Some(name.to_string());
                            }
                        }
                    }
                    // Fall back to deck_hash only if no human-readable name is available
                    if let Some(deck_hash) = player.get("deck_hash").and_then(|v| v.as_str()) {
                        if !deck_hash.is_empty() {
                            return Some(deck_hash.to_string());
                        }
                    }
                    // Also try generic deck field
                    if let Some(name) = player.get("deck").and_then(|v| v.as_str()) {
                        if !name.is_empty() {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
        // Also check meta-level deck fields
        for field in &["deck_name", "deckName", "deck"] {
            if let Some(name) = meta.get(field).and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    
    // Direct top-level fields
    let field_names = ["deckName", "deck_name", "deck", "playerDeck", "player_deck"];
    for field in &field_names {
        if let Some(name) = value.get(field).and_then(|v| v.as_str()) {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        // Nested object: { "deck": { "name": "..." } }
        if let Some(obj) = value.get(field).and_then(|v| v.as_object()) {
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    
    // Players array: { "players": [{ "deck": "...", "deckName": "..." }] }
    if let Some(players) = value.get("players").and_then(|v| v.as_array()) {
        for player in players {
            let is_human = player.get("isHuman").and_then(|v| v.as_bool()).unwrap_or(false)
                || player.get("type").and_then(|v| v.as_str()) == Some("human");
            
            for field in &field_names {
                if let Some(name) = player.get(field).and_then(|v| v.as_str()) {
                    if !name.is_empty() && is_human {
                        return Some(name.to_string());
                    }
                }
            }
        }
        // Fall back to first player's deck
        if let Some(first_player) = players.first() {
            for field in &field_names {
                if let Some(name) = first_player.get(field).and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    
    // Nested game object
    if let Some(game) = value.get("game").or_else(|| value.get("gameData")) {
        return extract_deck_from_json(game);
    }
    
    None
}

/// Extract deck name from filename
fn extract_deck_from_filename(filename: &str) -> Option<String> {
    // Remove .json extension
    let name = filename.trim_end_matches(".json");
    
    // Pattern: "game-{deck_name}-{date}"
    if name.starts_with("game-") {
        let parts: Vec<&str> = name.strip_prefix("game-").unwrap_or(name).split('-').collect();
        if parts.len() >= 2 {
            // Join all parts except the last few (which are likely date components)
            // e.g., "game-My-Deck-Name-2026-02-02" -> "My-Deck-Name"
            let non_date_parts: Vec<&str> = parts.iter()
                .take_while(|p| !p.chars().all(|c| c.is_ascii_digit()))
                .cloned()
                .collect();
            if !non_date_parts.is_empty() {
                return Some(non_date_parts.join("-"));
            }
        }
    }
    
    // Pattern: "{deck_name}_vs_{opponent}"
    if name.contains("_vs_") || name.contains(" vs ") {
        let deck_name = name.split("_vs_").next()
            .or_else(|| name.split(" vs ").next())
            .map(|s| s.trim().to_string());
        if let Some(ref dn) = deck_name {
            if !dn.is_empty() && dn.len() > 2 {
                return deck_name;
            }
        }
    }
    
    None
}

/// Payload for uploading a game log
#[derive(Debug, Serialize)]
pub struct GameLogUploadPayload {
    pub filename: String,
    /// File content, optionally compressed. When `content_encoding` is `"gzip"`,
    /// this is the base64-encoded gzip of the original UTF-8 content.
    pub content: String,
    /// Encoding applied to `content`. `None` / absent → raw UTF-8. `"gzip"` → base64(gzip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    pub file_size: u64,
    pub modified_timestamp: Option<u64>,
    pub user_id: Option<String>,
    pub uploaded_at: String,
    /// SHA256 checksum of the original (pre-compression) content
    pub checksum: String,
    /// Deck identifier extracted from filename or content
    pub deck_identifier: Option<String>,
    /// Authoritative MaMo deck id, resolved from the local deck mappings
    /// (log deck name -> MaMo deck id). When present the backend associates the
    /// log to this deck directly instead of relying on fuzzy name matching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deck_id: Option<String>,
    /// Deck revision id this log was played against, resolved from the local deck mappings
    /// (recorded when the deck was last exported/launched). When present and it belongs to
    /// the associated deck, the backend uses it instead of "latest revision at upload time".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    /// Deck link/URL extracted from content (e.g. MaMo deck page URL)
    pub deck_link: Option<String>,
}

/// Response from upload API
#[derive(Debug, Clone, Deserialize)]
pub struct UploadResponse {
    pub success: bool,
    pub message: String,
    pub id: Option<String>,
}

/// Response from replay-content API
#[derive(Debug, Clone, Deserialize)]
pub struct ReplayContentResponse {
    pub success: bool,
    pub content: Option<String>,
    pub filename: Option<String>,
    pub error: Option<String>,
}

/// Trigger re-parsing of all parse_failed game logs on the backend.
///
/// Calls `POST /api/gamelog/reparse-failed`. The backend re-runs the parser
/// on stored raw_content for every failed record owned by this user. Returns
/// how many were successfully re-parsed.
pub async fn reparse_failed_logs(
    api_url: &str,
    auth_token: &str,
) -> Result<(u32, u32, u32)> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/gamelog/reparse-failed", api_url);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .header("Content-Length", "0")
        .send()
        .await
        .context("Failed to connect to backend for re-parse")?;

    if response.status().is_success() {
        let body: serde_json::Value = response.json().await.context("Failed to parse reparse response")?;
        let reparsed = body["reparsed"].as_u64().unwrap_or(0) as u32;
        let still_failed = body["stillFailed"].as_u64().unwrap_or(0) as u32;
        let total = body["total"].as_u64().unwrap_or(0) as u32;
        Ok((reparsed, still_failed, total))
    } else {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        Err(anyhow::anyhow!("Re-parse failed with status {}: {}", status, error_text))
    }
}

/// Download replay content from the backend for a specific game log
///
/// Calls `GET /api/gamelog/{id}/replay-content` with PAT authentication.
/// Returns (content, filename) on success.
pub async fn download_replay_content(
    api_url: &str,
    gamelog_id: &str,
    auth_token: &str,
) -> Result<(String, String)> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/gamelog/{}/replay-content", api_url, gamelog_id);

    log::info!("Downloading replay content from: {}", url);

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .send()
        .await
        .context("Failed to connect to backend for replay download")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        if status.as_u16() == 401 {
            return Err(anyhow::anyhow!("Authentication failed. Please re-authenticate the Connector from MaMo."));
        } else if status.as_u16() == 404 {
            return Err(anyhow::anyhow!("Game log not found. It may have been deleted."));
        } else if status.as_u16() == 400 {
            return Err(anyhow::anyhow!("Game log has not been parsed. Only parsed logs can be replayed."));
        }
        return Err(anyhow::anyhow!("Failed to download replay: {} - {}", status, error_text));
    }

    let data: ReplayContentResponse = response.json().await
        .context("Failed to parse replay content response")?;

    if !data.success {
        return Err(anyhow::anyhow!("Server returned error: {}", data.error.unwrap_or_default()));
    }

    let content = data.content
        .ok_or_else(|| anyhow::anyhow!("Response missing replay content"))?;
    let filename = data.filename
        .unwrap_or_else(|| format!("replay_{}.json", gamelog_id));

    Ok((content, filename))
}

/// Save replay content to the Forge gamelogs directory
///
/// Returns the full path to the saved file.
pub fn save_replay_to_forge_dir(filename: &str, content: &str) -> Result<PathBuf> {
    let gamelogs_dir = get_default_forge_log_directory();
    if gamelogs_dir.is_empty() {
        return Err(anyhow::anyhow!("Could not determine Forge gamelogs directory"));
    }

    let dir = Path::new(&gamelogs_dir);
    if !dir.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create Forge gamelogs directory: {}", gamelogs_dir))?;
    }

    let file_path = dir.join(filename);
    fs::write(&file_path, content)
        .with_context(|| format!("Failed to write replay file: {:?}", file_path))?;

    log::info!("Saved replay file to: {:?}", file_path);
    Ok(file_path)
}

/// Filter options for processing game logs
#[derive(Debug, Clone, Default)]
pub struct GameLogFilterOptions {
    /// Only process logs from the last N days (0 = no filter)
    pub days_filter: u32,
    /// Only process logs matching these deck names (empty = all decks)
    pub deck_filter: HashSet<String>,
}

impl GameLogFilterOptions {
    /// Check if a file passes the days filter based on its modification time
    pub fn passes_days_filter(&self, modified: Option<std::time::SystemTime>) -> bool {
        if self.days_filter == 0 {
            return true;
        }
        
        if let Some(mod_time) = modified {
            let now = std::time::SystemTime::now();
            let days_ago = std::time::Duration::from_secs(self.days_filter as u64 * 24 * 60 * 60);
            if let Some(cutoff) = now.checked_sub(days_ago) {
                return mod_time >= cutoff;
            }
        }
        true // If we can't determine modification time, include it
    }
    
    /// Check if a deck name passes the deck filter
    pub fn passes_deck_filter(&self, deck_name: Option<&str>) -> bool {
        if self.deck_filter.is_empty() {
            return true;
        }
        
        if let Some(name) = deck_name {
            // Check for exact match or partial match (case-insensitive)
            let name_lower = name.to_lowercase();
            self.deck_filter.iter().any(|f| {
                let filter_lower = f.to_lowercase();
                name_lower.contains(&filter_lower) || filter_lower.contains(&name_lower)
            })
        } else {
            false // No deck name detected, doesn't pass deck filter
        }
    }
}

/// Process new game logs from the configured directory
pub async fn process_new_logs(
    config: &GameLogConfig,
    processed_files: &Arc<Mutex<HashSet<String>>>,
) -> Result<ScanSummary> {
    process_new_logs_with_filter(config, processed_files, &GameLogFilterOptions::default()).await
}

/// Process new game logs from the configured directory with filter options
pub async fn process_new_logs_with_filter(
    config: &GameLogConfig,
    processed_files: &Arc<Mutex<HashSet<String>>>,
    filter: &GameLogFilterOptions,
) -> Result<ScanSummary> {
    let mut summary = ScanSummary::default();
    
    // Pre-flight: if no auth token, this is a "not connected" state — not a failure.
    // Report how many new files are waiting (so the user knows uploads are pending)
    // but do NOT fabricate a failed upload or scan/parse anything.
    if config.auth_token.is_none() {
        summary.auth_missing = true;
        // Count new (unprocessed) files so the UI can say "N waiting to upload"
        if let Ok(files) = scan_directory(config) {
            let processed = processed_files.lock().unwrap();
            summary.total_files_found = files.len();
            summary.new_files = files
                .iter()
                .filter(|f| {
                    f.file_name()
                        .map(|n| !processed.contains(&n.to_string_lossy().to_string()))
                        .unwrap_or(false)
                })
                .count();
        }
        return Ok(summary);
    }
    
    // Scan directory for files
    let files = scan_directory(config)?;
    summary.total_files_found = files.len();
    
    let mut skipped_by_filter = 0usize;
    
    for file_path in files {
        let filename = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        
        // Check if already processed
        {
            let processed = processed_files.lock().unwrap();
            if processed.contains(&filename) {
                summary.already_processed += 1;
                continue;
            }
        }
        
        // Check days filter based on file modification time
        let modified_time = file_path.metadata().ok().and_then(|m| m.modified().ok());
        if !filter.passes_days_filter(modified_time) {
            skipped_by_filter += 1;
            continue;
        }
        
        // Read the file to extract deck identifier for deck filter
        let log_content = match read_game_log(&file_path) {
            Ok(content) => content,
            Err(e) => {
                summary.failed_uploads += 1;
                summary.results.push(GameLogProcessResult::failed(
                    filename.clone(),
                    format!("{e}"),
                ));
                continue;
            }
        };
        
        // Extract deck identifier before checking filter
        let deck_identifier = extract_deck_identifier(&log_content.filename, &log_content.content);
        
        // Check deck filter
        if !filter.passes_deck_filter(deck_identifier.as_deref()) {
            skipped_by_filter += 1;
            continue;
        }
        
        summary.new_files += 1;
        
        // Upload to backend
        match upload_game_log(config, &log_content).await {
            Ok(response) => {
                if response.success {
                    summary.successfully_uploaded += 1;
                    // Same lookup upload_game_log already did internally to attach deck_id to
                    // the upload payload — re-resolved here (rather than threading it back out
                    // through UploadResponse) so the UI can link straight to this deck's
                    // analysis instead of just reporting a filename.
                    let resolved_deck_id = deck_identifier.as_deref().and_then(|name| {
                        DeckMappings::load().ok().and_then(|m| m.get_mapping(name).cloned())
                    });
                    summary.results.push(GameLogProcessResult::success(
                        filename.clone(),
                        log_content.file_size,
                        response.id,
                        deck_identifier,
                        resolved_deck_id,
                    ));
                    
                    // Mark as processed
                    let mut processed = processed_files.lock().unwrap();
                    processed.insert(filename);
                } else {
                    summary.failed_uploads += 1;
                    summary.results.push(GameLogProcessResult::failed(
                        filename,
                        response.message,
                    ));
                }
            }
            Err(e) => {
                summary.failed_uploads += 1;
                summary.results.push(GameLogProcessResult::failed(
                    filename,
                    format!("Upload failed: {}", e),
                ));
            }
        }
    }
    
    Ok(summary)
}

/// Validate that a directory path exists and is accessible
pub fn validate_directory(path: &str) -> Result<bool> {
    let dir = Path::new(path);
    
    if path.is_empty() {
        return Ok(false);
    }
    
    if !dir.exists() {
        return Ok(false);
    }
    
    if !dir.is_dir() {
        return Ok(false);
    }
    
    // Try to read the directory to check permissions
    match fs::read_dir(dir) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Get file count in a directory matching extensions
pub fn get_file_count(config: &GameLogConfig) -> Result<usize> {
    let files = scan_directory(config)?;
    Ok(files.len())
}

/// Load processed files list from persistent storage
pub fn load_processed_files() -> Result<HashSet<String>> {
    let path = get_processed_files_path()?;
    
    if path.exists() {
        let content = fs::read_to_string(&path)
            .context("Failed to read processed files list")?;
        let files: HashSet<String> = serde_json::from_str(&content)
            .context("Failed to parse processed files list")?;
        Ok(files)
    } else {
        Ok(HashSet::new())
    }
}

/// Save processed files list to persistent storage
pub fn save_processed_files(files: &HashSet<String>) -> Result<()> {
    let path = get_processed_files_path()?;
    
    // Ensure directory exists
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    
    let content = serde_json::to_string_pretty(files)?;
    fs::write(&path, content)?;
    
    Ok(())
}

fn get_processed_files_path() -> Result<PathBuf> {
    let config_dir = if cfg!(windows) {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("MamoConnector")
    } else if cfg!(target_os = "macos") {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join("Library")
            .join("Application Support")
            .join("MamoConnector")
    } else {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("mamo-connector")
    };
    
    Ok(config_dir.join("processed_gamelogs.json"))
}

// ==================== DECK MAPPING ====================

/// A deck from the MaMo backend
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDeck {
    pub deck_id: String,
    pub deck_name: String,
    pub user_id: String,
    pub color_identity: Option<Vec<String>>,  // Array of color letters like ["B", "G", "U"]
    pub commander_id: Option<String>,
    pub commander_partner_id: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
}

/// Response from the mydecks endpoint
#[derive(Debug, Deserialize)]
pub struct MyDecksResponse {
    pub decks: Vec<UserDeck>,
    pub total: usize,
}

/// Mapping from deck name (as appears in game logs) to MaMo deck ID
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeckMappings {
    /// Maps deck name from gamelog -> MaMo deck ID
    pub mappings: std::collections::HashMap<String, String>,
    /// Maps deck name from gamelog -> the MaMo deck revision id that was last exported/
    /// launched for it. Recorded at deck-download time (see `deck::create_deck_from_mamo_with_progress`)
    /// so a gamelog produced from that launch can be attributed to the revision actually
    /// played, not just whatever revision is "latest" when the log is later uploaded.
    #[serde(default)]
    pub revisions: std::collections::HashMap<String, String>,
    /// When mappings were last updated
    pub updated_at: Option<String>,
}

impl DeckMappings {
    /// Load mappings from disk
    pub fn load() -> Result<Self> {
        let path = get_deck_mappings_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)
            .context("Failed to read deck mappings file")?;
        let mappings: DeckMappings = serde_json::from_str(&content)
            .context("Failed to parse deck mappings")?;
        Ok(mappings)
    }

    /// Save mappings to disk
    pub fn save(&self) -> Result<()> {
        let path = get_deck_mappings_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }

    /// Add or update a mapping
    pub fn set_mapping(&mut self, log_deck_name: &str, mamo_deck_id: &str) {
        self.mappings.insert(log_deck_name.to_string(), mamo_deck_id.to_string());
        self.updated_at = Some(chrono::Local::now().to_rfc3339());
    }

    /// Get the MaMo deck ID for a log deck name
    pub fn get_mapping(&self, log_deck_name: &str) -> Option<&String> {
        self.mappings.get(log_deck_name)
    }

    /// Remove a mapping
    pub fn remove_mapping(&mut self, log_deck_name: &str) {
        self.mappings.remove(log_deck_name);
        self.updated_at = Some(chrono::Local::now().to_rfc3339());
    }

    /// Record which deck revision was last exported/launched for a deck name
    pub fn set_revision_mapping(&mut self, log_deck_name: &str, revision_id: &str) {
        self.revisions.insert(log_deck_name.to_string(), revision_id.to_string());
        self.updated_at = Some(chrono::Local::now().to_rfc3339());
    }

    /// Get the last-known revision id exported/launched for a log deck name
    pub fn get_revision_mapping(&self, log_deck_name: &str) -> Option<&String> {
        self.revisions.get(log_deck_name)
    }
}

/// Get path to deck mappings file
fn get_deck_mappings_path() -> Result<PathBuf> {
    let config_dir = if cfg!(windows) {
        dirs::data_dir()
            .context("Could not find data directory")?
            .join("MaMoConnector")
    } else {
        dirs::config_dir()
            .context("Could not find config directory")?
            .join("mamo-connector")
    };
    
    Ok(config_dir.join("deck_mappings.json"))
}

/// Cached user decks with timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedUserDecks {
    pub decks: Vec<UserDeck>,
    pub cached_at: String,
}

impl CachedUserDecks {
    /// Create a new cache with current timestamp
    pub fn new(decks: Vec<UserDeck>) -> Self {
        Self {
            decks,
            cached_at: chrono::Local::now().to_rfc3339(),
        }
    }
}

/// Get path to cached decks file
fn get_cached_decks_path() -> Result<PathBuf> {
    let config_dir = if cfg!(windows) {
        dirs::data_dir()
            .context("Could not find data directory")?
            .join("MaMoConnector")
    } else {
        dirs::config_dir()
            .context("Could not find config directory")?
            .join("mamo-connector")
    };
    
    Ok(config_dir.join("cached_decks.json"))
}

/// Load cached user decks from disk
pub fn load_cached_decks() -> Result<CachedUserDecks> {
    let path = get_cached_decks_path()?;
    if !path.exists() {
        return Err(anyhow::anyhow!("No cached decks found"));
    }
    let content = fs::read_to_string(&path)
        .context("Failed to read cached decks file")?;
    let cached: CachedUserDecks = serde_json::from_str(&content)
        .context("Failed to parse cached decks")?;
    Ok(cached)
}

/// Save user decks to disk cache
pub fn save_cached_decks(decks: &[UserDeck]) -> Result<()> {
    let path = get_cached_decks_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cached = CachedUserDecks::new(decks.to_vec());
    let content = serde_json::to_string_pretty(&cached)?;
    fs::write(&path, content)?;
    log::info!("Saved {} decks to cache", decks.len());
    Ok(())
}

/// Fetch user's decks from the backend using PAT authentication
pub async fn fetch_my_decks(config: &GameLogConfig) -> Result<Vec<UserDeck>> {
    let auth_token = config.auth_token.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No authentication token configured"))?;

    let client = reqwest::Client::new();
    let url = format!("{}/api/gamelog/mydecks", config.api_url);

    log::info!("Fetching user decks from: {}", url);

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .send()
        .await
        .context("Failed to fetch decks")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        return Err(anyhow::anyhow!("Failed to fetch decks: {} - {}", status, error_text));
    }

    let data: MyDecksResponse = response.json().await
        .context("Failed to parse decks response")?;

    log::info!("Fetched {} decks from backend", data.total);
    Ok(data.decks)
}

/// A lightweight scenario summary from `GET /api/scenarios?deckId=` — enough to list a deck's
/// saved scenarios and decide which are playable in Forge, without needing the full
/// `ScenarioDefinition` payload's synergies/combos/events.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSummary {
    pub id: String,
    pub name: String,
    pub mode: String,
    #[serde(default)]
    pub cards: Vec<ScenarioCardZoneOnly>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ScenarioCardZoneOnly {
    zone: String,
}

impl ScenarioSummary {
    /// Mirrors the same two gates the web app applies before showing its own
    /// "▶ Play in Forge (scenario)" link: only starting-hand/perfect-game scenarios are
    /// exportable to Forge (the backend's forge-scenario export rejects other modes), and
    /// only once at least one card has actually been placed in the opening hand.
    pub fn playable_in_forge(&self) -> bool {
        (self.mode == "starting-hand" || self.mode == "perfect-game")
            && self.cards.iter().any(|c| c.zone == "hand")
    }
}

/// Fetch a deck's saved scenarios from the backend using PAT authentication — reuses the same
/// `gamelog:upload`-scoped token `fetch_my_decks` already sends, since the backend's
/// `/api/scenarios` route accepts either a JWT or that scope.
pub async fn fetch_deck_scenarios(config: &GameLogConfig, deck_id: &str) -> Result<Vec<ScenarioSummary>> {
    let auth_token = config.auth_token.as_ref()
        .ok_or_else(|| anyhow::anyhow!("No authentication token configured"))?;

    let client = reqwest::Client::new();
    let url = format!("{}/api/scenarios?deckId={}", config.api_url, deck_id);

    log::info!("Fetching deck scenarios from: {}", url);

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", auth_token))
        .send()
        .await
        .context("Failed to fetch scenarios")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
        return Err(anyhow::anyhow!("Failed to fetch scenarios: {} - {}", status, error_text));
    }

    let scenarios: Vec<ScenarioSummary> = response.json().await
        .context("Failed to parse scenarios response")?;

    log::info!("Fetched {} scenarios for deck {}", scenarios.len(), deck_id);
    Ok(scenarios)
}

/// Calculate similarity score between two deck names (0.0 - 1.0)
pub fn deck_name_similarity(name1: &str, name2: &str) -> f64 {
    let n1 = name1.to_lowercase();
    let n2 = name2.to_lowercase();
    
    // Exact match
    if n1 == n2 {
        return 1.0;
    }
    
    // Contains match
    if n1.contains(&n2) || n2.contains(&n1) {
        return 0.8;
    }
    
    // Word overlap
    let words1: HashSet<&str> = n1.split_whitespace().collect();
    let words2: HashSet<&str> = n2.split_whitespace().collect();
    
    if words1.is_empty() || words2.is_empty() {
        return 0.0;
    }
    
    let intersection = words1.intersection(&words2).count();
    let union = words1.union(&words2).count();
    
    if union == 0 {
        return 0.0;
    }
    
    // Jaccard similarity
    intersection as f64 / union as f64
}

/// A suggested deck match with confidence score
#[derive(Debug, Clone)]
pub struct DeckSuggestion {
    pub deck: UserDeck,
    pub score: f64,
}

/// Find matching deck suggestions for a deck name from a gamelog
pub fn suggest_deck_matches(log_deck_name: &str, user_decks: &[UserDeck], limit: usize) -> Vec<DeckSuggestion> {
    let mut suggestions: Vec<DeckSuggestion> = user_decks.iter()
        .map(|deck| {
            let score = deck_name_similarity(log_deck_name, &deck.deck_name);
            
            // Note: We only have commander IDs, not names, so we can only match on deck name
            // In the future, we could look up commander names from a local card database
            
            DeckSuggestion {
                deck: deck.clone(),
                score,
            }
        })
        .filter(|s| s.score > 0.1) // Only include meaningful matches
        .collect();
    
    // Sort by score descending
    suggestions.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    
    // Return top N
    suggestions.into_iter().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GameLogConfig::default();
        assert!(!config.watch_directory.is_empty() || cfg!(target_os = "linux"));
        assert_eq!(config.file_extensions, vec!["json"]);
        assert!(!config.background_scan_enabled);
        assert_eq!(config.scan_interval_secs, 30);
    }

    #[test]
    fn test_validate_directory_empty() {
        let result = validate_directory("").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_validate_directory_nonexistent() {
        let result = validate_directory("/nonexistent/path/xyz123").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_game_log_process_result() {
        let success = GameLogProcessResult::success(
            "test.json".to_string(),
            1024,
            Some("id123".to_string()),
            Some("MyDeck".to_string()),
            Some("deck-uuid-456".to_string()),
        );
        assert!(success.success);
        assert_eq!(success.filename, "test.json");
        assert_eq!(success.file_size, 1024);
        assert_eq!(success.server_id, Some("id123".to_string()));
        assert_eq!(success.deck_identifier, Some("MyDeck".to_string()));
        assert_eq!(success.resolved_deck_id, Some("deck-uuid-456".to_string()));

        let failed = GameLogProcessResult::failed(
            "test.json".to_string(),
            "Error message".to_string(),
        );
        assert!(!failed.success);
        assert_eq!(failed.message, "Error message");
    }

    #[test]
    fn test_read_game_log_plain_text() {
        let temp_dir = std::env::temp_dir().join("mamo-test-gamelog-plain");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("replay_plain.json");
        std::fs::write(&file_path, b"{\"game\": \"data\"}").unwrap();

        let res = read_game_log(&file_path).expect("plain text should read successfully");
        assert_eq!(res.filename, "replay_plain.json");
        assert_eq!(res.content, "{\"game\": \"data\"}");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_read_game_log_raw_gzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let temp_dir = std::env::temp_dir().join("mamo-test-gamelog-gzip");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("replay_gzip.json");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"{\"gzipped\": true}").unwrap();
        let compressed = encoder.finish().unwrap();
        std::fs::write(&file_path, compressed).unwrap();

        let res = read_game_log(&file_path).expect("raw gzip should decompress successfully");
        assert_eq!(res.content, "{\"gzipped\": true}");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_read_game_log_base64_gzip() {
        use base64::prelude::*;
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let temp_dir = std::env::temp_dir().join("mamo-test-gamelog-b64-gzip");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("replay_b64.json");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"{\"b64\": true}").unwrap();
        let compressed = encoder.finish().unwrap();
        let b64 = BASE64_STANDARD.encode(&compressed);
        std::fs::write(&file_path, b64).unwrap();

        let res = read_game_log(&file_path).expect("base64 gzip should decompress successfully");
        assert_eq!(res.content, "{\"b64\": true}");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_read_game_log_empty_file() {
        let temp_dir = std::env::temp_dir().join("mamo-test-gamelog-empty");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("empty.json");
        std::fs::write(&file_path, b"").unwrap();

        let res = read_game_log(&file_path);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("empty"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
