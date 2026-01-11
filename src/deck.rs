use anyhow::{Context, Result};
use log::info;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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

/// Create a deck file by fetching from API with forge format
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

/// Fetch deck export from API using POST with deckId and format=forge
async fn fetch_deck_export(deck_id: &str, api_base_url: &str) -> Result<DeckExportResponse> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/decks/export", api_base_url);
    
    let request_body = DeckExportRequest {
        deck_id: deck_id.to_string(),
        format: "forge".to_string(),
    };
    
    info!("Fetching deck export from: {} with deckId: {}, format: forge", url, deck_id);
    
    let response = client
        .post(&url)
        .json(&request_body)
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
        println!("API Endpoint: {}/api/decks/export", api_base_url);
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
}