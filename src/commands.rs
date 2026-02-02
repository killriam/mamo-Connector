use log::{error, info, warn};
use crate::deeplink::Deeplink;
use crate::deck::{create_deck_from_id, create_deck_from_moxfield, create_deck_from_mamo, DeckCreationResult, UserDecksImportResult, import_user_decks, list_moxfield_user_decks, MoxfieldDeckEntry};
use crate::forge::{launch_forge_from_settings, ForgeLaunchResult};
use crate::settings::Settings;

#[derive(Debug, Clone)]
pub enum CommandResult {
    DeckCreated(DeckCreationResult),
    DeckCreatedAndLaunched(DeckCreationResult, ForgeLaunchResult),
    ForgeLaunched(ForgeLaunchResult),
    UserDecksImported(UserDecksImportResult),
    UserDecksList(Vec<MoxfieldDeckEntry>),
    AuthTokenSaved(String),  // Success message
    UnknownAction(String),
    MissingParameters(String),
    Error(String),
}

impl CommandResult {
    pub fn get_message(&self) -> String {
        match self {
            CommandResult::DeckCreated(result) => result.message.clone(),
            CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                format!("{} | {}", deck_result.message, forge_result.message)
            }
            CommandResult::ForgeLaunched(result) => result.message.clone(),
            CommandResult::UserDecksImported(result) => result.message.clone(),
            CommandResult::UserDecksList(decks) => format!("Found {} decks", decks.len()),
            CommandResult::AuthTokenSaved(msg) => msg.clone(),
            CommandResult::UnknownAction(action) => format!("Unknown action: {}", action),
            CommandResult::MissingParameters(msg) => format!("Missing parameters: {}", msg),
            CommandResult::Error(msg) => format!("Error: {}", msg),
        }
    }

    pub fn is_success(&self) -> bool {
        match self {
            CommandResult::DeckCreated(result) => result.success,
            CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                deck_result.success && forge_result.success
            }
            CommandResult::ForgeLaunched(result) => result.success,
            CommandResult::UserDecksImported(result) => result.success,
            CommandResult::UserDecksList(decks) => !decks.is_empty(),
            CommandResult::AuthTokenSaved(_) => true,
            _ => false,
        }
    }
}

pub async fn handle_command(deeplink: &Deeplink) -> CommandResult {
    info!("Handling command with action: {}", deeplink.action);
    
    match deeplink.action.as_str() {
        "create-deck" => handle_create_deck(deeplink).await,
        "createdeck" => handle_create_deck(deeplink).await, // Alternative format
        "deck" => handle_deck_download(deeplink).await, // New: mamoConnector://deck/DECK_ID
        "mamo" => handle_mamo_deck_download(deeplink).await, // MaMo backend: mamoConnector://mamo/DECK_UUID
        "launch-forge" | "launchforge" | "playtest" => handle_launch_forge(deeplink).await, // Launch Forge with deck
        "import-user-decks" | "importuserdecks" => handle_import_user_decks(deeplink).await,
        "list-user-decks" | "listuserdecks" => handle_list_user_decks(deeplink).await,
        "auth" | "authenticate" | "connect" => handle_auth(deeplink).await, // Auth token: mamoConnector://auth?token=xxx
        "" => CommandResult::MissingParameters("No action specified in deeplink".to_string()),
        action => {
            warn!("Unknown action received: {}", action);
            CommandResult::UnknownAction(action.to_string())
        }
    }
}

/// Handle mamoConnector://deck/DECK_ID - direct deck download using curl
async fn handle_deck_download(deeplink: &Deeplink) -> CommandResult {
    // Get deck ID from path or params
    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"));

    let deck_id = match deck_id {
        Some(id) => id,
        None => {
            error!("No deck ID provided in deck command");
            return CommandResult::MissingParameters(
                "Deck ID is required. Use mamoConnector://deck/DECK_ID".to_string()
            );
        }
    };

    info!("Downloading deck directly via curl: {}", deck_id);

    match create_deck_from_moxfield(&deck_id).await {
        Ok(result) => CommandResult::DeckCreated(result),
        Err(err) => {
            error!("Failed to download deck: {:?}", err);
            CommandResult::Error(format!("Failed to download deck: {}", err))
        }
    }
}

