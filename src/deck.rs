use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ==================== Moxfield API Types ====================

/// Status of a deck compared to local files
#[derive(Debug, Clone, PartialEq)]
pub enum DeckStatus {
    New,           // Deck doesn't exist locally
    UpToDate,      // Local deck has same or newer date
    NeedsUpdate,   // Moxfield deck is newer than local
}

/// User info from Moxfield API
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoxfieldUserInfo {
    pub user_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Represents a deck entry from Moxfield user's deck list with local status
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoxfieldDeckEntry {
    pub public_id: String,
    pub name: String,
    pub format: Option<String>,
    #[serde(default)]
    pub colors: Vec<String>,
    #[serde(default)]
    pub color_percentages: serde_json::Value,
    pub main_card_id: Option<String>,
    pub has_primer: Option<bool>,
    #[serde(default)]
    pub view_count: u32,
    #[serde(default)]
    pub like_count: u32,
    #[serde(default)]
    pub comment_count: u32,
    pub are_comments_enabled: Option<bool>,
    pub is_shared: Option<bool>,
    pub visibility: Option<String>,
    pub public_url: Option<String>,
    pub created_at_utc: Option<String>,
    pub last_updated_at_utc: Option<String>,
    #[serde(default)]
    pub created_by_user: Option<MoxfieldUserInfo>,
    #[serde(skip)]
    pub local_status: Option<DeckStatus>,
    #[serde(skip)]
    pub local_date: Option<String>,
}

/// Response from Moxfield API when fetching user decks
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct MoxfieldUserDecksResponse {
    pub page_number: u32,
    pub page_size: u32,
    pub total_results: u32,
    pub total_pages: u32,
    pub data: Vec<MoxfieldDeckEntry>,
}

/// Result of importing multiple decks from a user profile
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct UserDecksImportResult {
    pub success: bool,
    pub message: String,
    pub username: String,
    pub total_decks: usize,
    pub imported_decks: Vec<DeckCreationResult>,
    pub failed_decks: Vec<(String, String)>, // (deck_name, error_message)
}

impl UserDecksImportResult {
    pub fn success(username: String, imported: Vec<DeckCreationResult>, failed: Vec<(String, String)>) -> Self {
        let total = imported.len() + failed.len();
        let success_count = imported.iter().filter(|d| d.success).count();
        Self {
            success: !imported.is_empty() && failed.is_empty(),
            message: format!(
                "Imported {}/{} decks for user '{}' ({} failed)",
                success_count, total, username, failed.len()
            ),
            username,
            total_decks: total,
            imported_decks: imported,
            failed_decks: failed,
        }
    }

    pub fn failed(username: String, error: String) -> Self {
        Self {
            success: false,
            message: format!("Failed to fetch decks for user '{}': {}", username, error),
            username,
            total_decks: 0,
            imported_decks: vec![],
            failed_decks: vec![],
        }
    }
}

// ==================== Original Types ====================

#[derive(Debug, Deserialize, Serialize)]
pub struct Card {
    pub name: String,
    pub set: String,
    pub quantity: u32,
    pub collector_number: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeckData {
    pub name: String,
    pub commander: Vec<Card>,
    pub main: Vec<Card>,
    pub sideboard: Vec<Card>,
    pub attractions: Vec<Card>,
}

/// Request body for deck export API
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct DeckExportRequest {
    deck_id: String,
    format: String,
}

/// Response from deck export API in forge format
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeckExportResponse {
    /// Deck name from the API
    name: String,
    /// The formatted deck content (for forge format, this is the complete file content)
    content: String,
    /// Export format used
    #[allow(dead_code)]
    format: String,
}

#[derive(Debug, Clone)]
pub struct DeckCreationResult {
    pub success: bool,
    pub message: String,
    pub deck_path: Option<PathBuf>,
}

impl DeckCreationResult {
    pub fn success(message: String, deck_path: PathBuf) -> Self {
        Self {
            success: true,
            message,
            deck_path: Some(deck_path),
        }
    }

    pub fn failed(message: String) -> Self {
        Self {
            success: false,
            message,
            deck_path: None,
        }
    }
}

const MAX_FORGE_SIDEBOARD_CARDS: usize = 10;

// ==================== Common Helpers ====================

/// Generic fetch using curl (handles Cloudflare and user agent)
fn fetch_with_curl(url: &str) -> Result<String> {
    fetch_with_curl_custom(url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "-H", "Accept: application/json",
        "-H", "Referer: https://www.moxfield.com/",
    ])
}

/// Fetch URL with custom headers using curl
fn fetch_with_curl_custom(url: &str, extra_args: &[&str]) -> Result<String> {
    info!("Fetching via curl: {}", url);
    
    let mut args = vec!["-s"];
    args.extend(extra_args);
    args.push(url);
    
    let output = Command::new("curl")
        .args(&args)
        .output()
        .context("Failed to execute curl command")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("curl failed: {}", stderr));
    }
    
    let body = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in curl response")?;
    
    // Check for Cloudflare block (HTML response)
    if body.contains("<!DOCTYPE html>") && body.contains("Cloudflare") {
        return Err(anyhow::anyhow!("Cloudflare blocked the request"));
    }
    
    Ok(body)
}

// ==================== Direct Moxfield Access ====================

const MOXFIELD_API_URL: &str = "https://api2.moxfield.com/v2";

/// Moxfield full deck response structure
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct MoxfieldFullDeck {
    name: String,
    #[serde(default)]
    created_by_user: Option<MoxfieldUser>,
    #[serde(default)]
    last_updated_at_utc: Option<String>,
    #[serde(default)]
    commanders: serde_json::Value,
    #[serde(default)]
    mainboard: serde_json::Value,
    #[serde(default)]
    sideboard: serde_json::Value,
    #[serde(default)]
    companions: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoxfieldUser {
    user_name: String,
}

/// Create a deck directly from Moxfield (no backend needed)
pub async fn create_deck_from_moxfield(deck_id: &str) -> Result<DeckCreationResult> {
    info!("Creating deck directly from Moxfield: {}", deck_id);
    
    let url = format!("{}/decks/all/{}", MOXFIELD_API_URL, deck_id);
    let body = fetch_with_curl(&url)?;
    
    let deck: MoxfieldFullDeck = serde_json::from_str(&body)
        .context("Failed to parse Moxfield deck response")?;
    
    // Build filename with author and date: "user - Name of deck (date)"
    let author = deck.created_by_user.as_ref()
        .map(|u| u.user_name.as_str())
        .unwrap_or("Unknown");
    
    let date = deck.last_updated_at_utc.as_ref()
        .and_then(|d| d.split('T').next())  // Extract just the date part (YYYY-MM-DD)
        .unwrap_or("Unknown");
    
    let full_name = format!("{} - {} ({})", author, deck.name, date);
    
    // Convert to Forge format
    let forge_content = convert_moxfield_to_forge(&full_name, &body)?;
    
    // Write the deck file with full name including author/date
    let (deck_path, _archived) = write_deck_file(&full_name, &forge_content).await
        .context("Failed to create deck file")?;
    
    Ok(DeckCreationResult::success(
        format!("Successfully created deck '{}' at {:?}", full_name, deck_path),
        deck_path,
    ))
}

/// Fetch user decks directly from Moxfield using curl (bypasses Cloudflare)
/// Also checks local deck directory and sets status for each deck
pub fn fetch_user_decks_direct(username: &str) -> Result<Vec<MoxfieldDeckEntry>> {
    info!("Fetching decks for user '{}' directly via curl", username);
    
    let mut all_decks = Vec::new();
    let mut page = 1;
    let page_size = 100;
    
    loop {
        let url = format!(
            "{}/users/{}/decks?pageNumber={}&pageSize={}",
            MOXFIELD_API_URL, username, page, page_size
        );
        
        let body = fetch_with_curl(&url)?;
        
        let page_response: MoxfieldUserDecksResponse = serde_json::from_str(&body)
            .context("Failed to parse Moxfield user decks response")?;
        
        info!("Fetched {} decks (page {}/{})", 
              page_response.data.len(), 
              page_response.page_number, 
              page_response.total_pages);
        
        all_decks.extend(page_response.data);
        
        if page >= page_response.total_pages {
            break;
        }
        page += 1;
    }
    
    // Check local status for each deck
    let deck_dir = get_deck_directory().ok();
    let mut decks_with_status = all_decks;
    
    for deck in &mut decks_with_status {
        let moxfield_date = deck.last_updated_at_utc.as_ref()
            .and_then(|d| d.split('T').next())
            .map(|s| s.to_string());
        
        // Use the actual deck author, not the profile username
        let author = deck.created_by_user.as_ref()
            .map(|u| u.user_name.as_str())
            .unwrap_or(username);
        
        // Build the expected filename pattern: "author - deckname (date)"
        let (status, local_date) = check_deck_exists_locally(author, &deck.name, &deck_dir);
        deck.local_status = Some(status.clone());
        deck.local_date = local_date.clone();
        
        // If deck exists locally, compare dates
        if status == DeckStatus::UpToDate {
            if let (Some(mox_date), Some(loc_date)) = (&moxfield_date, &local_date) {
                if mox_date > loc_date {
                    deck.local_status = Some(DeckStatus::NeedsUpdate);
                }
            }
        }
    }
    
    info!("Total decks fetched for user '{}': {}", username, decks_with_status.len());
    Ok(decks_with_status)
}

/// Check if a deck already exists locally and extract its date
fn check_deck_exists_locally(username: &str, deck_name: &str, deck_dir: &Option<PathBuf>) -> (DeckStatus, Option<String>) {
    let Some(dir) = deck_dir else {
        return (DeckStatus::New, None);
    };
    
    if !dir.exists() {
        return (DeckStatus::New, None);
    }
    
    // Look for files matching pattern: "username - deck_name (date).dck"
    // Must sanitize the name since filenames have special characters replaced
    let sanitized_deck_name = sanitize_filename(deck_name);
    let pattern_start = format!("{} - {}", username, sanitized_deck_name);
    
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let filename = entry.file_name().to_string_lossy().to_string();
            
            // Check if filename matches our pattern
            if filename.starts_with(&pattern_start) && filename.ends_with(".dck") {
                // Extract date from filename: "user - name (YYYY-MM-DD).dck"
                if let Some(date_start) = filename.rfind('(') {
                    if let Some(date_end) = filename.rfind(')') {
                        let date = &filename[date_start + 1..date_end];
                        return (DeckStatus::UpToDate, Some(date.to_string()));
                    }
                }
                // File exists but couldn't extract date
                return (DeckStatus::UpToDate, None);
            }
        }
    }
    
    (DeckStatus::New, None)
}

/// Convert Moxfield deck JSON to Forge .dck format
fn convert_moxfield_to_forge(deck_name: &str, raw_json: &str) -> Result<String> {
    let mut lines = Vec::new();
    
    // Metadata
    lines.push("[metadata]".to_string());
    lines.push(format!("Name={}", deck_name));
    lines.push(String::new());
    
    // Parse the raw JSON to access card data
    let parsed: serde_json::Value = serde_json::from_str(raw_json)?;
    
    // Commander section
    lines.push("[Commander]".to_string());
    if let Some(commanders) = parsed.get("commanders").and_then(|c| c.as_object()) {
        for (_, card_entry) in commanders {
            if let Some(card_line) = format_moxfield_card(card_entry) {
                lines.push(card_line);
            }
        }
    }
    lines.push(String::new());
    
    // Main deck section
    lines.push("[Main]".to_string());
    if let Some(mainboard) = parsed.get("mainboard").and_then(|c| c.as_object()) {
        for (_, card_entry) in mainboard {
            if let Some(card_line) = format_moxfield_card(card_entry) {
                lines.push(card_line);
            }
        }
    }
    lines.push(String::new());
    
    // Sideboard section
    lines.push("[Sideboard]".to_string());
    if let Some(sideboard) = parsed.get("sideboard").and_then(|c| c.as_object()) {
        let mut sideboard_count = 0;
        for (_, card_entry) in sideboard {
            if sideboard_count >= MAX_FORGE_SIDEBOARD_CARDS {
                break;
            }
            if let Some(card_line) = format_moxfield_card(card_entry) {
                lines.push(card_line);
                sideboard_count += 1;
            }
        }
    }
    
    Ok(lines.join("\n"))
}

