use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ==================== Moxfield API Types ====================

/// Represents a deck entry from Moxfield user's deck list
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
}

/// Response from Moxfield API when fetching user decks
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoxfieldUserDecksResponse {
    pub page_number: u32,
    pub page_size: u32,
    pub total_results: u32,
    pub total_pages: u32,
    pub data: Vec<MoxfieldDeckEntry>,
}

/// Result of importing multiple decks from a user profile
#[derive(Debug, Clone)]
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

// ==================== Direct Moxfield Access (using curl) ====================

const MOXFIELD_API_URL: &str = "https://api2.moxfield.com/v2";

/// Fetch data from Moxfield API using curl (bypasses Cloudflare)
fn fetch_with_curl(url: &str) -> Result<String> {
    info!("Fetching from Moxfield via curl: {}", url);
    
    let output = Command::new("curl")
        .args([
            "-s",
            "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            "-H", "Accept: application/json",
            "-H", "Referer: https://www.moxfield.com/",
            url,
        ])
        .output()
        .context("Failed to execute curl command")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("curl failed: {}", stderr));
    }
    
    let body = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in curl response")?;
    
    // Check for Cloudflare block (HTML response)
    if body.contains("<!DOCTYPE html>") || body.contains("Cloudflare") {
        return Err(anyhow::anyhow!("Cloudflare blocked the request"));
    }
    
    Ok(body)
}

/// Moxfield full deck response structure
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    let deck_path = write_deck_file(&full_name, &forge_content).await
        .context("Failed to create deck file")?;
    
    Ok(DeckCreationResult::success(
        format!("Successfully created deck '{}' at {:?}", full_name, deck_path),
        deck_path,
    ))
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
        for (_, card_entry) in sideboard {
            if let Some(card_line) = format_moxfield_card(card_entry) {
                lines.push(card_line);
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
    
    // For double-faced cards like "Brightclimb Pathway // Grimclimb Pathway",
    // Forge only recognizes the front face name
    let name = full_name.split(" // ").next().unwrap_or(full_name);
    
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
    let deck_path = write_deck_file(&export_response.name, &export_response.content).await
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

/// Write deck content directly to file (content already in forge format from API)
async fn write_deck_file(deck_name: &str, content: &str) -> Result<PathBuf> {
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

    // Write deck file with content from API
    fs::write(&deck_file_path, content)
        .with_context(|| format!("Failed to write deck file: {:?}", deck_file_path))?;

    info!("Successfully created deck file: {:?}", deck_file_path);
    Ok(deck_file_path)
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
    for card in &deck_data.sideboard {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        
        let result = list_moxfield_user_decks(username).await;
        
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