/// Handle mamoConnector://mamo/DECK_UUID - download deck from MaMo backend
async fn handle_mamo_deck_download(deeplink: &Deeplink) -> CommandResult {
    // Get deck UUID from path or params
    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"));

    let deck_id = match deck_id {
        Some(id) => id,
        None => {
            error!("No deck UUID provided in mamo command");
            return CommandResult::MissingParameters(
                "Deck UUID is required. Use mamoConnector://mamo/DECK_UUID".to_string()
            );
        }
    };

    info!("Downloading deck from MaMo backend: {}", deck_id);

    match create_deck_from_mamo(&deck_id).await {
        Ok(result) => CommandResult::DeckCreated(result),
        Err(err) => {
            error!("Failed to download MaMo deck: {:?}", err);
            CommandResult::Error(format!("Failed to download MaMo deck: {}", err))
        }
    }
}

/// Handle mamoConnector://launch-forge?deckId=UUID or mamoConnector://playtest/UUID
/// Downloads deck from MaMo and launches Forge with it
async fn handle_launch_forge(deeplink: &Deeplink) -> CommandResult {
    // Get deck UUID from path or params
    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"));

    // Check if we should skip download (deck already exists locally)
    let skip_download = get_parameter(&deeplink.params, "skip_download")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);

    // Get optional deck path for pre-existing deck
    let existing_deck_path = get_parameter(&deeplink.params, "deck_path");

    info!("Launch Forge command - deck_id: {:?}, skip_download: {}", deck_id, skip_download);

    // If we have a deck ID and shouldn't skip download, download it first
    let deck_path: Option<String> = if let Some(ref id) = deck_id {
        if skip_download {
            existing_deck_path
        } else {
            // Download the deck from MaMo
            info!("Downloading deck from MaMo before launching Forge: {}", id);
            match create_deck_from_mamo(id).await {
                Ok(result) => {
                    if result.success {
                        info!("Deck downloaded successfully: {:?}", result.deck_path);
                        // Convert PathBuf to string for launch_forge_from_settings
                        let deck_path_str = result.deck_path.as_ref()
                            .map(|p| p.to_string_lossy().to_string());
                        let forge_result = launch_forge_from_settings(deck_path_str.as_deref());
                        match forge_result {
                            Ok(forge_res) => {
                                return CommandResult::DeckCreatedAndLaunched(result, forge_res);
                            }
                            Err(e) => {
                                return CommandResult::Error(format!(
                                    "Deck downloaded but Forge launch failed: {}", e
                                ));
                            }
                        }
                    } else {
                        return CommandResult::Error(format!(
                            "Failed to download deck: {}", result.message
                        ));
                    }
                }
                Err(e) => {
                    error!("Failed to download deck for Forge: {}", e);
                    return CommandResult::Error(format!("Failed to download deck: {}", e));
                }
            }
        }
    } else {
        existing_deck_path
    };

    // Launch Forge (without deck download, or deck already exists)
    match launch_forge_from_settings(deck_path.as_deref()) {
        Ok(result) => CommandResult::ForgeLaunched(result),
        Err(e) => {
            error!("Failed to launch Forge: {}", e);
            CommandResult::Error(format!("Failed to launch Forge: {}", e))
        }
    }
}

/// Handle mamoConnector://auth?token=PAT_xxx - save authentication token for gamelog uploads
/// This allows the frontend to securely transfer the user's personal access token
async fn handle_auth(deeplink: &Deeplink) -> CommandResult {
    // Get token from query params
    let token = deeplink.token.clone()
        .or_else(|| get_parameter(&deeplink.params, "token"))
        .or_else(|| get_parameter(&deeplink.params, "pat"))
        .or_else(|| get_parameter(&deeplink.params, "access_token"));

    let token = match token {
        Some(t) if !t.is_empty() => t,
        _ => {
            error!("No token provided in auth command");
            return CommandResult::MissingParameters(
                "Token is required. Use mamoConnector://auth?token=YOUR_TOKEN".to_string()
            );
        }
    };

    info!("Received auth token via deeplink (length: {} chars)", token.len());

    // Load settings, update token, and save
    let mut settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to load settings: {}", e);
            return CommandResult::Error(format!("Failed to load settings: {}", e));
        }
    };

    // Store the token in both places
    settings.auth_token = Some(token.clone());
    settings.gamelog_config.auth_token = Some(token);

    // Save settings
    if let Err(e) = settings.save() {
        error!("Failed to save auth token: {}", e);
        return CommandResult::Error(format!("Failed to save auth token: {}", e));
    }

    info!("Auth token saved successfully");
    CommandResult::AuthTokenSaved("Authentication token saved successfully! Game log uploads are now enabled.".to_string())
}