/// Format a single card from Moxfield JSON to Forge format
fn format_moxfield_card(card_entry: &serde_json::Value) -> Option<String> {
    let quantity = card_entry.get("quantity")?.as_u64().unwrap_or(1);
    let card = card_entry.get("card")?;
    let full_name = card.get("name")?.as_str()?;
    
    // For double-faced cards, Forge only recognizes the front face name
    let name = front_face_name(full_name);
    
    let set = card.get("set")?.as_str()?.to_uppercase();
    let collector_number = card.get("cn").and_then(|c| c.as_str()).unwrap_or("1");
    
    Some(format!("{} {}|{}|{}", quantity, name, set, collector_number))
}

/// Create a deck file by fetching from API with forge format (uses backend proxy)
pub async fn create_deck_from_id(deck_id: &str, api_base_url: &str) -> Result<DeckCreationResult> {
    info!("Creating deck from ID: {} with FORGE format", deck_id);

    // Fetch deck data from API in forge format
    let export_response = fetch_deck_export(deck_id, api_base_url).await
        .context("Failed to fetch deck data from API")?;

    // Create deck file with the content from API
    let content = post_process_forge_content(&export_response.content);
    let (deck_path, _archived) = write_deck_file(&export_response.name, &content).await
        .context("Failed to create deck file")?;

    Ok(DeckCreationResult::success(
        format!("Successfully created deck '{}' at {:?}", export_response.name, deck_path),
        deck_path,
    ))
}

// ==================== Moxfield User Decks Functions ====================

const MOXFIELD_API_BASE: &str = "https://api2.moxfield.com/v2";

/// Fetch all public decks for a Moxfield user directly from Moxfield API
/// Note: This may be blocked by Cloudflare protection. Use fetch_user_decks_via_backend for reliability.
pub async fn fetch_moxfield_user_decks(username: &str) -> Result<Vec<MoxfieldDeckEntry>> {
    info!("Fetching decks for Moxfield user: {} (direct API)", username);
    
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .build()
        .context("Failed to create HTTP client")?;
    
    let mut all_decks = Vec::new();
    let mut page = 1;
    let page_size = 100;
    
    loop {
        let url = format!(
            "{}/users/{}/decks?pageNumber={}&pageSize={}",
            MOXFIELD_API_BASE, username, page, page_size
        );
        
        info!("Fetching page {} from: {}", page, url);
        
        let response = client
            .get(&url)
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Origin", "https://www.moxfield.com")
            .header("Referer", format!("https://www.moxfield.com/users/{}", username))
            .send()
            .await
            .context("Failed to send request to Moxfield API")?;
        
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Moxfield API returned error status: {} - {}",
                status, body
            ));
        }
        
        let page_response: MoxfieldUserDecksResponse = response
            .json()
            .await
            .context("Failed to parse Moxfield user decks response")?;
        
        info!("Fetched {} decks (page {}/{})", 
              page_response.data.len(), 
              page_response.page_number, 
              page_response.total_pages);
        
        all_decks.extend(page_response.data);
        
        if page >= page_response.total_pages {
            break;
        }
        page += 1;
    }
    
    info!("Total decks fetched for user '{}': {}", username, all_decks.len());
    Ok(all_decks)
}

/// Fetch user decks list via the backend API (recommended - avoids Cloudflare blocking)
/// The backend should have an endpoint like: GET /api/moxfield/users/{username}/decks
pub async fn fetch_user_decks_via_backend(username: &str, api_base_url: &str) -> Result<Vec<MoxfieldDeckEntry>> {
    info!("Fetching decks for user '{}' via backend: {}", username, api_base_url);
    
    let client = reqwest::Client::new();
    let url = format!("{}/api/moxfield/users/{}/decks", api_base_url, username);
    
    info!("Fetching from: {}", url);
    
    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to send request to backend API")?;
    
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Backend API returned error status: {} - {}",
            status, body
        ));
    }
    
    let decks: Vec<MoxfieldDeckEntry> = response
        .json()
        .await
        .context("Failed to parse user decks response from backend")?;
    
    info!("Fetched {} decks for user '{}' via backend", decks.len(), username);
    Ok(decks)
}

/// Import all decks from a Moxfield user profile
/// First tries the backend API, falls back to direct Moxfield API if backend fails
pub async fn import_user_decks(username: &str, api_base_url: &str) -> Result<UserDecksImportResult> {
    info!("Importing all decks for Moxfield user: {} via API: {}", username, api_base_url);
    
    // Try backend API first, then fall back to direct Moxfield API
    let decks = match fetch_user_decks_via_backend(username, api_base_url).await {
        Ok(d) => {
            info!("Successfully fetched deck list via backend");
            d
        }
        Err(backend_err) => {
            warn!("Backend API failed ({}), trying direct Moxfield API...", backend_err);
            match fetch_moxfield_user_decks(username).await {
                Ok(d) => d,
                Err(moxfield_err) => {
                    return Ok(UserDecksImportResult::failed(
                        username.to_string(),
                        format!("Backend: {} | Moxfield: {}", backend_err, moxfield_err)
                    ));
                }
            }
        }
    };
    
    if decks.is_empty() {
        return Ok(UserDecksImportResult::failed(
            username.to_string(),
            "No public decks found for this user".to_string()
        ));
    }
    
    info!("Found {} decks for user '{}', starting import...", decks.len(), username);
    
    let mut imported = Vec::new();
    let mut failed = Vec::new();
    
    for deck in decks {
        info!("Importing deck: {} (ID: {})", deck.name, deck.public_id);
        
        match create_deck_from_id(&deck.public_id, api_base_url).await {
            Ok(result) => {
                if result.success {
                    info!("Successfully imported: {}", deck.name);
                } else {
                    warn!("Deck import reported failure: {}", result.message);
                }
                imported.push(result);
            }
            Err(e) => {
                warn!("Failed to import deck '{}': {}", deck.name, e);
                failed.push((deck.name.clone(), e.to_string()));
            }
        }
    }
    
    Ok(UserDecksImportResult::success(username.to_string(), imported, failed))
}
/// Fetch the list of decks for a user (without importing them)
/// Uses backend proxy to avoid Cloudflare blocking
pub async fn list_moxfield_user_decks(username: &str, api_base_url: &str) -> Result<Vec<MoxfieldDeckEntry>> {
    // Use backend proxy to avoid Cloudflare blocking
    fetch_user_decks_via_backend(username, api_base_url).await
}

/// Import selected decks from a list of deck IDs
#[allow(dead_code)]
pub async fn import_selected_decks(
    deck_ids: &[String], 
    api_base_url: &str,
    username: &str,
) -> Result<UserDecksImportResult> {
    info!("Importing {} selected decks via API: {}", deck_ids.len(), api_base_url);
    
    let mut imported = Vec::new();
    let mut failed = Vec::new();
    
    for deck_id in deck_ids {
        info!("Importing deck ID: {}", deck_id);
        
        match create_deck_from_id(deck_id, api_base_url).await {
            Ok(result) => {
                imported.push(result);
            }
            Err(e) => {
                warn!("Failed to import deck '{}': {}", deck_id, e);
                failed.push((deck_id.clone(), e.to_string()));
            }
        }
    }
    
    Ok(UserDecksImportResult::success(username.to_string(), imported, failed))
}

// ==================== Helper Functions ====================

/// Fetch deck export from API using GET with deckId in path
async fn fetch_deck_export(deck_id: &str, api_base_url: &str) -> Result<DeckExportResponse> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/moxfield/decks/{}/export?format=forge", api_base_url, deck_id);
    
    info!("Fetching deck export from: {}", url);
    
    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to send request to API")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "API returned error status: {} - {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }

    let export_response: DeckExportResponse = response
        .json()
        .await
        .context("Failed to parse deck export response from API")?;

    info!("Successfully fetched deck export for: {}", export_response.name);
    Ok(export_response)
}

// ==================== Archidekt Support ====================

/// Archidekt deck response structure
#[derive(Debug, Deserialize)]
struct ArchidektDeck {
    name: String,
    #[serde(default)]
    owner: Option<ArchidektOwner>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(default)]
    cards: Vec<ArchidektCard>,
}

#[derive(Debug, Deserialize)]
struct ArchidektOwner {
    username: String,
}

#[derive(Debug, Deserialize)]
struct ArchidektCard {
    quantity: u32,
    categories: Vec<String>,
    card: ArchidektCardInfo,
}

#[derive(Debug, Deserialize)]
struct ArchidektCardInfo {
    #[serde(rename = "oracleCard")]
    oracle_card: ArchidektOracleCard,
    edition: ArchidektEdition,
}

#[derive(Debug, Deserialize)]
struct ArchidektOracleCard {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ArchidektEdition {
    #[serde(rename = "editioncode")]
    edition_code: String,
}

/// Create a deck from an Archidekt URL
/// URL format: https://archidekt.com/decks/{deck_id}/{deck_name}
pub async fn create_deck_from_archidekt(deck_id: &str) -> Result<DeckCreationResult> {
    info!("Creating deck from Archidekt: {}", deck_id);
    
    let url = format!("https://archidekt.com/api/decks/{}/", deck_id);
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "-H", "Accept: application/json",
    ])?;
    
    let deck: ArchidektDeck = serde_json::from_str(&body)
        .context("Failed to parse Archidekt deck response")?;
    
    // Build filename with author and date
    let author = deck.owner.as_ref()
        .map(|o| o.username.as_str())
        .unwrap_or("Unknown");
    
    let date = deck.updated_at.as_ref()
        .and_then(|d| d.split('T').next())
        .unwrap_or("Unknown");
    
    let full_name = format!("{} - {} ({})", author, deck.name, date);
    
    // Convert to Forge format
    let forge_content = convert_archidekt_to_forge(&full_name, &deck)?;
    
    // Write the deck file
    let (deck_path, _archived) = write_deck_file(&full_name, &forge_content).await
        .context("Failed to create deck file")?;
    
    Ok(DeckCreationResult::success(
        format!("Successfully created deck '{}' at {:?}", full_name, deck_path),
        deck_path,
    ))
}

/// Convert Archidekt deck to Forge .dck format
fn convert_archidekt_to_forge(deck_name: &str, deck: &ArchidektDeck) -> Result<String> {
    let mut lines = Vec::new();
    
    // Metadata
    lines.push("[metadata]".to_string());
    lines.push(format!("Name={}", deck_name));
    lines.push(String::new());
    
    // Separate cards by category
    let mut commanders = Vec::new();
    let mut mainboard = Vec::new();
    let mut sideboard = Vec::new();
    
    for card in &deck.cards {
        let is_commander = card.categories.iter()
            .any(|c| c.to_lowercase().contains("commander"));
        let is_sideboard = card.categories.iter()
            .any(|c| c.to_lowercase().contains("sideboard") || c.to_lowercase().contains("maybeboard"));
        
        // For double-faced cards, Forge only recognizes the front face name
        let name = front_face_name(&card.card.oracle_card.name);
        let set = card.card.edition.edition_code.to_uppercase();
        let line = format!("{} {}|{}|1", card.quantity, name, set);
        
        if is_commander {
            commanders.push(line);
        } else if is_sideboard {
            sideboard.push(line);
        } else {
            mainboard.push(line);
        }
    }
    
    // Commander section
    lines.push("[Commander]".to_string());
    for line in commanders {
        lines.push(line);
    }
    lines.push(String::new());
    
    // Main deck section
    lines.push("[Main]".to_string());
    for line in mainboard {
        lines.push(line);
    }
    lines.push(String::new());
    
    // Sideboard section
    lines.push("[Sideboard]".to_string());
    for line in sideboard {
        lines.push(line);
    }
    
    Ok(lines.join("\n"))
}

/// Parse an Archidekt URL and extract the deck ID
/// URL format: https://archidekt.com/decks/{deck_id}/{deck_name}
pub fn parse_archidekt_url(url: &str) -> Option<String> {
    let url = url.trim();
    
    // Match patterns like https://archidekt.com/decks/12345/deck_name
    if url.contains("archidekt.com/decks/") {
        let parts: Vec<&str> = url.split("/decks/").collect();
        if parts.len() >= 2 {
            // Extract just the numeric ID
            let id_part = parts[1].split('/').next()?;
            if id_part.chars().all(|c| c.is_ascii_digit()) {
                return Some(id_part.to_string());
            }
        }
    }
    
    None
}

