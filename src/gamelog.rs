//! Game Log Reader Module
//! 
//! Monitors a configured directory for new MTG game log files (from Forge MTG),
//! parses them, and uploads them to the backend API for storage and analysis.
//! 
//! # Architecture
//! 
//! 1. **File Watcher**: Monitors a directory for new `.txt` or `.log` files
//! 2. **File Parser**: Reads and validates game log content
//! 3. **API Upload**: Sends parsed logs to backend as blob storage
//! 4. **Status Tracking**: Tracks which files have been processed

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
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
}

impl GameLogProcessResult {
    pub fn success(filename: String, file_size: u64, server_id: Option<String>) -> Self {
        Self {
            filename,
            success: true,
            message: "Successfully processed and uploaded".to_string(),
            file_size,
            processed_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            server_id,
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
}

fn default_extensions() -> Vec<String> {
    vec!["txt".to_string(), "log".to_string()]
}

fn default_scan_interval() -> u64 {
    30 // 30 seconds
}

fn default_api_url() -> String {
    "https://mamo-backend.vercel.app".to_string()
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
        }
    }
}

/// Get the default Forge game log directory based on OS
pub fn get_default_forge_log_directory() -> String {
    if cfg!(windows) {
        // Windows: %USERPROFILE%\AppData\Roaming\Forge\games\
        if let Some(appdata) = dirs::data_dir() {
            return appdata.join("Forge").join("games").to_string_lossy().to_string();
        }
    } else if cfg!(target_os = "macos") {
        // macOS: ~/Library/Application Support/Forge/games/
        if let Some(home) = dirs::home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("Forge")
                .join("games")
                .to_string_lossy()
                .to_string();
        }
    } else {
        // Linux: ~/.forge/games/
        if let Some(home) = dirs::home_dir() {
            return home.join(".forge").join("games").to_string_lossy().to_string();
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

/// Read and parse a game log file
pub fn read_game_log(path: &Path) -> Result<GameLogContent> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {:?}", path))?;
    
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
    
    let upload_payload = GameLogUploadPayload {
        filename: log_content.filename.clone(),
        content: log_content.content.clone(),
        file_size: log_content.file_size,
        modified_timestamp: log_content.modified_timestamp,
        user_id: config.user_id.clone(),
        uploaded_at: chrono::Utc::now().to_rfc3339(),
    };
    
    let url = format!("{}/api/gamelog/upload", config.api_url);
    
    let response = client
        .post(&url)
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

/// Payload for uploading a game log
#[derive(Debug, Serialize)]
pub struct GameLogUploadPayload {
    pub filename: String,
    pub content: String,
    pub file_size: u64,
    pub modified_timestamp: Option<u64>,
    pub user_id: Option<String>,
    pub uploaded_at: String,
}

/// Response from upload API
#[derive(Debug, Clone, Deserialize)]
pub struct UploadResponse {
    pub success: bool,
    pub message: String,
    pub gamelog_id: Option<String>,
}

/// Process new game logs from the configured directory
pub async fn process_new_logs(
    config: &GameLogConfig,
    processed_files: &Arc<Mutex<HashSet<String>>>,
) -> Result<ScanSummary> {
    let mut summary = ScanSummary::default();
    
    // Scan directory for files
    let files = scan_directory(config)?;
    summary.total_files_found = files.len();
    
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
        
        summary.new_files += 1;
        
        // Read the file
        let log_content = match read_game_log(&file_path) {
            Ok(content) => content,
            Err(e) => {
                summary.failed_uploads += 1;
                summary.results.push(GameLogProcessResult::failed(
                    filename.clone(),
                    format!("Failed to read file: {}", e),
                ));
                continue;
            }
        };
        
        // Upload to backend
        match upload_game_log(config, &log_content).await {
            Ok(response) => {
                if response.success {
                    summary.successfully_uploaded += 1;
                    summary.results.push(GameLogProcessResult::success(
                        filename.clone(),
                        log_content.file_size,
                        response.gamelog_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GameLogConfig::default();
        assert!(!config.watch_directory.is_empty() || cfg!(target_os = "linux"));
        assert_eq!(config.file_extensions, vec!["txt", "log"]);
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
            "test.log".to_string(),
            1024,
            Some("id123".to_string()),
        );
        assert!(success.success);
        assert_eq!(success.filename, "test.log");
        assert_eq!(success.file_size, 1024);
        assert_eq!(success.server_id, Some("id123".to_string()));

        let failed = GameLogProcessResult::failed(
            "test.log".to_string(),
            "Error message".to_string(),
        );
        assert!(!failed.success);
        assert_eq!(failed.message, "Error message");
    }
}