async fn handle_create_deck(deeplink: &Deeplink) -> CommandResult {
    // Extract deck ID from parameters
    let deck_id = match get_parameter(&deeplink.params, "id")
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"))
    {
        Some(id) => id,
        None => {
            error!("No deck ID provided in create-deck command");
            return CommandResult::MissingParameters(
                "Deck ID is required. Use 'id', 'deck_id', or 'deckId' parameter".to_string()
            );
        }
    };

    // Extract API base URL (with default fallback)
    let api_base_url = get_parameter(&deeplink.params, "api_url")
        .or_else(|| get_parameter(&deeplink.params, "api"))
        .unwrap_or_else(|| "https://api.example.com".to_string()); // Replace with actual default API URL

    info!("Creating deck with ID: {} from API: {}", deck_id, api_base_url);

    match create_deck_from_id(&deck_id, &api_base_url).await {
        Ok(result) => CommandResult::DeckCreated(result),
        Err(err) => {
            error!("Failed to create deck: {:?}", err);
            CommandResult::Error(format!("Failed to create deck: {}", err))
        }
    }
}

async fn handle_import_user_decks(deeplink: &Deeplink) -> CommandResult {
    // Extract username from parameters
    let username = match get_parameter(&deeplink.params, "username")
        .or_else(|| get_parameter(&deeplink.params, "user"))
        .or_else(|| deeplink.username.clone())
    {
        Some(u) => u,
        None => {
            error!("No username provided in import-user-decks command");
            return CommandResult::MissingParameters(
                "Username is required. Use 'username' or 'user' parameter".to_string()
            );
        }
    };

    // Extract API base URL (with default fallback)
    let api_base_url = get_parameter(&deeplink.params, "api_url")
        .or_else(|| get_parameter(&deeplink.params, "api"))
        .unwrap_or_else(|| "https://api.example.com".to_string());

    info!("Importing decks for user: {} from API: {}", username, api_base_url);

    match import_user_decks(&username, &api_base_url).await {
        Ok(result) => CommandResult::UserDecksImported(result),
        Err(err) => {
            error!("Failed to import user decks: {:?}", err);
            CommandResult::Error(format!("Failed to import user decks: {}", err))
        }
    }
}

async fn handle_list_user_decks(deeplink: &Deeplink) -> CommandResult {
    // Extract username from parameters
    let username = match get_parameter(&deeplink.params, "username")
        .or_else(|| get_parameter(&deeplink.params, "user"))
        .or_else(|| deeplink.username.clone())
    {
        Some(u) => u,
        None => {
            error!("No username provided in list-user-decks command");
            return CommandResult::MissingParameters(
                "Username is required. Use 'username' or 'user' parameter".to_string()
            );
        }
    };

    // Extract API base URL (with default fallback to Vercel backend)
    let api_base_url = get_parameter(&deeplink.params, "api_url")
        .or_else(|| get_parameter(&deeplink.params, "api"))
        .unwrap_or_else(|| "https://new-backend-two-eosin.vercel.app".to_string());

    info!("Listing decks for Moxfield user: {} via API: {}", username, api_base_url);

    match list_moxfield_user_decks(&username, &api_base_url).await {
        Ok(decks) => {
            info!("Found {} decks for user '{}'", decks.len(), username);
            CommandResult::UserDecksList(decks)
        }
        Err(err) => {
            error!("Failed to list user decks: {:?}", err);
            CommandResult::Error(format!("Failed to list user decks: {}", err))
        }
    }
}