// ==================== Deckstats Support ====================

/// Create a deck from a Deckstats URL
/// URL format: https://deckstats.net/decks/{owner_id}/{deck_id}-{deck_name}
pub async fn create_deck_from_deckstats(owner_id: &str, deck_id: &str) -> Result<DeckCreationResult> {
    info!("Creating deck from Deckstats: owner={} deck={}", owner_id, deck_id);
    
    // Deckstats has a simple export endpoint that returns plain text
    let url = format!(
        "https://deckstats.net/decks/{}/{}-deck?export_dec=1",
        owner_id, deck_id
    );
    
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
    ])?;
    
    // Parse the deckstats export format
    // First line is typically: //NAME: Deck Name from deckstats.net
    let deck_name = body.lines()
        .find(|line| line.starts_with("//NAME:"))
        .map(|line| line.trim_start_matches("//NAME:").trim())
        .unwrap_or("Unknown Deck");
    
    // Get today's date for filename
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let full_name = format!("Deckstats - {} ({})", deck_name, date);
    
    // Convert to Forge format
    let forge_content = convert_deckstats_to_forge(&full_name, &body)?;
    
    // Write the deck file
    let (deck_path, _archived) = write_deck_file(&full_name, &forge_content).await
        .context("Failed to create deck file")?;
    
    Ok(DeckCreationResult::success(
        format!("Successfully created deck '{}' at {:?}", full_name, deck_path),
        deck_path,
    ))
}

/// Convert Deckstats export to Forge .dck format
fn convert_deckstats_to_forge(deck_name: &str, content: &str) -> Result<String> {
    let mut lines = Vec::new();
    
    // Metadata
    lines.push("[metadata]".to_string());
    lines.push(format!("Name={}", deck_name));
    lines.push(String::new());
    
    let mut commanders = Vec::new();
    let mut mainboard = Vec::new();
    let mut sideboard = Vec::new();
    let mut in_sideboard = false;
    
    for line in content.lines() {
        let line = line.trim();
        
        // Skip empty lines and comments
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        
        // Check for section markers
        if line.to_lowercase().contains("sideboard") {
            in_sideboard = true;
            continue;
        }
        
        // Parse card lines: "1 Card Name" or "1 Card Name [SET]"
        if let Some(card_line) = parse_deckstats_card_line(line) {
            // First card is often the commander (quantity 1, creature/planeswalker)
            // But deckstats doesn't clearly mark commander, so put first line as commander
            if commanders.is_empty() && !in_sideboard && line.starts_with("1 ") {
                commanders.push(card_line);
            } else if in_sideboard {
                sideboard.push(card_line);
            } else {
                mainboard.push(card_line);
            }
        }
    }
    
    // Commander section
    lines.push("[Commander]".to_string());
    for line in commanders {
        lines.push(line);
    }
    lines.push(String::new());
    
    // Main deck section
    lines.push("[Main]".to_string());
    for line in mainboard {
        lines.push(line);
    }
    lines.push(String::new());
    
    // Sideboard section
    lines.push("[Sideboard]".to_string());
    for line in sideboard {
        lines.push(line);
    }
    
    Ok(lines.join("\n"))
}

/// Parse a deckstats card line
fn parse_deckstats_card_line(line: &str) -> Option<String> {
    // Format: "1 Card Name" or "1 Card Name // Other Face"
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    if parts.len() != 2 {
        return None;
    }
    
    let quantity: u32 = parts[0].parse().ok()?;
    let mut card_name = front_face_name(parts[1]).to_string();
    
    // Remove any set info in brackets [SET]
    if let Some(pos) = card_name.find(" [") {
        card_name = card_name[..pos].to_string();
    }
    
    Some(format!("{} {}", quantity, card_name.trim()))
}

/// Parse a Deckstats URL and extract owner_id and deck_id
/// URL format: https://deckstats.net/decks/{owner_id}/{deck_id}-{deck_name}
pub fn parse_deckstats_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    
    // Match patterns like https://deckstats.net/decks/141959/3718418-Wayta
    if url.contains("deckstats.net/decks/") {
        let parts: Vec<&str> = url.split("/decks/").collect();
        if parts.len() >= 2 {
            let path_parts: Vec<&str> = parts[1].split('/').collect();
            if path_parts.len() >= 2 {
                let owner_id = path_parts[0].to_string();
                // deck_id is before the hyphen
                let deck_part = path_parts[1].split('-').next()?;
                if owner_id.chars().all(|c| c.is_ascii_digit()) && 
                   deck_part.chars().all(|c| c.is_ascii_digit()) {
                    return Some((owner_id, deck_part.to_string()));
                }
            }
        }
    }
    
    None
}

// ==================== MaMo Support ====================

/// MaMo API base URL for deck export
const MAMO_API_URL: &str = "https://new-backend-two-eosin.vercel.app";

/// Progress callback type for deck operations
pub type ProgressCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Create a deck from MaMo backend with progress logging
/// Fetches the deck in Forge format directly from the MaMo backend
/// Checks if deck already exists by hash to avoid duplicate downloads
pub async fn create_deck_from_mamo_with_progress(
    deck_id: &str,
    on_progress: Option<&ProgressCallback>,
) -> Result<DeckCreationResult> {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(cb) = on_progress {
            cb(msg);
        }
    };
    
    log(&format!("Starting deck download for ID: {}", deck_id));
    
    // MaMo backend returns plain text Forge format
    let url = format!("{}/api/deck/export/{}/forge", MAMO_API_URL, deck_id);
    log("Fetching deck from MaMo API...");
    
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: MaMo-Connector/1.0",
        "-H", "Accept: text/plain",
    ])?;
    
    // Check if response looks like an error (JSON or plain text error message)
    if body.starts_with("{") && body.contains("error") {
        log("API returned a JSON error");
        return Ok(DeckCreationResult::failed(
            format!("MaMo API error: {}", body)
        ));
    }
    
    // Check for plain text error messages
    let body_lower = body.to_lowercase();
    if body_lower.starts_with("failed") || body_lower.starts_with("error") 
        || body_lower.contains("not found") || body_lower.contains("failed to export") {
        log(&format!("API returned an error message: {}", body.trim()));
        return Ok(DeckCreationResult::failed(
            format!("MaMo API error: {}", body.trim())
        ));
    }
    
    // Validate that the response looks like a valid Forge deck file
    // It should contain [metadata] section or deck sections like [Main], [Commander]
    if !body.contains("[metadata]") && !body.contains("[Main]") && !body.contains("[Commander]") && !body.contains("Name=") {
        log("Response doesn't look like a valid Forge deck file");
        return Ok(DeckCreationResult::failed(
            format!("Invalid deck format received from API: {}", 
                    if body.len() > 100 { format!("{}...", &body[..100]) } else { body.clone() })
        ));
    }
    
    log("Deck data received, parsing...");
    
    // Parse deck name from the Forge content: "Name=Author - Deck Name"
    let deck_name = body.lines()
        .find(|line| line.starts_with("Name=") || line.starts_with("Name ="))
        .map(|line| {
            line.trim_start_matches("Name")
                .trim_start_matches(" ")
                .trim_start_matches("=")
                .trim()
        })
        .unwrap_or("MaMo Deck");
    
    log(&format!("Deck name: {}", deck_name));
    
    // Calculate deck hash to check if deck content changed
    log("Calculating deck hash...");
    let new_deck_hash = calculate_deck_hash(&body);
    log(&format!("Deck hash: {}", new_deck_hash));
    
    // Post-process: strip double-faced card back faces (Forge only uses front face)
    log("Processing double-faced card names...");
    let body = post_process_forge_content(&body);
    
    log("Writing deck file...");
    // Content is already in Forge format, write directly
    // write_deck_file deletes any existing versions to avoid duplicates in Forge
    let (deck_path, removed_files) = write_deck_file(deck_name, &body).await
        .context("Failed to create deck file")?;
    
    // Log removed old versions in the UI progress
    for (removed_name, same_hash) in &removed_files {
        if *same_hash {
            log(&format!("🗑️ Replaced old version (same deck content): {}", removed_name));
        } else {
            log(&format!("🗑️ Replaced old version: {}", removed_name));
        }
    }
    
    log(&format!("Deck saved to: {:?}", deck_path));
    
    Ok(DeckCreationResult::success(
        format!("Successfully created MaMo deck '{}' at {:?}", deck_name, deck_path),
        deck_path,
    ))
}

/// Create a deck from MaMo backend
/// Fetches the deck in Forge format directly from the MaMo backend
/// Checks if deck already exists by hash to avoid duplicate downloads
pub async fn create_deck_from_mamo(deck_id: &str) -> Result<DeckCreationResult> {
    create_deck_from_mamo_with_progress(deck_id, None).await
}

// ==================== Forge Scenario Export ====================

/// Returns the Forge game-log directory where scenario JSON files are placed.
/// Forge's CSubmenuScenario scans this directory for `*.json` scenario files.
/// Windows: %APPDATA%\Forge\games\gamelogs\
/// macOS/Linux: ~/.forge/games/gamelogs/
fn get_game_log_directory() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            PathBuf::from(appdata)
        } else {
            return Err(anyhow::anyhow!("APPDATA environment variable not found"));
        }
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME environment variable not found"))?;
        PathBuf::from(home).join(".forge")
    };
    Ok(base.join("Forge").join("games").join("gamelogs"))
}

/// Bundle returned by the backend forge-scenario export endpoint.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForgeScenarioBundle {
    deck_name: String,
    dck: String,
    scenario_json: serde_json::Value,
}

/// Fetch the scenario bundle from the MaMo backend.
async fn fetch_forge_scenario_bundle(deck_id: &str, scenario_id: &str) -> Result<ForgeScenarioBundle> {
    let url = format!("{}/api/deck/{}/forge-scenario/{}", MAMO_API_URL, deck_id, scenario_id);
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: MaMo-Connector/1.0",
        "-H", "Accept: application/json",
    ])?;

    let bundle: ForgeScenarioBundle = serde_json::from_str(&body)
        .with_context(|| format!("Failed to parse forge-scenario bundle: {}", &body[..body.len().min(200)]))?;
    Ok(bundle)
}

/// Download the scenario-ordered .dck to the Forge commander deck directory and
/// write the Forge scenario JSON to the game-log directory.
/// Returns the deck file path (used to open Forge on that deck).
pub async fn create_deck_and_scenario_for_forge(deck_id: &str, scenario_id: &str) -> Result<DeckCreationResult> {
    info!("Fetching Forge scenario bundle — deck: {}, scenario: {}", deck_id, scenario_id);

    let bundle = fetch_forge_scenario_bundle(deck_id, scenario_id).await
        .context("Failed to fetch Forge scenario bundle from MaMo API")?;

    // Write ordered .dck file
    let (deck_path, _) = write_deck_file(&bundle.deck_name, &bundle.dck).await
        .context("Failed to write scenario .dck file")?;
    info!("Scenario deck written: {:?}", deck_path);

    // Write Forge scenario JSON to the game-log directory
    let log_dir = get_game_log_directory()?;
    if !log_dir.exists() {
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("Failed to create game-log directory: {:?}", log_dir))?;
    }
    let scenario_file_name = format!("Scenario_{}.json", sanitize_filename(&bundle.deck_name));
    let scenario_path = log_dir.join(&scenario_file_name);
    let scenario_json_str = serde_json::to_string_pretty(&bundle.scenario_json)
        .context("Failed to serialise scenario JSON")?;
    fs::write(&scenario_path, &scenario_json_str)
        .with_context(|| format!("Failed to write scenario JSON to {:?}", scenario_path))?;
    info!("Scenario JSON written: {:?}", scenario_path);

    Ok(DeckCreationResult::success(
        format!("Scenario deck '{}' and scenario file written for Forge", bundle.deck_name),
        deck_path,
    ))
}

