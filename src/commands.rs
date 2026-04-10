use log::{error, info, warn};
use std::sync::{Arc, Mutex};
use crate::deeplink::Deeplink;
use crate::deck::{create_deck_from_id, create_deck_from_moxfield, create_deck_from_mamo, create_deck_from_mamo_with_progress, DeckCreationResult, UserDecksImportResult, import_user_decks, list_moxfield_user_decks, MoxfieldDeckEntry, ProgressCallback};
use crate::forge::{launch_forge_from_settings, launch_forge_replay, ForgeLaunchResult};
use crate::gamelog::{download_replay_content, save_replay_to_forge_dir};
use crate::settings::Settings;
use crate::simulation::{run_simulation_for_deck, post_simulation_report, SimulationResult};

/// Type alias for a shared log collector
pub type SharedLogCollector = Arc<Mutex<Vec<String>>>;

/// Create a progress callback that collects logs
#[allow(dead_code)]
pub fn make_log_collector(collector: SharedLogCollector) -> ProgressCallback {
    Box::new(move |msg: &str| {
        if let Ok(mut logs) = collector.lock() {
            logs.push(msg.to_string());
        }
    })
}

#[derive(Debug, Clone)]
pub enum CommandResult {
    DeckCreated(DeckCreationResult),
    DeckCreatedAndLaunched(DeckCreationResult, ForgeLaunchResult),
    ForgeLaunched(ForgeLaunchResult),
    ReplayGameLaunched(ForgeLaunchResult),
    UserDecksImported(UserDecksImportResult),
    UserDecksList(Vec<MoxfieldDeckEntry>),
    AuthTokenSaved(String),  // Success message
    SimulationCompleted(SimulationResult),
    UnknownAction(String),
    MissingParameters(String),
    Error(String),
}

#[allow(dead_code)]
impl CommandResult {
    pub fn get_message(&self) -> String {
        match self {
            CommandResult::DeckCreated(result) => result.message.clone(),
            CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                format!("{} | {}", deck_result.message, forge_result.message)
            }
            CommandResult::ForgeLaunched(result) => result.message.clone(),
            CommandResult::ReplayGameLaunched(result) => result.message.clone(),
            CommandResult::UserDecksImported(result) => result.message.clone(),
            CommandResult::UserDecksList(decks) => format!("Found {} decks", decks.len()),
            CommandResult::AuthTokenSaved(msg) => msg.clone(),
            CommandResult::SimulationCompleted(result) => result.message.clone(),
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
            CommandResult::ReplayGameLaunched(result) => result.success,
            CommandResult::UserDecksImported(result) => result.success,
            CommandResult::UserDecksList(decks) => !decks.is_empty(),
            CommandResult::AuthTokenSaved(_) => true,
            CommandResult::SimulationCompleted(result) => result.success,
            _ => false,
        }
    }
}

#[allow(dead_code)]
pub async fn handle_command(deeplink: &Deeplink) -> CommandResult {
    handle_command_with_logger(deeplink, None).await
}

