use log::{error, info, warn};
use crate::deeplink::Deeplink;
use crate::deck::{create_deck_from_id, DeckCreationResult, UserDecksImportResult, import_user_decks, list_moxfield_user_decks, MoxfieldDeckEntry};

#[derive(Debug, Clone)]
pub enum CommandResult {
    DeckCreated(DeckCreationResult),
    UserDecksImported(UserDecksImportResult),
    UserDecksList(Vec<MoxfieldDeckEntry>),
    UnknownAction(String),
    MissingParameters(String),
    Error(String),
}

impl CommandResult {
    pub fn get_message(&self) -> String {
        match self {
            CommandResult::DeckCreated(result) => result.message.clone(),
            CommandResult::UserDecksImported(result) => result.message.clone(),
            CommandResult::UserDecksList(decks) => format!("Found {} decks", decks.len()),
            CommandResult::UnknownAction(action) => format!("Unknown action: {}", action),
            CommandResult::MissingParameters(msg) => format!("Missing parameters: {}", msg),
            CommandResult::Error(msg) => format!("Error: {}", msg),
        }
    }

    pub fn is_success(&self) -> bool {
        match self {
            CommandResult::DeckCreated(result) => result.success,
            CommandResult::UserDecksImported(result) => result.success,
            CommandResult::UserDecksList(decks) => !decks.is_empty(),
            _ => false,
        }
    }
}

pub async fn handle_command(deeplink: &Deeplink) -> CommandResult {
    info!("Handling command with action: {}", deeplink.action);
    
    match deeplink.action.as_str() {
        "create-deck" => handle_create_deck(deeplink).await,
        "createdeck" => handle_create_deck(deeplink).await, // Alternative format
        "import-user-decks" | "importuserdecks" => handle_import_user_decks(deeplink).await,
        "list-user-decks" | "listuserdecks" => handle_list_user_decks(deeplink).await,
        "" => CommandResult::MissingParameters("No action specified in deeplink".to_string()),
        action => {
            warn!("Unknown action received: {}", action);
            CommandResult::UnknownAction(action.to_string())
        }
    }
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