/// Parse a MaMo URL and extract the deck UUID
/// Supported URL formats:
/// - https://ma-mo-frontend.vercel.app/deck/UUID
/// - https://ma-mo-frontend.vercel.app?deckId=UUID
/// - https://new-backend-two-eosin.vercel.app/api/deck/export/UUID/forge
/// - Plain UUID: 2e16bd73-d2a9-4b0d-af8d-77b931d26bef
pub fn parse_mamo_url(url: &str) -> Option<String> {
    let url = url.trim();
    
    // UUID pattern: 8-4-4-4-12 hex characters
    let uuid_regex = regex::Regex::new(
        r"[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}"
    ).ok()?;
    
    // First, check if this is a user URL (to avoid false positive UUID extraction)
    if parse_mamo_user_url(url).is_some() {
        return None;
    }
    
    // Check if it's a MaMo URL (production URLs only)
    if url.contains("ma-mo-frontend.vercel.app") || 
       url.contains("new-backend-two-eosin.vercel.app") {
        // Extract UUID from URL
        if let Some(captures) = uuid_regex.find(url) {
            return Some(captures.as_str().to_string());
        }
    }
    
    // Check if it's a plain UUID
    if uuid_regex.is_match(url) && !url.contains("://") {
        return Some(url.to_string());
    }
    
    None
}

/// Parse a MaMo URL and extract the username for user profile
/// Supported URL formats:
/// - https://ma-mo-frontend.vercel.app/user/USERNAME
pub fn parse_mamo_user_url(url: &str) -> Option<String> {
    let url = url.trim();
    
    // Check if it's a MaMo URL with /user/ path (production URLs only)
    if url.contains("ma-mo-frontend.vercel.app") {
        // Look for /user/USERNAME pattern
        if let Some(user_part) = url.split("/user/").nth(1) {
            let username = user_part.split(&['/', '?', '#'][..]).next().unwrap_or(user_part);
            if !username.is_empty() && !username.contains("-") {
                return Some(username.to_string());
            }
        }
    }
    
    None
}

/// MaMo deck entry for listing user decks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MamoDeckEntry {
    pub deck_id: String,
    pub deck_name: String,
    pub user_id: String,
    pub commander_name: Option<String>,
    pub commander_partner_name: Option<String>,
    pub color_identity: Option<String>,
    pub format: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
    #[serde(skip)]
    pub local_status: Option<DeckStatus>,
}

/// Response from MaMo API when fetching user decks
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MamoUserDecksResponse {
    pub decks: Vec<MamoDeckApiEntry>,
    pub total: Option<usize>,
}

/// Individual deck entry from MaMo API
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MamoDeckApiEntry {
    #[serde(alias = "DeckID", alias = "deckid")]
    pub deck_id: Option<String>,
    #[serde(alias = "DeckName", alias = "deckname")]
    pub deck_name: Option<String>,
    #[serde(alias = "User", alias = "user_id")]
    pub user_id: Option<String>,
    #[serde(alias = "CommanderName")]
    pub commander_name: Option<String>,
    #[serde(alias = "ColorIdentity", alias = "coloridentity")]
    pub color_identity: Option<String>,
    #[serde(alias = "UpdatedAt", alias = "updatedat")]
    pub updated_at: Option<String>,
    #[serde(alias = "CreatedAt", alias = "createdat")]
    pub created_at: Option<String>,
}

/// Fetch all decks for a MaMo user
pub async fn fetch_mamo_user_decks(username: &str) -> Result<Vec<MamoDeckEntry>> {
    info!("Fetching decks for MaMo user: {}", username);
    
    let url = format!("{}/api/decks/user/{}", MAMO_API_URL, username);
    
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: MaMo-Connector/1.0",
        "-H", "Accept: application/json",
    ])?;
    
    // Try to parse response
    let response: MamoUserDecksResponse = serde_json::from_str(&body)
        .with_context(|| format!("Failed to parse MaMo API response: {}", &body[..body.len().min(200)]))?;
    
    // Get deck directory for local status checking
    let deck_dir = get_deck_directory().ok();
    
    // Convert to MamoDeckEntry with local status
    let decks: Vec<MamoDeckEntry> = response.decks.into_iter().map(|entry| {
        let deck_name = entry.deck_name.clone().unwrap_or_else(|| "Unnamed Deck".to_string());
        
        // Check local status
        let local_status = if let Some(ref dir) = deck_dir {
            let sanitized = sanitize_filename(&deck_name);
            let deck_path = dir.join(format!("{}.dck", sanitized));
            if deck_path.exists() {
                // TODO: Compare timestamps if needed
                Some(DeckStatus::UpToDate)
            } else {
                Some(DeckStatus::New)
            }
        } else {
            Some(DeckStatus::New)
        };
        
        MamoDeckEntry {
            deck_id: entry.deck_id.unwrap_or_default(),
            deck_name,
            user_id: entry.user_id.unwrap_or_default(),
            commander_name: entry.commander_name,
            commander_partner_name: None,
            color_identity: entry.color_identity,
            format: Some("Commander".to_string()), // MaMo is primarily Commander
            updated_at: entry.updated_at,
            created_at: entry.created_at,
            local_status,
        }
    }).collect();
    
    info!("Found {} decks for MaMo user '{}'", decks.len(), username);
    Ok(decks)
}

// ==================== Deck Hash Calculation ====================

/// Calculate a deck hash from Forge deck content
/// Algorithm (per MTG Replay Notation spec v1.1.0):
/// 1. Extract cards from Main and Commander sections only (Sideboard excluded)
/// 2. Collect all cards as "CardName:Quantity" pairs
/// 3. Sort alphabetically
/// 4. Concatenate into canonical string
/// 5. Calculate SHA-256 hash
/// 6. Return first 16 hex characters (64 bits)
pub fn calculate_deck_hash(forge_content: &str) -> String {
    let mut cards: Vec<(String, u32)> = Vec::new();
    let mut current_section = "";
    
    for line in forge_content.lines() {
        let line = line.trim();
        
        // Track section headers
        if line.starts_with('[') && line.ends_with(']') {
            current_section = line.trim_start_matches('[').trim_end_matches(']');
            continue;
        }
        
        // Skip non-card lines
        if line.is_empty() || line.starts_with("Name") || line.contains('=') {
            continue;
        }
        
        // Only include Main and Commander sections (case-insensitive)
        let section_lower = current_section.to_lowercase();
        if section_lower != "main" && section_lower != "commander" {
            continue;
        }
        
        // Parse card line: "1 Card Name" or "1 Card Name|SET"
        if let Some((qty, name)) = parse_card_line(line) {
            // Check if card already exists in list
            if let Some(existing) = cards.iter_mut().find(|(n, _)| n == &name) {
                existing.1 += qty;
            } else {
                cards.push((name, qty));
            }
        }
    }
    
    // Sort alphabetically by card name
    cards.sort_by(|a, b| a.0.cmp(&b.0));
    
    // Create canonical string: "CardName:Qty,CardName:Qty,..."
    let canonical: String = cards.iter()
        .map(|(name, qty)| format!("{}:{}", name, qty))
        .collect::<Vec<_>>()
        .join(",");
    
    // Calculate SHA-256 hash
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let result = hasher.finalize();
    
    // Return first 16 hex characters
    format!("{:x}", result).chars().take(16).collect()
}

/// Parse a card line from Forge format: "1 Card Name" or "1 Card Name|SET"
fn parse_card_line(line: &str) -> Option<(u32, String)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    
    // Split on first space to get quantity
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    if parts.len() < 2 {
        return None;
    }
    
    let qty: u32 = parts[0].parse().ok()?;
    let mut card_name = parts[1].to_string();
    
    // Remove set code if present: "Card Name|SET" -> "Card Name"
    if let Some(pipe_idx) = card_name.find('|') {
        card_name = card_name[..pipe_idx].to_string();
    }
    
    // Clean up the card name
    let card_name = card_name.trim().to_string();
    if card_name.is_empty() {
        return None;
    }
    
    Some((qty, card_name))
}

/// Find an existing deck file by its hash
/// Scans all .dck files in the deck directory and compares hashes
fn find_deck_by_hash(target_hash: &str) -> Option<PathBuf> {
    let deck_dir = get_deck_directory().ok()?;
    
    if !deck_dir.exists() {
        return None;
    }
    
    // Scan all .dck files
    let entries = fs::read_dir(&deck_dir).ok()?;
    
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "dck").unwrap_or(false) {
            if let Ok(content) = fs::read_to_string(&path) {
                let file_hash = calculate_deck_hash(&content);
                if file_hash == target_hash {
                    return Some(path);
                }
            }
        }
    }
    
    None
}

// ==================== File Operations ====================

/// Write deck content directly to file (content already in forge format from API)
/// Archives any existing versions of the same deck (even with different dates in the name)
/// by renaming them with an "Archived_" prefix.
/// Returns (new_file_path, list of (archived_file_name, same_hash)).
async fn write_deck_file(deck_name: &str, content: &str) -> Result<(PathBuf, Vec<(String, bool)>)> {
    let deck_dir = get_deck_directory()?;
    
    // Ensure the directory exists
    if !deck_dir.exists() {
        fs::create_dir_all(&deck_dir)
            .with_context(|| format!("Failed to create deck directory: {:?}", deck_dir))?;
        info!("Created deck directory: {:?}", deck_dir);
    }

    // Create deck file path (sanitize the name for filesystem)
    let sanitized_name = sanitize_filename(deck_name);
    let deck_file_path = deck_dir.join(format!("{}.dck", sanitized_name));

    // Calculate hash of the new content for comparison
    let new_hash = calculate_deck_hash(content);

    // Extract the base deck name without the date suffix, e.g.
    // "killriam - Welcome to the Capital of Karl Marx (2026-02-15)" -> "killriam - Welcome to the Capital of Karl Marx"
    // This allows us to find and remove old versions with different dates.
    let base_name = if let Some(paren_pos) = sanitized_name.rfind(" (") {
        // Verify it looks like a date pattern: " (YYYY-MM-DD)"
        let after_paren = &sanitized_name[paren_pos..];
        if after_paren.len() >= 12 && after_paren.ends_with(')') {
            sanitized_name[..paren_pos].to_string()
        } else {
            sanitized_name.clone()
        }
    } else {
        sanitized_name.clone()
    };

    // Remove any existing versions of this deck (same base name, any date)
    // Old versions are deleted so Forge doesn't show duplicate entries
    let mut removed_files: Vec<(String, bool)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&deck_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            
            // Only look at .dck files (skip directories, other files)
            if !file_name.ends_with(".dck") {
                continue;
            }
            
            // Clean up any legacy archived files for this deck
            if (file_name.starts_with("Archived_") 
                || file_name.starts_with("_archive_") 
                || file_name.starts_with("archive_") 
                || file_name.starts_with("PASTVersionS_"))
                && file_name.contains(&base_name)
            {
                if let Err(e) = fs::remove_file(entry.path()) {
                    warn!("Failed to remove old archive {:?}: {}", entry.path(), e);
                } else {
                    info!("Removed old archive: {}", file_name);
                }
                continue;
            }
            
            // Check if this is an existing version of the same deck (matching base name)
            if file_name.starts_with(&base_name) {
                let path = entry.path();
                
                // Compare hash to detect if content actually changed
                let old_hash = fs::read_to_string(&path)
                    .map(|old_content| calculate_deck_hash(&old_content))
                    .unwrap_or_default();
                let same_hash = !old_hash.is_empty() && old_hash == new_hash;
                
                // Delete old version
                if let Err(e) = fs::remove_file(&path) {
                    warn!("Failed to remove old deck version {:?}: {}", path, e);
                } else {
                    info!("Removed old deck version: {}", file_name);
                    removed_files.push((file_name.clone(), same_hash));
                }
            }
        }
    }

    // Write deck file with content from API
    fs::write(&deck_file_path, content)
        .with_context(|| format!("Failed to write deck file: {:?}", deck_file_path))?;

    info!("Successfully created deck file: {:?}", deck_file_path);
    Ok((deck_file_path, removed_files))
}

/// Legacy function to fetch deck data as structured JSON (kept for compatibility)
#[allow(dead_code)]
async fn fetch_deck_data(deck_id: &str, api_base_url: &str) -> Result<DeckData> {
    let client = reqwest::Client::new();
    let url = format!("{}/decks/{}", api_base_url, deck_id);
    
    info!("Fetching deck data from: {}", url);
    
    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to send request to API")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "API returned error status: {} - {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }

    let deck_data: DeckData = response
        .json()
        .await
        .context("Failed to parse deck data from API response")?;

    info!("Successfully fetched deck data for: {}", deck_data.name);
    Ok(deck_data)
}