fn get_parameter(params: &[(String, String)], key: &str) -> Option<String> {
    params.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== Parameter Extraction Tests ====================

    #[test]
    fn test_get_parameter_exact_match() {
        let params = vec![
            ("id".to_string(), "123".to_string()),
            ("name".to_string(), "test".to_string()),
        ];
        
        assert_eq!(get_parameter(&params, "id"), Some("123".to_string()));
        assert_eq!(get_parameter(&params, "name"), Some("test".to_string()));
    }

    #[test]
    fn test_get_parameter_case_insensitive() {
        let params = vec![
            ("id".to_string(), "123".to_string()),
            ("API_URL".to_string(), "http://test.com".to_string()),
        ];
        
        assert_eq!(get_parameter(&params, "ID"), Some("123".to_string()));
        assert_eq!(get_parameter(&params, "Id"), Some("123".to_string()));
        assert_eq!(get_parameter(&params, "api_url"), Some("http://test.com".to_string()));
        assert_eq!(get_parameter(&params, "Api_Url"), Some("http://test.com".to_string()));
    }

    #[test]
    fn test_get_parameter_not_found() {
        let params = vec![
            ("id".to_string(), "123".to_string()),
        ];
        
        assert_eq!(get_parameter(&params, "missing"), None);
        assert_eq!(get_parameter(&params, ""), None);
    }

    #[test]
    fn test_get_parameter_empty_params() {
        let params: Vec<(String, String)> = vec![];
        
        assert_eq!(get_parameter(&params, "id"), None);
    }

    #[test]
    fn test_get_parameter_empty_value() {
        let params = vec![
            ("id".to_string(), "".to_string()),
        ];
        
        assert_eq!(get_parameter(&params, "id"), Some("".to_string()));
    }

    // ==================== CommandResult Tests ====================

    #[test]
    fn test_command_result_get_message_unknown_action() {
        let result = CommandResult::UnknownAction("test-action".to_string());
        assert_eq!(result.get_message(), "Unknown action: test-action");
    }

    #[test]
    fn test_command_result_get_message_missing_params() {
        let result = CommandResult::MissingParameters("deck_id required".to_string());
        assert_eq!(result.get_message(), "Missing parameters: deck_id required");
    }

    #[test]
    fn test_command_result_get_message_error() {
        let result = CommandResult::Error("API failed".to_string());
        assert_eq!(result.get_message(), "Error: API failed");
    }

    #[test]
    fn test_command_result_is_success_unknown_action() {
        let result = CommandResult::UnknownAction("test".to_string());
        assert!(!result.is_success());
    }

    #[test]
    fn test_command_result_is_success_missing_params() {
        let result = CommandResult::MissingParameters("test".to_string());
        assert!(!result.is_success());
    }

    #[test]
    fn test_command_result_is_success_error() {
        let result = CommandResult::Error("test".to_string());
        assert!(!result.is_success());
    }

    #[test]
    fn test_command_result_is_success_deck_created_success() {
        use std::path::PathBuf;
        use crate::deck::DeckCreationResult;
        
        let deck_result = DeckCreationResult::success(
            "Created".to_string(),
            PathBuf::from("/test/deck.txt")
        );
        let result = CommandResult::DeckCreated(deck_result);
        assert!(result.is_success());
    }

    #[test]
    fn test_command_result_is_success_deck_created_failed() {
        use crate::deck::DeckCreationResult;
        
        let deck_result = DeckCreationResult::failed("Failed".to_string());
        let result = CommandResult::DeckCreated(deck_result);
        assert!(!result.is_success());
    }

    // ==================== Async Command Handler Tests ====================

    fn create_test_deeplink(action: &str, params: Vec<(&str, &str)>) -> Deeplink {
        let params: Vec<(String, String)> = params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        
        let deck_id = params.iter()
            .find(|(k, _)| k == "id" || k == "deck_id" || k == "deckId")
            .map(|(_, v)| v.clone());
        
        let username = params.iter()
            .find(|(k, _)| k == "username" || k == "user")
            .map(|(_, v)| v.clone());
        
        Deeplink {
            raw: format!("mamoConnector://{}?test", action),
            action: action.to_string(),
            params,
            token: None,
            doc: None,
            deck_id,
            username,
        }
    }

    #[tokio::test]
    async fn test_handle_command_unknown_action() {
        let deeplink = create_test_deeplink("unknown-action", vec![]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::UnknownAction(action) => {
                assert_eq!(action, "unknown-action");
            }
            _ => panic!("Expected UnknownAction result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_empty_action() {
        let deeplink = create_test_deeplink("", vec![]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::MissingParameters(msg) => {
                assert!(msg.contains("No action"));
            }
            _ => panic!("Expected MissingParameters result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_create_deck_missing_id() {
        let deeplink = create_test_deeplink("create-deck", vec![
            ("api_url", "http://localhost:8080")
        ]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::MissingParameters(msg) => {
                assert!(msg.contains("Deck ID"));
            }
            _ => panic!("Expected MissingParameters result for missing deck ID"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_createdeck_variant() {
        // Test the alternative "createdeck" action (no hyphen)
        let deeplink = create_test_deeplink("createdeck", vec![]);
        let result = handle_command(&deeplink).await;
        
        // Should handle createdeck the same as create-deck
        match result {
            CommandResult::MissingParameters(msg) => {
                assert!(msg.contains("Deck ID"));
            }
            _ => panic!("Expected MissingParameters for createdeck without ID"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_create_deck_with_id_param() {
        let deeplink = create_test_deeplink("create-deck", vec![
            ("id", "12345"),
            ("api_url", "http://invalid-url-for-test.local")
        ]);
        let result = handle_command(&deeplink).await;
        
        // Should attempt to create deck and fail due to invalid API URL
        match result {
            CommandResult::Error(msg) => {
                assert!(msg.contains("Failed to create deck"));
            }
            CommandResult::DeckCreated(_) => {
                // This could happen if there's actually a server at that URL
            }
            _ => panic!("Expected Error or DeckCreated result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_create_deck_with_deck_id_param() {
        let deeplink = create_test_deeplink("create-deck", vec![
            ("deck_id", "abc123"),
            ("api_url", "http://invalid-url-for-test.local")
        ]);
        let result = handle_command(&deeplink).await;
        
        // Should recognize deck_id parameter
        match result {
            CommandResult::Error(msg) => {
                assert!(msg.contains("Failed to create deck"));
            }
            CommandResult::DeckCreated(_) => {}
            _ => panic!("Expected Error or DeckCreated result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_create_deck_with_deckId_param() {
        let deeplink = create_test_deeplink("create-deck", vec![
            ("deckId", "xyz789"),
            ("api_url", "http://invalid-url-for-test.local")
        ]);
        let result = handle_command(&deeplink).await;
        
        // Should recognize deckId (camelCase) parameter
        match result {
            CommandResult::Error(msg) => {
                assert!(msg.contains("Failed to create deck"));
            }
            CommandResult::DeckCreated(_) => {}
            _ => panic!("Expected Error or DeckCreated result"),
        }
    }

    // ==================== Import User Decks Tests ====================

    #[tokio::test]
    async fn test_handle_command_import_user_decks_missing_username() {
        let deeplink = create_test_deeplink("import-user-decks", vec![
            ("api_url", "http://localhost:8080")
        ]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::MissingParameters(msg) => {
                assert!(msg.contains("Username"));
            }
            _ => panic!("Expected MissingParameters result for missing username"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_import_user_decks_with_username() {
        let deeplink = create_test_deeplink("import-user-decks", vec![
            ("username", "IceMagma"),
            ("api_url", "http://invalid-url-for-test.local")
        ]);
        let result = handle_command(&deeplink).await;
        
        // Should attempt to fetch from Moxfield first, then fail on API
        match result {
            CommandResult::UserDecksImported(import_result) => {
                // Either success (if Moxfield is reachable) or failed (if not)
                assert_eq!(import_result.username, "IceMagma");
            }
            CommandResult::Error(_) => {
                // Network error is also acceptable
            }
            _ => panic!("Expected UserDecksImported or Error result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_import_user_decks_alternative_format() {
        let deeplink = create_test_deeplink("importuserdecks", vec![
            ("user", "TestUser"),
        ]);
        let result = handle_command(&deeplink).await;
        
        // Should handle importuserdecks the same as import-user-decks
        match result {
            CommandResult::UserDecksImported(import_result) => {
                assert_eq!(import_result.username, "TestUser");
            }
            CommandResult::Error(_) => {}
            _ => panic!("Expected UserDecksImported or Error result"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_list_user_decks_missing_username() {
        let deeplink = create_test_deeplink("list-user-decks", vec![]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::MissingParameters(msg) => {
                assert!(msg.contains("Username"));
            }
            _ => panic!("Expected MissingParameters result for missing username"),
        }
    }

    #[tokio::test]
    async fn test_handle_command_list_user_decks_with_username() {
        let deeplink = create_test_deeplink("list-user-decks", vec![
            ("username", "IceMagma")
        ]);
        let result = handle_command(&deeplink).await;
        
        match result {
            CommandResult::UserDecksList(decks) => {
                // If Moxfield is reachable, we should get some decks
                // IceMagma has public decks
                println!("Found {} decks for IceMagma", decks.len());
            }
            CommandResult::Error(_) => {
                // Network error is acceptable in test environment
            }
            _ => panic!("Expected UserDecksList or Error result"),
        }
    }
}