pub async fn handle_command_with_logger(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    info!("Handling command with action: {}", deeplink.action);
    
    let log = |msg: &str| {
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };
    
    log(&format!("Processing action: {}", deeplink.action));
    
    match deeplink.action.as_str() {
        "create-deck" => handle_create_deck(deeplink).await,
        "createdeck" => handle_create_deck(deeplink).await, // Alternative format
        "deck" => handle_deck_download(deeplink).await, // New: mamoConnector://deck/DECK_ID
        "mamo" => handle_mamo_deck_download(deeplink).await, // MaMo backend: mamoConnector://mamo/DECK_UUID
        "launch-forge" | "launchforge" | "playtest" => handle_launch_forge_with_logger(deeplink, log_collector).await, // Launch Forge with deck
        "replay-game" | "replaygame" => handle_replay_game_with_logger(deeplink, log_collector).await, // Replay a game in Forge
        "import-user-decks" | "importuserdecks" => handle_import_user_decks(deeplink).await,
        "list-user-decks" | "listuserdecks" => handle_list_user_decks(deeplink).await,
        "auth" | "authenticate" | "connect" => handle_auth(deeplink).await, // Auth token: mamoConnector://auth?token=xxx
        "simulate" => handle_simulate(deeplink, log_collector).await, // AI simulation: mamoConnector://simulate/DECK_UUID
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
#[allow(dead_code)]
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

/// Handle mamoConnector://launch-forge?deckId=UUID or mamoConnector://playtest/UUID
/// Downloads deck from MaMo and launches Forge with it - with progress logging
async fn handle_launch_forge_with_logger(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };
    
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

    log(&format!("Launch Forge command - deck_id: {:?}", deck_id));

    // If we have a deck ID and shouldn't skip download, download it first
    let deck_path: Option<String> = if let Some(ref id) = deck_id {
        if skip_download {
            log("Using existing deck (skip_download=true)");
            existing_deck_path
        } else {
            // Download the deck from MaMo with progress logging
            log(&format!("Downloading deck from MaMo: {}", id));
            
            // Create progress callback that uses the log collector
            let progress_callback: Option<crate::deck::ProgressCallback> = log_collector.clone().map(|collector| {
                Box::new(move |msg: &str| {
                    if let Ok(mut logs) = collector.lock() {
                        logs.push(msg.to_string());
                    }
                }) as crate::deck::ProgressCallback
            });
            
            match create_deck_from_mamo_with_progress(id, progress_callback.as_ref()).await {
                Ok(result) => {
                    if result.success {
                        log(&format!("Deck ready: {:?}", result.deck_path));
                        // Convert PathBuf to string for launch_forge_from_settings
                        let deck_path_str = result.deck_path.as_ref()
                            .map(|p| p.to_string_lossy().to_string());
                        
                        log("Launching Forge with deck...");
                        let forge_result = launch_forge_from_settings(deck_path_str.as_deref());
                        match forge_result {
                            Ok(forge_res) => {
                                if forge_res.success {
                                    log("Forge launched successfully!");
                                } else {
                                    log(&format!("Forge launch issue: {}", forge_res.message));
                                }
                                return CommandResult::DeckCreatedAndLaunched(result, forge_res);
                            }
                            Err(e) => {
                                log(&format!("Forge launch failed: {}", e));
                                return CommandResult::Error(format!(
                                    "Deck downloaded but Forge launch failed: {}", e
                                ));
                            }
                        }
                    } else {
                        // Deck download failed, but still launch Forge without deck
                        log(&format!("Deck download failed: {}", result.message));
                        log("Launching Forge without deck...");
                        match launch_forge_from_settings(None) {
                            Ok(forge_res) => {
                                if forge_res.success {
                                    log("Forge launched (without deck due to download error)");
                                }
                                return CommandResult::DeckCreatedAndLaunched(result, forge_res);
                            }
                            Err(e) => {
                                log(&format!("Forge launch also failed: {}", e));
                                return CommandResult::Error(format!(
                                    "Deck download failed and Forge launch failed: {}", e
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    // Error during deck download, but still try to launch Forge
                    log(&format!("Error downloading deck: {}", e));
                    log("Launching Forge without deck...");
                    match launch_forge_from_settings(None) {
                        Ok(forge_res) => {
                            if forge_res.success {
                                log("Forge launched (without deck due to error)");
                            }
                            return CommandResult::ForgeLaunched(forge_res);
                        }
                        Err(forge_err) => {
                            log(&format!("Forge launch also failed: {}", forge_err));
                            return CommandResult::Error(format!("Failed to download deck: {}. Forge launch also failed: {}", e, forge_err));
                        }
                    }
                }
            }
        }
    } else {
        log("No deck ID provided, launching Forge without deck");
        existing_deck_path
    };

    // Launch Forge (without deck download, or deck already exists)
    log("Launching Forge...");
    match launch_forge_from_settings(deck_path.as_deref()) {
        Ok(result) => {
            if result.success {
                log("Forge launched successfully!");
            }
            CommandResult::ForgeLaunched(result)
        }
        Err(e) => {
            log(&format!("Failed to launch Forge: {}", e));
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

/// Handle mamoConnector://replay-game/GAMELOG_UUID — download replay from backend and launch Forge in replay mode
async fn handle_replay_game_with_logger(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };

    // Extract gamelog ID from path (same as deck_id extraction — URL path segment)
    let gamelog_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "gamelog_id"))
        .or_else(|| get_parameter(&deeplink.params, "gamelogId"));

    let gamelog_id = match gamelog_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            error!("No gamelog ID provided in replay-game command");
            return CommandResult::MissingParameters(
                "Gamelog ID is required. Use mamoConnector://replay-game/GAMELOG_UUID".to_string()
            );
        }
    };

    log(&format!("Replay game command — gamelog ID: {}", gamelog_id));

    // Load settings and check for auth token + API URL
    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => {
            log(&format!("Failed to load settings: {}", e));
            return CommandResult::Error(format!("Failed to load settings: {}", e));
        }
    };

    let auth_token = match settings.auth_token.as_ref().or(settings.gamelog_config.auth_token.as_ref()) {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            log("No authentication token found. Please authenticate the Connector from MaMo first.");
            return CommandResult::Error(
                "Not authenticated. Please connect the Connector to MaMo first (use the auth deeplink).".to_string()
            );
        }
    };

    let api_url = &settings.gamelog_config.api_url;
    log(&format!("Downloading replay from backend: {}", api_url));

    // Download replay content
    let (content, filename) = match download_replay_content(api_url, &gamelog_id, &auth_token).await {
        Ok(result) => result,
        Err(e) => {
            log(&format!("Failed to download replay: {}", e));
            return CommandResult::Error(format!("Failed to download replay: {}", e));
        }
    };

    log(&format!("Downloaded replay: {} ({} bytes)", filename, content.len()));

    // Save replay to Forge gamelogs directory
    let replay_path = match save_replay_to_forge_dir(&filename, &content) {
        Ok(path) => path,
        Err(e) => {
            log(&format!("Failed to save replay file: {}", e));
            return CommandResult::Error(format!("Failed to save replay file: {}", e));
        }
    };

    let replay_path_str = replay_path.to_string_lossy().to_string();
    log(&format!("Saved replay to: {}", replay_path_str));

    // Launch Forge in replay mode
    log("Launching Forge in replay mode...");
    match launch_forge_replay(&replay_path_str) {
        Ok(result) => {
            if result.success {
                log(&format!("Forge replay: {}", result.message));
            } else {
                log(&format!("Forge launch issue: {}", result.message));
            }
            CommandResult::ReplayGameLaunched(result)
        }
        Err(e) => {
            log(&format!("Failed to launch Forge: {}", e));
            CommandResult::Error(format!("Replay file saved but Forge launch failed: {}", e))
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

/// Handle mamoConnector://simulate/DECK_UUID
///
/// Full simulation pipeline:
///   1. Download deck from MaMo backend (.dck format)
///   2. Run Forge headless batch simulation
///   3. Analyse per-game stat JSONs with Python script
///   4. POST aggregated report to MaMo backend
async fn handle_simulate(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };

    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"));

    let deck_id = match deck_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            error!("No deck UUID provided in simulate command");
            return CommandResult::MissingParameters(
                "Deck UUID is required. Use mamoConnector://simulate/DECK_UUID".to_string(),
            );
        }
    };

    log(&format!("Simulate command — deck UUID: {}", deck_id));

    // Download deck from MaMo backend to get the local .dck file name
    log("Downloading deck from MaMo backend…");
    let deck_result = match create_deck_from_mamo(&deck_id).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to download deck for simulation: {}", e);
            return CommandResult::Error(format!("Failed to download deck: {}", e));
        }
    };

    if !deck_result.success {
        return CommandResult::Error(format!("Deck download failed: {}", deck_result.message));
    }

    // Extract deck name stem from the saved .dck file path
    let deck_name = match &deck_result.deck_path {
        Some(path) => path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
        None => {
            return CommandResult::Error(
                "Deck downloaded but path unknown — cannot determine deck name for simulation".to_string(),
            )
        }
    };

    log(&format!("Deck '{}' ready. Starting AI simulation…", deck_name));

    let result = run_simulation_for_deck(&deck_id, &deck_name, &log).await;

    // If simulation failed, POST an error report so the frontend poll detects it
    if !result.success {
        let error_report = serde_json::json!({
            "error": result.message,
            "success": false
        });
        let auth_token = Settings::load().ok().and_then(|s| s.auth_token);
        if let Err(e) = post_simulation_report(&deck_id, &error_report, auth_token.as_deref()).await {
            warn!("Could not upload error report: {}", e);
        } else {
            log("Error report uploaded — frontend will be notified.");
        }
    }

    CommandResult::SimulationCompleted(result)
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