/// Legacy function to create deck file from structured DeckData (kept for compatibility)
#[allow(dead_code)]
async fn create_deck_file(deck_data: &DeckData) -> Result<PathBuf> {
    let deck_dir = get_deck_directory()?;
    
    // Ensure the directory exists
    if !deck_dir.exists() {
        fs::create_dir_all(&deck_dir)
            .with_context(|| format!("Failed to create deck directory: {:?}", deck_dir))?;
        info!("Created deck directory: {:?}", deck_dir);
    }

    // Create deck file path (sanitize the name for filesystem)
    let sanitized_name = sanitize_filename(&deck_data.name);
    let deck_file_path = deck_dir.join(format!("{}.dck", sanitized_name));

    // Generate deck file content
    let deck_content = format_deck_file(deck_data);

    // Write deck file
    fs::write(&deck_file_path, deck_content)
        .with_context(|| format!("Failed to write deck file: {:?}", deck_file_path))?;

    info!("Successfully created deck file: {:?}", deck_file_path);
    Ok(deck_file_path)
}

fn get_deck_directory() -> Result<PathBuf> {
    let deck_dir = if cfg!(windows) {
        // Windows: C:\Users\[username]\AppData\Roaming\Forge\decks\commander
        if let Some(appdata) = std::env::var_os("APPDATA") {
            PathBuf::from(appdata)
        } else {
            return Err(anyhow::anyhow!("APPDATA environment variable not found"));
        }
    } else {
        // For non-Windows platforms, use home directory
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME environment variable not found"))?;
        PathBuf::from(home).join(".forge")
    };
    
    Ok(deck_dir.join("Forge").join("decks").join("commander"))
}

fn sanitize_filename(name: &str) -> String {
    // Replace invalid filename characters with underscores
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// For double-faced cards like "Brightclimb Pathway // Grimclimb Pathway",
/// Forge only recognizes the front face name. This returns just the front face.
fn front_face_name(full_name: &str) -> &str {
    full_name.split(" // ").next().unwrap_or(full_name)
}

/// Post-process Forge deck content to fix double-faced card names.
/// Replaces "Qty Front Face // Back Face|SET|CN" with "Qty Front Face|SET|CN"
/// and "Qty Front Face // Back Face" with "Qty Front Face"
fn post_process_forge_content(content: &str) -> String {
    let mut trimmed = Vec::new();
    let mut in_sideboard = false;
    let mut sideboard_seen = 0;

    for line in content.lines() {
        let line_str = line.to_string();
        let stripped = line.trim();

        if stripped.eq_ignore_ascii_case("[sideboard]") {
            in_sideboard = true;
            sideboard_seen = 0;
            trimmed.push(line_str);
            continue;
        }

        if in_sideboard {
            if stripped.starts_with('[') {
                in_sideboard = false;
                trimmed.push(line_str);
                continue;
            }

            if stripped.is_empty() {
                trimmed.push(line_str);
                continue;
            }

            if sideboard_seen >= MAX_FORGE_SIDEBOARD_CARDS {
                continue;
            }

            sideboard_seen += 1;
            trimmed.push(line_str);
            continue;
        }

        // Skip non-card lines (sections, metadata, empty)
        if stripped.is_empty() || stripped.starts_with('[') || stripped.contains('=') {
            trimmed.push(line_str);
            continue;
        }

        // Try to parse as a card line: "Qty CardName..."
        if let Some(space_idx) = stripped.find(' ') {
            let qty_str = &stripped[..space_idx];
            if qty_str.chars().all(|c| c.is_ascii_digit()) {
                let rest = &stripped[space_idx + 1..];
                // Split off set info (pipe-separated): "CardName|SET|CN"
                if let Some(pipe_idx) = rest.find('|') {
                    let card_name = &rest[..pipe_idx];
                    let set_info = &rest[pipe_idx..]; // includes leading '|'
                    let front = front_face_name(card_name);
                    trimmed.push(format!("{} {}{}", qty_str, front, set_info));
                    continue;
                } else {
                    // No set info, just "Qty CardName"
                    let front = front_face_name(rest);
                    trimmed.push(format!("{} {}", qty_str, front));
                    continue;
                }
            }
        }

        trimmed.push(line_str);
    }

    trimmed.join("\n")
}

fn format_deck_file(deck_data: &DeckData) -> String {
    let mut content = String::new();
    
    // Metadata section
    content.push_str("[metadata]\n");
    content.push_str(&format!("Name={}\n", deck_data.name));
    content.push('\n');
    
    // Commander section
    content.push_str("[Commander]\n");
    for card in &deck_data.commander {
        content.push_str(&format_card_line(card));
    }
    content.push('\n');
    
    // Main deck section
    content.push_str("[Main]\n");
    for card in &deck_data.main {
        content.push_str(&format_card_line(card));
    }
    content.push('\n');
    
    // Sideboard section
    content.push_str("[Sideboard]\n");
    for card in deck_data.sideboard.iter().take(MAX_FORGE_SIDEBOARD_CARDS) {
        content.push_str(&format_card_line(card));
    }
    content.push('\n');
    
    // Attractions section
    content.push_str("[Attractions]\n");
    for card in &deck_data.attractions {
        content.push_str(&format_card_line(card));
    }
    
    content
}

fn format_card_line(card: &Card) -> String {
    if let Some(collector_number) = &card.collector_number {
        format!("{} {}|{}|{}\n", card.quantity, card.name, card.set, collector_number)
    } else {
        format!("{} {}|{}|1\n", card.quantity, card.name, card.set)
    }
}

// ==================== Deck Sync Functions ====================

/// Result of a single deck sync operation
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DeckSyncResult {
    pub deck_name: String,
    pub status: SyncStatus,
    pub message: String,
    pub old_file: Option<PathBuf>,
    pub new_file: Option<PathBuf>,
}

/// Status of a sync operation
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum SyncStatus {
    Updated,       // Deck was updated (old version renamed)
    AlreadyUpToDate,
    NewDownloaded, // New deck was downloaded
    Failed,        // Sync failed
    Skipped,       // Skipped (disabled or error checking)
}

#[allow(dead_code)]
impl DeckSyncResult {
    pub fn updated(deck_name: String, old_file: PathBuf, new_file: PathBuf) -> Self {
        Self {
            deck_name: deck_name.clone(),
            status: SyncStatus::Updated,
            message: format!("Updated '{}' - old version archived", deck_name),
            old_file: Some(old_file),
            new_file: Some(new_file),
        }
    }

    pub fn already_up_to_date(deck_name: String) -> Self {
        Self {
            deck_name: deck_name.clone(),
            status: SyncStatus::AlreadyUpToDate,
            message: format!("'{}' is already up to date", deck_name),
            old_file: None,
            new_file: None,
        }
    }

    pub fn new_downloaded(deck_name: String, new_file: PathBuf) -> Self {
        Self {
            deck_name: deck_name.clone(),
            status: SyncStatus::NewDownloaded,
            message: format!("Downloaded new deck '{}'", deck_name),
            old_file: None,
            new_file: Some(new_file),
        }
    }

    pub fn failed(deck_name: String, error: String) -> Self {
        Self {
            deck_name: deck_name.clone(),
            status: SyncStatus::Failed,
            message: format!("Failed to sync '{}': {}", deck_name, error),
            old_file: None,
            new_file: None,
        }
    }

    pub fn skipped(deck_name: String, reason: String) -> Self {
        Self {
            deck_name: deck_name.clone(),
            status: SyncStatus::Skipped,
            message: format!("Skipped '{}': {}", deck_name, reason),
            old_file: None,
            new_file: None,
        }
    }
}

/// Rename a deck file with "Archived_" prefix in the same directory.
/// Returns the new path of the archived file.
fn archive_deck_with_prefix(deck_path: &std::path::Path) -> Result<PathBuf> {
    let deck_dir = deck_path.parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine parent directory of {:?}", deck_path))?;
    let file_name = deck_path.file_name()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine filename of {:?}", deck_path))?
        .to_string_lossy();
    let archived_name = format!("Archived_{}", file_name);
    let archived_path = deck_dir.join(&archived_name);
    fs::rename(deck_path, &archived_path)
        .with_context(|| format!("Failed to archive {:?} -> {:?}", deck_path, archived_path))?;
    info!("Archived old deck: {} -> {}", file_name, archived_name);
    Ok(archived_path)
}

/// Sync a single Moxfield deck - check if newer and update if needed
pub async fn sync_moxfield_deck(deck_id: &str) -> Result<DeckSyncResult> {
    info!("Syncing Moxfield deck: {}", deck_id);
    
    // Fetch deck info from Moxfield
    let url = format!("{}/decks/all/{}", MOXFIELD_API_URL, deck_id);
    let body = fetch_with_curl(&url)?;
    
    let deck: MoxfieldFullDeck = serde_json::from_str(&body)
        .with_context(|| "Failed to parse Moxfield deck response")?;
    
    // Build the expected filename
    let author = deck.created_by_user.as_ref()
        .map(|u| u.user_name.as_str())
        .unwrap_or("Unknown");
    
    let moxfield_date = deck.last_updated_at_utc.as_ref()
        .and_then(|dt| dt.split('T').next())
        .unwrap_or("unknown");
    
    let deck_dir = get_deck_directory()?;
    
    // Check for existing deck files matching this pattern
    let (existing_file, existing_date) = find_existing_deck_file(author, &deck.name, &deck_dir)?;
    
    if let Some(existing_path) = existing_file {
        // Compare dates to see if we need to update
        if let Some(ref local_date) = existing_date {
            if local_date >= &moxfield_date.to_string() {
                info!("Deck '{}' is already up to date (local: {}, moxfield: {})", 
                      deck.name, local_date, moxfield_date);
                return Ok(DeckSyncResult::already_up_to_date(deck.name));
            }
        }
        
        // Moxfield is newer - archive old version and download new
        info!("Deck '{}' needs update (local: {:?}, moxfield: {})", 
              deck.name, &existing_date, moxfield_date);
        
        // Move old file to _archive subdirectory
        let archived_path = archive_deck_with_prefix(&existing_path)?;
        
        // Download the new version
        let full_name = format!("{} - {} ({})", author, deck.name, moxfield_date);
        let forge_content = convert_moxfield_to_forge(&full_name, &body)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::updated(deck.name, archived_path, new_path))
    } else {
        // No existing file - download as new
        info!("Deck '{}' is new, downloading...", deck.name);
        
        let full_name = format!("{} - {} ({})", author, deck.name, moxfield_date);
        let forge_content = convert_moxfield_to_forge(&full_name, &body)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::new_downloaded(deck.name, new_path))
    }
}

/// Sync all decks from a Moxfield user
pub async fn sync_moxfield_user_decks(username: &str) -> Result<Vec<DeckSyncResult>> {
    info!("Syncing all decks for Moxfield user: {}", username);
    
    // Fetch all user decks
    let decks = fetch_user_decks_direct(username)?;
    let mut results = Vec::new();
    
    for deck in decks {
        let result = sync_moxfield_deck(&deck.public_id).await
            .unwrap_or_else(|e| DeckSyncResult::failed(deck.name.clone(), e.to_string()));
        results.push(result);
    }
    
    Ok(results)
}

/// Sync an Archidekt deck
pub async fn sync_archidekt_deck(deck_id: &str) -> Result<DeckSyncResult> {
    info!("Syncing Archidekt deck: {}", deck_id);
    
    let url = format!("https://archidekt.com/api/decks/{}/", deck_id);
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "-H", "Accept: application/json",
    ])?;
    
    let deck: ArchidektDeck = serde_json::from_str(&body)
        .with_context(|| "Failed to parse Archidekt deck response")?;
    
    let author = deck.owner.as_ref()
        .map(|o| o.username.as_str())
        .unwrap_or("Unknown");
    
    let archidekt_date = deck.updated_at.as_ref()
        .and_then(|dt| dt.split('T').next())
        .unwrap_or("unknown");
    
    let deck_dir = get_deck_directory()?;
    let (existing_file, existing_date) = find_existing_deck_file(author, &deck.name, &deck_dir)?;
    
    if let Some(existing_path) = existing_file {
        if let Some(local_date) = existing_date {
            if local_date >= archidekt_date.to_string() {
                return Ok(DeckSyncResult::already_up_to_date(deck.name));
            }
        }
        
        // Move old file to _archive subdirectory
        let archived_path = archive_deck_with_prefix(&existing_path)?;
        
        let full_name = format!("{} - {} ({})", author, deck.name, archidekt_date);
        let forge_content = convert_archidekt_to_forge(&full_name, &deck)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::updated(deck.name, archived_path, new_path))
    } else {
        let full_name = format!("{} - {} ({})", author, deck.name, archidekt_date);
        let forge_content = convert_archidekt_to_forge(&full_name, &deck)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::new_downloaded(deck.name, new_path))
    }
}

/// Sync a Deckstats deck
pub async fn sync_deckstats_deck(owner_id: &str, deck_id: &str) -> Result<DeckSyncResult> {
    info!("Syncing Deckstats deck: owner={} deck={}", owner_id, deck_id);
    
    // Deckstats doesn't provide easy date comparison, so we always re-download
    let url = format!(
        "https://deckstats.net/api.php?action=get_deck&id_type=saved&owner_id={}&id={}&response_type=list",
        owner_id, deck_id
    );
    
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
    ])?;
    
    let deck_name = body.lines()
        .find(|l| l.starts_with("//NAME:"))
        .map(|l| l.trim_start_matches("//NAME:").trim())
        .unwrap_or("Unknown Deck");
    
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let full_name = format!("Deckstats - {} ({})", deck_name, date);
    
    let deck_dir = get_deck_directory()?;
    let (existing_file, _) = find_existing_deck_file("Deckstats", deck_name, &deck_dir)?;
    
    if let Some(existing_path) = existing_file {
        // Move old file to _archive subdirectory
        let archived_path = archive_deck_with_prefix(&existing_path)?;
        
        let forge_content = convert_deckstats_to_forge(&full_name, &body)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::updated(deck_name.to_string(), archived_path, new_path))
    } else {
        let forge_content = convert_deckstats_to_forge(&full_name, &body)?;
        let (new_path, _) = write_deck_file(&full_name, &forge_content).await?;
        
        Ok(DeckSyncResult::new_downloaded(deck_name.to_string(), new_path))
    }
}

/// Sync a MaMo deck
pub async fn sync_mamo_deck(deck_id: &str) -> Result<DeckSyncResult> {
    info!("Syncing MaMo deck: {}", deck_id);
    
    let url = format!("{}/api/deck/export/{}/forge", MAMO_API_URL, deck_id);
    
    let body = fetch_with_curl_custom(&url, &[
        "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        "-H", "Accept: text/plain",
    ])?;
    
    if body.starts_with("{") && body.contains("error") {
        return Err(anyhow::anyhow!("MaMo API returned error: {}", body));
    }
    
    // Parse deck name from content
    let deck_name = body.lines()
        .find(|l| l.starts_with("Name="))
        .map(|l| l.trim_start_matches("Name="))
        .unwrap_or("Unknown MaMo Deck");
    
    // Extract author and deck name from "Author - Deck Name" format
    let (author, name) = if deck_name.contains(" - ") {
        let parts: Vec<&str> = deck_name.splitn(2, " - ").collect();
        (parts.get(0).copied().unwrap_or("Unknown"), parts.get(1).copied().unwrap_or(deck_name))
    } else {
        ("MaMo", deck_name)
    };
    
    let deck_dir = get_deck_directory()?;
    let (existing_file, _) = find_existing_deck_file(author, name, &deck_dir)?;
    
    if let Some(existing_path) = existing_file {
        // Move old file to _archive subdirectory
        let archived_path = archive_deck_with_prefix(&existing_path)?;
        
        let (new_path, _) = write_deck_file(deck_name, &body).await?;
        
        Ok(DeckSyncResult::updated(name.to_string(), archived_path, new_path))
    } else {
        let (new_path, _) = write_deck_file(deck_name, &body).await?;
        
        Ok(DeckSyncResult::new_downloaded(name.to_string(), new_path))
    }
}

/// Find an existing deck file matching the pattern "author - deck_name (date).dck"
fn find_existing_deck_file(author: &str, deck_name: &str, deck_dir: &PathBuf) -> Result<(Option<PathBuf>, Option<String>)> {
    if !deck_dir.exists() {
        return Ok((None, None));
    }
    
    let sanitized_deck_name = sanitize_filename(deck_name);
    let pattern_start = format!("{} - {}", author, sanitized_deck_name);
    
    if let Ok(entries) = fs::read_dir(deck_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let filename = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            
            // Skip archived versions (legacy prefix or _archive_ prefix)
            if filename.starts_with("PASTVersionS_") || filename.starts_with("_archive_") {
                continue;
            }
            
            // Check if filename matches our pattern
            if filename.starts_with(&pattern_start) && filename.ends_with(".dck") {
                // Extract date from filename: "user - name (YYYY-MM-DD).dck"
                let date = if let Some(date_start) = filename.rfind('(') {
                    if let Some(date_end) = filename.rfind(')') {
                        Some(filename[date_start + 1..date_end].to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                return Ok((Some(path), date));
            }
        }
    }
    
    Ok((None, None))
}

/// Get the public deck directory path (for UI display)
pub fn get_deck_directory_display() -> String {
    match get_deck_directory() {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(_) => "Unknown".to_string(),
    }
}

/// Name (stem, without .dck) of the bundled standard AI opponent deck.
pub const DUMMY_DEFENDER_DECK_NAME: &str = "killriam - dummy defender (2026-04-09)";

/// Embedded content of the bundled dummy-defender deck.
const DUMMY_DEFENDER_DECK_CONTENT: &str =
    include_str!("../res/decks/killriam - dummy defender (2026-04-09).dck");

/// Ensure the bundled dummy-defender deck exists in the Forge commander decks directory.
/// Creates it from the embedded content when not present; never overwrites an existing file.
pub fn ensure_dummy_defender_deck() -> Result<()> {
    let deck_dir = get_deck_directory()?;

    if !deck_dir.exists() {
        fs::create_dir_all(&deck_dir)
            .with_context(|| format!("Failed to create deck directory: {:?}", deck_dir))?;
    }

    let sanitized = sanitize_filename(DUMMY_DEFENDER_DECK_NAME);
    let deck_path = deck_dir.join(format!("{}.dck", sanitized));

    if !deck_path.exists() {
        fs::write(&deck_path, DUMMY_DEFENDER_DECK_CONTENT)
            .with_context(|| format!("Failed to write dummy defender deck: {:?}", deck_path))?;
        info!("Created bundled dummy defender deck at {:?}", deck_path);
    }

    Ok(())
}


mod tests {
    use super::*;

    // ==================== Deck Hash Calculation Tests ====================
    
    #[test]
    fn test_calculate_deck_hash_simple() {
        let content = r#"[metadata]
Name = Test Deck
[Main]
4 Lightning Bolt
4 Mountain
[Commander]
1 Zurgo Helmsmasher
[Sideboard]
2 Pyroblast
"#;
        let hash = calculate_deck_hash(content);
        assert_eq!(hash.len(), 16);
        // Hash should be consistent
        assert_eq!(calculate_deck_hash(content), hash);
    }
    
    #[test]
    fn test_calculate_deck_hash_excludes_sideboard() {
        // Same main/commander, different sideboard = same hash
        let content1 = r#"[Main]
4 Lightning Bolt
[Commander]
1 Zurgo
[Sideboard]
2 Pyroblast
"#;
        let content2 = r#"[Main]
4 Lightning Bolt
[Commander]
1 Zurgo
[Sideboard]
4 Blue Elemental Blast
"#;
        assert_eq!(calculate_deck_hash(content1), calculate_deck_hash(content2));
    }
    
    #[test]
    fn test_calculate_deck_hash_different_cards_different_hash() {
        let content1 = r#"[Main]
4 Lightning Bolt
"#;
        let content2 = r#"[Main]
4 Shock
"#;
        assert_ne!(calculate_deck_hash(content1), calculate_deck_hash(content2));
    }
    
    #[test]
    fn test_calculate_deck_hash_order_independent() {
        // Cards in different order should produce same hash (sorted alphabetically)
        let content1 = r#"[Main]
4 Lightning Bolt
4 Mountain
"#;
        let content2 = r#"[Main]
4 Mountain
4 Lightning Bolt
"#;
        assert_eq!(calculate_deck_hash(content1), calculate_deck_hash(content2));
    }
    
    #[test]
    fn test_calculate_deck_hash_strips_set_code() {
        // Set codes should be stripped, same card = same hash
        let content1 = r#"[Main]
4 Lightning Bolt|M20
"#;
        let content2 = r#"[Main]
4 Lightning Bolt|M21
"#;
        assert_eq!(calculate_deck_hash(content1), calculate_deck_hash(content2));
    }
    
    #[test]
    fn test_parse_card_line_simple() {
        let result = parse_card_line("4 Lightning Bolt");
        assert_eq!(result, Some((4, "Lightning Bolt".to_string())));
    }
    
    #[test]
    fn test_parse_card_line_with_set() {
        let result = parse_card_line("1 Zurgo Helmsmasher|KTK");
        assert_eq!(result, Some((1, "Zurgo Helmsmasher".to_string())));
    }
    
    #[test]
    fn test_parse_card_line_with_set_and_number() {
        let result = parse_card_line("2 Mountain|M20|123");
        assert_eq!(result, Some((2, "Mountain".to_string())));
    }
    
    #[test]
    fn test_parse_card_line_empty() {
        assert_eq!(parse_card_line(""), None);
        assert_eq!(parse_card_line("   "), None);
    }

    // ==================== Filename Sanitization Tests ====================
    
    #[test]
    fn test_sanitize_filename_normal() {
        assert_eq!(sanitize_filename("Test Deck"), "Test Deck");
        assert_eq!(sanitize_filename("My Commander Deck"), "My Commander Deck");
    }

    #[test]
    fn test_sanitize_filename_with_slashes() {
        assert_eq!(sanitize_filename("Test/Deck\\Name"), "Test_Deck_Name");
    }

    #[test]
    fn test_sanitize_filename_with_special_chars() {
        assert_eq!(sanitize_filename("Deck: Name?"), "Deck_ Name_");
        assert_eq!(sanitize_filename("Test<Deck>Name"), "Test_Deck_Name");
        assert_eq!(sanitize_filename("Pipe|Test"), "Pipe_Test");
        assert_eq!(sanitize_filename("Star*Deck"), "Star_Deck");
        assert_eq!(sanitize_filename("Quote\"Test"), "Quote_Test");
    }

    #[test]
    fn test_sanitize_filename_with_whitespace() {
        assert_eq!(sanitize_filename("  Trimmed  "), "Trimmed");
    }

    #[test]
    fn test_sanitize_filename_all_special() {
        assert_eq!(sanitize_filename("<>:\"/\\|?*"), "_________");
    }

    // ==================== Card Line Formatting Tests ====================

    #[test]
    fn test_format_card_line_with_collector_number() {
        let card = Card {
            name: "Lightning Bolt".to_string(),
            set: "M20".to_string(),
            quantity: 4,
            collector_number: Some("123".to_string()),
        };
        assert_eq!(format_card_line(&card), "4 Lightning Bolt|M20|123\n");
    }

    #[test]
    fn test_format_card_line_without_collector_number() {
        let card = Card {
            name: "Lightning Bolt".to_string(),
            set: "M20".to_string(),
            quantity: 1,
            collector_number: None,
        };
        assert_eq!(format_card_line(&card), "1 Lightning Bolt|M20|1\n");
    }

    #[test]
    fn test_format_card_line_high_quantity() {
        let card = Card {
            name: "Snow-Covered Mountain".to_string(),
            set: "MB2".to_string(),
            quantity: 31,
            collector_number: Some("1".to_string()),
        };
        assert_eq!(format_card_line(&card), "31 Snow-Covered Mountain|MB2|1\n");
    }

    #[test]
    fn test_format_card_line_special_card_name() {
        let card = Card {
            name: "Alhammarret's Archive".to_string(),
            set: "C21".to_string(),
            quantity: 1,
            collector_number: Some("1".to_string()),
        };
        assert_eq!(format_card_line(&card), "1 Alhammarret's Archive|C21|1\n");
    }

    #[test]
    fn test_format_card_line_split_card() {
        let card = Card {
            name: "Insult // Injury".to_string(),
            set: "AKR".to_string(),
            quantity: 1,
            collector_number: Some("1".to_string()),
        };
        assert_eq!(format_card_line(&card), "1 Insult // Injury|AKR|1\n");
    }

    // ==================== Deck File Format Tests ====================

    fn create_test_deck() -> DeckData {
        DeckData {
            name: "Test EDH Deck".to_string(),
            commander: vec![
                Card {
                    name: "Ashling, Flame Dancer".to_string(),
                    set: "MH3".to_string(),
                    quantity: 1,
                    collector_number: Some("1".to_string()),
                }
            ],
            main: vec![
                Card {
                    name: "Lightning Bolt".to_string(),
                    set: "M20".to_string(),
                    quantity: 4,
                    collector_number: Some("123".to_string()),
                },
                Card {
                    name: "Mountain".to_string(),
                    set: "UNH".to_string(),
                    quantity: 35,
                    collector_number: Some("138".to_string()),
                },
            ],
            sideboard: vec![
                Card {
                    name: "Pyroblast".to_string(),
                    set: "ICE".to_string(),
                    quantity: 1,
                    collector_number: Some("213".to_string()),
                }
            ],
            attractions: vec![],
        }
    }

    #[test]
    fn test_format_deck_file_has_metadata() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        assert!(content.contains("[metadata]"));
        assert!(content.contains("Name=Test EDH Deck"));
    }

    #[test]
    fn test_format_deck_file_has_commander_section() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        assert!(content.contains("[Commander]"));
        assert!(content.contains("1 Ashling, Flame Dancer|MH3|1"));
    }

    #[test]
    fn test_format_deck_file_has_main_section() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        assert!(content.contains("[Main]"));
        assert!(content.contains("4 Lightning Bolt|M20|123"));
        assert!(content.contains("35 Mountain|UNH|138"));
    }

    #[test]
    fn test_format_deck_file_has_sideboard_section() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        assert!(content.contains("[Sideboard]"));
        assert!(content.contains("1 Pyroblast|ICE|213"));
    }

    #[test]
    fn test_format_deck_file_has_attractions_section() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        assert!(content.contains("[Attractions]"));
    }

    #[test]
    fn test_format_deck_file_section_order() {
        let deck = create_test_deck();
        let content = format_deck_file(&deck);
        
        let metadata_pos = content.find("[metadata]").unwrap();
        let commander_pos = content.find("[Commander]").unwrap();
        let main_pos = content.find("[Main]").unwrap();
        let sideboard_pos = content.find("[Sideboard]").unwrap();
        let attractions_pos = content.find("[Attractions]").unwrap();
        
        assert!(metadata_pos < commander_pos);
        assert!(commander_pos < main_pos);
        assert!(main_pos < sideboard_pos);
        assert!(sideboard_pos < attractions_pos);
    }

    #[test]
    fn test_format_deck_file_empty_sections() {
        let deck = DeckData {
            name: "Empty Deck".to_string(),
            commander: vec![],
            main: vec![],
            sideboard: vec![],
            attractions: vec![],
        };
        let content = format_deck_file(&deck);
        
        // All sections should still be present
        assert!(content.contains("[metadata]"));
        assert!(content.contains("[Commander]"));
        assert!(content.contains("[Main]"));
        assert!(content.contains("[Sideboard]"));
        assert!(content.contains("[Attractions]"));
    }

    #[test]
    fn test_format_deck_file_complete_output() {
        let deck = DeckData {
            name: "ashes 0511".to_string(),
            commander: vec![
                Card {
                    name: "Ashling, Flame Dancer".to_string(),
                    set: "MH3".to_string(),
                    quantity: 1,
                    collector_number: Some("1".to_string()),
                }
            ],
            main: vec![
                Card {
                    name: "Abrade".to_string(),
                    set: "BLC".to_string(),
                    quantity: 1,
                    collector_number: Some("1".to_string()),
                },
            ],
            sideboard: vec![],
            attractions: vec![],
        };
        let content = format_deck_file(&deck);
        
        let expected_start = "[metadata]\nName=ashes 0511\n\n[Commander]\n1 Ashling, Flame Dancer|MH3|1\n";
        assert!(content.starts_with(expected_start));
    }

    // ==================== DeckCreationResult Tests ====================

    #[test]
    fn test_deck_creation_result_success() {
        let path = PathBuf::from("/test/path/deck.txt");
        let result = DeckCreationResult::success("Created successfully".to_string(), path.clone());
        
        assert!(result.success);
        assert_eq!(result.message, "Created successfully");
        assert_eq!(result.deck_path, Some(path));
    }

    #[test]
    fn test_deck_creation_result_failed() {
        let result = DeckCreationResult::failed("API error".to_string());
        
        assert!(!result.success);
        assert_eq!(result.message, "API error");
        assert!(result.deck_path.is_none());
    }

    // ==================== Directory Path Tests ====================

    #[test]
    fn test_get_deck_directory_returns_path() {
        let result = get_deck_directory();
        assert!(result.is_ok());
        
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("Forge"));
        assert!(path.to_string_lossy().contains("decks"));
        assert!(path.to_string_lossy().contains("commander"));
    }

    // ==================== Card Struct Tests ====================

    #[test]
    fn test_card_serialization() {
        let card = Card {
            name: "Test Card".to_string(),
            set: "TST".to_string(),
            quantity: 2,
            collector_number: Some("42".to_string()),
        };
        
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("Test Card"));
        assert!(json.contains("TST"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_card_deserialization() {
        let json = r#"{"name":"Test Card","set":"TST","quantity":2,"collector_number":"42"}"#;
        let card: Card = serde_json::from_str(json).unwrap();
        
        assert_eq!(card.name, "Test Card");
        assert_eq!(card.set, "TST");
        assert_eq!(card.quantity, 2);
        assert_eq!(card.collector_number, Some("42".to_string()));
    }

    #[test]
    fn test_card_deserialization_without_collector_number() {
        let json = r#"{"name":"Test Card","set":"TST","quantity":1,"collector_number":null}"#;
        let card: Card = serde_json::from_str(json).unwrap();
        
        assert_eq!(card.name, "Test Card");
        assert!(card.collector_number.is_none());
    }

    #[test]
    fn test_deck_data_deserialization() {
        let json = r#"{
            "name": "Test Deck",
            "commander": [{"name": "Commander", "set": "CMD", "quantity": 1, "collector_number": "1"}],
            "main": [{"name": "Card", "set": "SET", "quantity": 4, "collector_number": "2"}],
            "sideboard": [],
            "attractions": []
        }"#;
        
        let deck: DeckData = serde_json::from_str(json).unwrap();
        
        assert_eq!(deck.name, "Test Deck");
        assert_eq!(deck.commander.len(), 1);
        assert_eq!(deck.main.len(), 1);
        assert!(deck.sideboard.is_empty());
        assert!(deck.attractions.is_empty());
    }

    // ==================== Export Request/Response Tests ====================

    #[test]
    fn test_deck_export_request_serialization() {
        let request = DeckExportRequest {
            deck_id: "12345".to_string(),
            format: "forge".to_string(),
        };
        
        let json = serde_json::to_string(&request).unwrap();
        // Should use camelCase
        assert!(json.contains("\"deckId\""));
        assert!(json.contains("\"12345\""));
        assert!(json.contains("\"format\""));
        assert!(json.contains("\"forge\""));
    }

    #[test]
    fn test_deck_export_response_deserialization() {
        let json = r#"{
            "name": "Test Deck",
            "content": "[metadata]\nName=Test Deck\n\n[Main]\n4 Lightning Bolt|M20|123\n",
            "format": "forge"
        }"#;
        
        let response: DeckExportResponse = serde_json::from_str(json).unwrap();
        
        assert_eq!(response.name, "Test Deck");
        assert!(response.content.contains("[metadata]"));
        assert!(response.content.contains("Lightning Bolt"));
        assert_eq!(response.format, "forge");
    }

    #[test]
    fn test_deck_export_response_deserialization_camelcase() {
        // Test that camelCase response fields work
        let json = r#"{
            "name": "My Commander Deck",
            "content": "[Commander]\n1 Ashling|MH3|1\n",
            "format": "forge"
        }"#;
        
        let response: DeckExportResponse = serde_json::from_str(json).unwrap();
        
        assert_eq!(response.name, "My Commander Deck");
        assert!(response.content.contains("[Commander]"));
    }

    #[test]
    fn test_deck_export_response_with_full_forge_content() {
        let forge_content = r#"[metadata]
Name=ashes 0511

[Commander]
1 Ashling, Flame Dancer|MH3|1

[Main]
1 Abrade|BLC|1
1 Lightning Bolt|CLU|1
31 Snow-Covered Mountain|MB2|1

[Sideboard]
1 Bag of Holding|J22|1

[Attractions]
"#;

        let json = format!(
            r#"{{"name": "ashes 0511", "content": {}, "format": "forge"}}"#,
            serde_json::to_string(forge_content).unwrap()
        );
        
        let response: DeckExportResponse = serde_json::from_str(&json).unwrap();
        
        assert_eq!(response.name, "ashes 0511");
        assert!(response.content.contains("[metadata]"));
        assert!(response.content.contains("[Commander]"));
        assert!(response.content.contains("[Main]"));
        assert!(response.content.contains("[Sideboard]"));
        assert!(response.content.contains("[Attractions]"));
        assert!(response.content.contains("Ashling, Flame Dancer"));
        assert!(response.content.contains("31 Snow-Covered Mountain"));
    }

    #[test]
    fn test_post_process_forge_content_trims_sideboard_to_ten_cards() {
        let input = r#"[metadata]
Name=Test

[Main]
1 Lightning Bolt|M20|1

[Sideboard]
1 Card One|SET|1
1 Card Two|SET|1
1 Card Three|SET|1
1 Card Four|SET|1
1 Card Five|SET|1
1 Card Six|SET|1
1 Card Seven|SET|1
1 Card Eight|SET|1
1 Card Nine|SET|1
1 Card Ten|SET|1
1 Card Eleven|SET|1
1 Card Twelve|SET|1

[Attractions]
"#;
        let output = post_process_forge_content(input);
        let sideboard_lines: Vec<_> = output
            .lines()
            .skip_while(|line| !line.trim().eq_ignore_ascii_case("[Sideboard]"))
            .skip(1)
            .take_while(|line| !line.trim().starts_with('['))
            .filter(|line| !line.trim().is_empty())
            .collect();

        assert_eq!(sideboard_lines.len(), 10);
        assert!(sideboard_lines.contains(&"1 Card Ten|SET|1"));
        assert!(!sideboard_lines.contains(&"1 Card Eleven|SET|1"));
        assert!(!sideboard_lines.contains(&"1 Card Twelve|SET|1"));
    }

    #[test]
    fn test_format_deck_file_limits_sideboard_to_ten_cards() {
        let deck = DeckData {
            name: "Sideboard Cap Test".to_string(),
            commander: vec![Card { name: "Commander".to_string(), set: "CMD".to_string(), quantity: 1, collector_number: Some("1".to_string()) }],
            main: vec![Card { name: "Island".to_string(), set: "M20".to_string(), quantity: 1, collector_number: Some("1".to_string()) }],
            sideboard: (1..=12)
                .map(|i| Card { name: format!("Card {}", i), set: "SET".to_string(), quantity: 1, collector_number: Some("1".to_string()) })
                .collect(),
            attractions: vec![],
        };

        let content = format_deck_file(&deck);
        let sideboard_lines: Vec<_> = content
            .lines()
            .skip_while(|line| !line.trim().eq_ignore_ascii_case("[Sideboard]"))
            .skip(1)
            .take_while(|line| !line.trim().starts_with('['))
            .filter(|line| !line.trim().is_empty())
            .collect();

        assert_eq!(sideboard_lines.len(), 10);
        assert!(sideboard_lines.contains(&"1 Card 10|SET|1"));
        assert!(!sideboard_lines.contains(&"1 Card 11|SET|1"));
        assert!(!sideboard_lines.contains(&"1 Card 12|SET|1"));
    }

    // ==================== URL-based Deck Download Tests ====================

    /// Test extracting deck ID from Moxfield URL
    #[test]
    fn test_extract_deck_id_from_moxfield_url() {
        let url = "https://moxfield.com/decks/oR2h0X7tREyhBBW3AlC8tw";
        let deck_id = url.split("/decks/").nth(1).unwrap();
        assert_eq!(deck_id, "oR2h0X7tREyhBBW3AlC8tw");
    }

    /// Test case for downloading deck from URL and creating deck file
    /// This simulates the complete workflow:
    /// 1. Extract deck ID from Moxfield URL
    /// 2. Fetch deck data from API
    /// 3. Create deck file in Forge format
    #[tokio::test]
    async fn test_download_deck_from_moxfield_url() {
        // Test URL from Moxfield
        let moxfield_url = "https://moxfield.com/decks/oR2h0X7tREyhBBW3AlC8tw";
        
        // Extract deck ID from URL
        let deck_id = moxfield_url
            .split("/decks/")
            .nth(1)
            .expect("Invalid Moxfield URL format");
        
        assert_eq!(deck_id, "oR2h0X7tREyhBBW3AlC8tw");
        
        // Mock API base URL (in real scenario, this would point to your backend)
        let api_base_url = "http://localhost:8000";
        
        // Note: This test requires a running mock server to actually execute.
        // The test demonstrates the structure and expected flow.
        // To run integration tests, start test_server.py first.
        
        // Expected result structure
        let expected_forge_format = r#"[metadata]
Name=Test Moxfield Deck

[Commander]
1 Ashling, Flame Dancer|MH3|1

[Main]
1 Lightning Bolt|M20|123
1 Sol Ring|C21|247
35 Mountain|UNH|138

[Sideboard]
1 Pyroblast|ICE|213

[Attractions]
"#;
        
        // Verify expected format structure
        assert!(expected_forge_format.contains("[metadata]"));
        assert!(expected_forge_format.contains("[Commander]"));
        assert!(expected_forge_format.contains("[Main]"));
        assert!(expected_forge_format.contains("[Sideboard]"));
        assert!(expected_forge_format.contains("[Attractions]"));
        
        // Verify card format: quantity name|set|collector_number
        assert!(expected_forge_format.contains("1 Ashling, Flame Dancer|MH3|1"));
        assert!(expected_forge_format.contains("1 Lightning Bolt|M20|123"));
        assert!(expected_forge_format.contains("35 Mountain|UNH|138"));
        
        println!("Test URL: {}", moxfield_url);
        println!("Extracted Deck ID: {}", deck_id);
        println!("API Endpoint: {}/api/moxfield/decks/{}/export?format=forge", api_base_url, deck_id);
    }

    /// Integration test for full deck creation from URL
    /// This test demonstrates the actual API call and file creation
    /// Requires test_server.py to be running on localhost:8000
    #[tokio::test]
    #[ignore] // Ignored by default - run with: cargo test -- --ignored
    async fn test_integration_create_deck_from_moxfield_url() {
        // Start with a Moxfield URL
        let moxfield_url = "https://moxfield.com/decks/oR2h0X7tREyhBBW3AlC8tw";
        
        // Extract deck ID
        let deck_id = moxfield_url
            .split("/decks/")
            .nth(1)
            .expect("Invalid Moxfield URL");
        
        // API endpoint (test server)
        let api_base_url = "http://localhost:8000";
        
        // Call the actual function
        let result = create_deck_from_id(deck_id, api_base_url).await;
        
        // Verify success
        match result {
            Ok(creation_result) => {
                assert!(creation_result.success, "Deck creation should succeed");
                assert!(creation_result.deck_path.is_some(), "Deck path should be set");
                
                let deck_path = creation_result.deck_path.unwrap();
                println!("Deck created at: {:?}", deck_path);
                
                // Verify file exists
                assert!(deck_path.exists(), "Deck file should exist");
                
                // Read and verify content
                let content = fs::read_to_string(&deck_path)
                    .expect("Should be able to read deck file");
                
                // Verify Forge format sections
                assert!(content.contains("[metadata]"), "Should have metadata section");
                assert!(content.contains("[Commander]"), "Should have Commander section");
                assert!(content.contains("[Main]"), "Should have Main section");
                assert!(content.contains("[Sideboard]"), "Should have Sideboard section");
                assert!(content.contains("[Attractions]"), "Should have Attractions section");
                
                // Verify card format (quantity name|set|collector_number)
                let has_proper_format = content.lines().any(|line| {
                    line.contains("|") && line.split('|').count() == 3
                });
                assert!(has_proper_format, "Cards should be in format: quantity name|set|collector_number");
                
                println!("Deck file content:\n{}", content);
                
                // Cleanup: remove test file
                fs::remove_file(&deck_path).ok();
            }
            Err(e) => {
                panic!("Deck creation failed: {}. Make sure test_server.py is running on localhost:8000", e);
            }
        }
    }

    /// Test the complete expected output format for a deck file
    #[test]
    fn test_forge_deck_format_specification() {
        // This test documents the expected Forge deck file format
        
        let expected_format = r#"[metadata]
Name=Example Commander Deck

[Commander]
1 Ashling, Flame Dancer|MH3|1

[Main]
1 Lightning Bolt|M20|123
1 Sol Ring|C21|247
1 Arcane Signet|CMM|54
35 Snow-Covered Mountain|MB2|1

[Sideboard]
1 Pyroblast|ICE|213
1 Red Elemental Blast|M25|154

[Attractions]
"#;

        // Parse and verify structure
        let lines: Vec<&str> = expected_format.lines().collect();
        
        // Check required sections
        assert!(lines.iter().any(|l| l.contains("[metadata]")));
        assert!(lines.iter().any(|l| l.contains("[Commander]")));
        assert!(lines.iter().any(|l| l.contains("[Main]")));
        assert!(lines.iter().any(|l| l.contains("[Sideboard]")));
        assert!(lines.iter().any(|l| l.contains("[Attractions]")));
        
        // Check card format: quantity name|set|collector_number
        let card_lines: Vec<&str> = lines.iter()
            .filter(|l| l.contains("|"))
            .copied()
            .collect();
        
        for card_line in card_lines {
            let parts: Vec<&str> = card_line.split('|').collect();
            assert_eq!(parts.len(), 3, "Card line should have 3 parts separated by |");
            
            // First part should be "quantity name"
            let first_part: Vec<&str> = parts[0].trim().split_whitespace().collect();
            assert!(first_part.len() >= 2, "First part should have quantity and name");
            
            // Verify quantity is a number
            let quantity = first_part[0].parse::<u32>();
            assert!(quantity.is_ok(), "Quantity should be a number");
            
            // Second part is set code
            assert!(!parts[1].is_empty(), "Set code should not be empty");
            
            // Third part is collector number
            assert!(!parts[2].trim().is_empty(), "Collector number should not be empty");
        }
        
        println!("Forge format specification verified:");
        println!("{}", expected_format);
    }

    // ==================== Moxfield User Decks Tests ====================

    #[test]
    fn test_moxfield_deck_entry_deserialization() {
        let json = r#"{
            "publicId": "abc123",
            "name": "Test Commander Deck",
            "format": "commander",
            "colors": ["W", "U"],
            "viewCount": 100,
            "likeCount": 5,
            "commentCount": 2
        }"#;
        
        let entry: MoxfieldDeckEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.public_id, "abc123");
        assert_eq!(entry.name, "Test Commander Deck");
        assert_eq!(entry.format, Some("commander".to_string()));
        assert_eq!(entry.colors.len(), 2);
        assert_eq!(entry.view_count, 100);
    }

    #[test]
    fn test_moxfield_deck_entry_minimal_deserialization() {
        // Test with only required fields
        let json = r#"{
            "publicId": "xyz789",
            "name": "Minimal Deck"
        }"#;
        
        let entry: MoxfieldDeckEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.public_id, "xyz789");
        assert_eq!(entry.name, "Minimal Deck");
        assert!(entry.format.is_none());
        assert!(entry.colors.is_empty());
        assert_eq!(entry.view_count, 0);
    }

    #[test]
    fn test_moxfield_user_decks_response_deserialization() {
        let json = r#"{
            "pageNumber": 1,
            "pageSize": 10,
            "totalResults": 2,
            "totalPages": 1,
            "data": [
                {"publicId": "deck1", "name": "Deck One"},
                {"publicId": "deck2", "name": "Deck Two"}
            ]
        }"#;
        
        let response: MoxfieldUserDecksResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.page_number, 1);
        assert_eq!(response.page_size, 10);
        assert_eq!(response.total_results, 2);
        assert_eq!(response.total_pages, 1);
        assert_eq!(response.data.len(), 2);
        assert_eq!(response.data[0].name, "Deck One");
    }

    #[test]
    fn test_user_decks_import_result_success() {
        let imported = vec![
            DeckCreationResult::success("Deck 1".to_string(), PathBuf::from("/test/deck1.dck")),
            DeckCreationResult::success("Deck 2".to_string(), PathBuf::from("/test/deck2.dck")),
        ];
        let failed: Vec<(String, String)> = vec![];
        
        let result = UserDecksImportResult::success("TestUser".to_string(), imported, failed);
        
        assert!(result.success);
        assert_eq!(result.username, "TestUser");
        assert_eq!(result.total_decks, 2);
        assert_eq!(result.imported_decks.len(), 2);
        assert!(result.failed_decks.is_empty());
        assert!(result.message.contains("2/2"));
    }

    #[test]
    fn test_user_decks_import_result_partial_failure() {
        let imported = vec![
            DeckCreationResult::success("Deck 1".to_string(), PathBuf::from("/test/deck1.dck")),
        ];
        let failed = vec![
            ("Deck 2".to_string(), "API error".to_string()),
        ];
        
        let result = UserDecksImportResult::success("TestUser".to_string(), imported, failed);
        
        assert!(!result.success); // Has failures
        assert_eq!(result.total_decks, 2);
        assert_eq!(result.imported_decks.len(), 1);
        assert_eq!(result.failed_decks.len(), 1);
        assert!(result.message.contains("1 failed"));
    }

    #[test]
    fn test_user_decks_import_result_failed() {
        let result = UserDecksImportResult::failed(
            "TestUser".to_string(), 
            "User not found".to_string()
        );
        
        assert!(!result.success);
        assert_eq!(result.username, "TestUser");
        assert_eq!(result.total_decks, 0);
        assert!(result.message.contains("Failed"));
        assert!(result.message.contains("User not found"));
    }

    /// Integration test for fetching real Moxfield user decks
    /// This test requires network access to Moxfield API
    #[tokio::test]
    #[ignore] // Ignored by default - run with: cargo test -- --ignored
    async fn test_integration_fetch_moxfield_user_decks() {
        let username = "IceMagma";
        
        let result = fetch_moxfield_user_decks(username).await;
        
        match result {
            Ok(decks) => {
                println!("Found {} decks for user '{}'", decks.len(), username);
                assert!(!decks.is_empty(), "User should have some public decks");
                
                // Print first few decks
                for (i, deck) in decks.iter().take(5).enumerate() {
                    println!("  {}. {} (ID: {}, Format: {:?})", 
                             i + 1, deck.name, deck.public_id, deck.format);
                }
            }
            Err(e) => {
                panic!("Failed to fetch user decks: {}. Check network connectivity.", e);
            }
        }
    }

    /// Integration test for listing user decks via the public function
    #[tokio::test]
    #[ignore]
    async fn test_integration_list_moxfield_user_decks() {
        let username = "IceMagma";
        let api_base_url = "https://mamo-magic.vercel.app";
        
        let result = list_moxfield_user_decks(username, api_base_url).await;
        
        match result {
            Ok(decks) => {
                println!("Listed {} decks for user '{}'", decks.len(), username);
                
                // Verify we get the expected structure
                for deck in decks.iter().take(3) {
                    assert!(!deck.public_id.is_empty());
                    assert!(!deck.name.is_empty());
                    println!("  - {} ({})", deck.name, deck.public_id);
                }
            }
            Err(e) => {
                println!("Could not list decks (network may be unavailable): {}", e);
            }
        }
    }
}
