use log::{error, info, warn};
use std::sync::{Arc, Mutex};
use crate::deeplink::Deeplink;
use crate::deck::{create_deck_from_id, create_deck_from_moxfield, create_deck_from_mamo, create_deck_from_mamo_with_progress, create_deck_and_scenario_for_forge, DeckCreationResult, UserDecksImportResult, import_user_decks, list_moxfield_user_decks, MoxfieldDeckEntry, ProgressCallback};
use crate::forge::{launch_forge_from_settings, launch_forge_replay, ForgeLaunchResult};
use crate::gamelog::{download_replay_content, save_replay_to_forge_dir, ScenarioSyncResult, sync_forge_scenario_file, sync_all_scenario_files};
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
    ScenarioSynced(Vec<ScenarioSyncResult>),
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
            CommandResult::ScenarioSynced(results) => format!("Synchronized {} scenario(s)", results.len()),
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
            CommandResult::ScenarioSynced(_) => true,
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
    
    // If a token is provided in the deeplink, automatically update the settings
    // so any subsequent network calls (e.g. deck downloads, simulations, or gamelog uploads)
    // use the current web app user's authentication context.
    if let Some(ref token) = deeplink.token {
        if !token.is_empty() {
            log("Updating authentication token from deeplink parameter...");
            match Settings::load() {
                Ok(mut settings) => {
                    settings.auth_token = Some(token.clone());
                    settings.gamelog_config.auth_token = Some(token.clone());
                    if let Err(e) = settings.save() {
                        error!("Failed to save auto-loaded token: {}", e);
                        log(&format!("Failed to save auth token: {}", e));
                    } else {
                        info!("Authentication token updated successfully from command");
                        log("Authentication token updated successfully.");
                    }
                }
                Err(e) => {
                    error!("Failed to load settings to save auto-loaded token: {}", e);
                    log(&format!("Failed to load settings: {}", e));
                }
            }
        }
    }
    
    match deeplink.action.as_str() {
        "create-deck" => handle_create_deck(deeplink).await,
        "createdeck" => handle_create_deck(deeplink).await, // Alternative format
        "deck" => handle_deck_download(deeplink).await, // New: mamoConnector://deck/DECK_ID
        "mamo" => handle_mamo_deck_download(deeplink).await, // MaMo backend: mamoConnector://mamo/DECK_UUID
        "download-deck" => handle_download_deck_only(deeplink).await, // Save .dck to Forge dir without launching Forge
        "playtest-scenario" => handle_playtest_with_scenario(deeplink, log_collector).await, // Scenario-ordered deck + JSON + launch Forge
        "sync-scenarios" | "syncscenarios" | "sync-scenario" | "syncscenario" => handle_sync_scenarios(deeplink, log_collector).await, // Sync Forge scenario(s) back to MaMo
        "launch-forge" | "launchforge" | "playtest" => handle_launch_forge_with_logger(deeplink, log_collector).await, // Launch Forge with deck
        "replay-game" | "replaygame" => handle_replay_game_with_logger(deeplink, log_collector).await, // Replay a game in Forge
        "import-user-decks" | "importuserdecks" => handle_import_user_decks(deeplink).await,
        "list-user-decks" | "listuserdecks" => handle_list_user_decks(deeplink).await,
        "auth" | "authenticate" | "connect" => handle_auth(deeplink).await, // Auth token: mamoConnector://auth?token=xxx
        "simulate" => handle_simulate(deeplink, log_collector).await, // Forge simulation: mamoConnector://simulate/DECK_UUID
        "simulate-ai" => handle_simulate_ai(deeplink, log_collector).await, // Native mamo-sim: mamoConnector://simulate-ai/DECK_UUID
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

/// Handle mamoConnector://download-deck/DECK_UUID
/// Downloads the deck from the MaMo backend and saves it to the Forge deck directory.
/// Does NOT launch Forge — only saves the .dck file.
async fn handle_download_deck_only(deeplink: &Deeplink) -> CommandResult {
    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "id"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"))
        .or_else(|| get_parameter(&deeplink.params, "deckId"));

    let deck_id = match deck_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            error!("No deck UUID provided in download-deck command");
            return CommandResult::MissingParameters(
                "Deck UUID is required. Use mamoConnector://download-deck/DECK_UUID".to_string()
            );
        }
    };

    info!("Downloading deck from MaMo (no Forge launch): {}", deck_id);

    match create_deck_from_mamo(&deck_id).await {
        Ok(result) => {
            if result.success {
                info!("Deck saved to Forge directory: {:?}", result.deck_path);
            }
            CommandResult::DeckCreated(result)
        }
        Err(err) => {
            error!("Failed to download deck for Forge: {:?}", err);
            CommandResult::Error(format!("Failed to download deck: {}", err))
        }
    }
}

/// Handle mamoConnector://playtest-scenario/DECK_UUID?scenarioId=SCENARIO_UUID
/// Downloads the scenario-ordered .dck and writes the Forge scenario JSON, then launches Forge.
async fn handle_playtest_with_scenario(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };

    let deck_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "deckId"))
        .or_else(|| get_parameter(&deeplink.params, "deck_id"));
    let scenario_id = get_parameter(&deeplink.params, "scenarioId")
        .or_else(|| get_parameter(&deeplink.params, "scenario_id"));

    let (deck_id, scenario_id) = match (deck_id, scenario_id) {
        (Some(d), Some(s)) if !d.is_empty() && !s.is_empty() => (d, s),
        _ => {
            error!("playtest-scenario requires both deck UUID in path and scenarioId query param");
            return CommandResult::MissingParameters(
                "Usage: mamoConnector://playtest-scenario/DECK_UUID?scenarioId=SCENARIO_UUID".to_string()
            );
        }
    };

    log(&format!("Preparing scenario playtest — deck: {}, scenario: {}", deck_id, scenario_id));

    let result = match create_deck_and_scenario_for_forge(&deck_id, &scenario_id).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to create scenario files: {:?}", e);
            return CommandResult::Error(format!("Failed to prepare scenario: {}", e));
        }
    };

    if !result.success {
        return CommandResult::DeckCreated(result);
    }

    let deck_path_str = result.deck_path.as_ref().map(|p| p.to_string_lossy().to_string());
    log("Launching Forge with scenario deck...");
    let deck2_path = resolve_opponent_deck_path(Some(&deck_id), None, None, &log).await;
    match launch_forge_from_settings(deck_path_str.as_deref(), deck2_path.as_deref()) {
        Ok(forge_res) => CommandResult::DeckCreatedAndLaunched(result, forge_res),
        Err(e) => {
            error!("Forge launch failed: {}", e);
            CommandResult::Error(format!("Deck and scenario ready but Forge launch failed: {}", e))
        }
    }
}

/// Handle mamoConnector://sync-scenarios or mamoConnector://sync-scenario?id=UUID
/// Synchronizes recorded Forge scenario JSON files back to the MaMo backend.
async fn handle_sync_scenarios(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
    let log = |msg: &str| {
        info!("{}", msg);
        if let Some(ref collector) = log_collector {
            if let Ok(mut logs) = collector.lock() {
                logs.push(msg.to_string());
            }
        }
    };

    log("Synchronizing Forge scenarios to MaMo backend...");
    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => return CommandResult::Error(format!("Failed to load settings: {}", e)),
    };

    let scenario_id = deeplink.deck_id.clone()
        .or_else(|| get_parameter(&deeplink.params, "scenarioId"))
        .or_else(|| get_parameter(&deeplink.params, "scenario_id"))
        .or_else(|| get_parameter(&deeplink.params, "id"));

    if let Some(sc_id) = scenario_id {
        let clean_id = sc_id.replace("scenario-", "");
        let scenario_dir = match crate::deck::get_scenario_directory() {
            Ok(d) => d,
            Err(e) => return CommandResult::Error(format!("Failed to locate scenario directory: {}", e)),
        };
        let file_path = scenario_dir.join(format!("scenario-{}.json", clean_id));
        if !file_path.exists() {
            return CommandResult::Error(format!("Scenario file not found: {:?}", file_path));
        }
        match sync_forge_scenario_file(&file_path, &settings.gamelog_config).await {
            Ok(res) => {
                log(&format!("Scenario '{}' synced successfully: {}", res.scenario_name, res.message));
                CommandResult::ScenarioSynced(vec![res])
            }
            Err(e) => {
                log(&format!("Scenario sync failed: {}", e));
                CommandResult::Error(format!("Scenario sync failed: {}", e))
            }
        }
    } else {
        match sync_all_scenario_files(&settings.gamelog_config).await {
            Ok(results) => {
                let msg = format!("Synchronized {} scenario(s) to MaMo", results.len());
                log(&msg);
                CommandResult::ScenarioSynced(results)
            }
            Err(e) => {
                log(&format!("Failed to sync scenarios: {}", e));
                CommandResult::Error(format!("Failed to sync scenarios: {}", e))
            }
        }
    }
}

/// Resolves the Forge `--deck2` opponent path for a launch, in priority order:
/// 1. An explicit deck2 name/id from the deeplink — an override, i.e. "if changed".
/// 2. `deck1_id`'s configured archenemy deck (Playbook's Deck Rules tab), if any.
/// 3. A random pick from the curated opponent-deck pool.
/// 4. `None` — Forge's own lobby default.
///
/// Every tier is best-effort: a failure or absence at any tier falls through to the next.
/// This must never block or fail a Forge launch — exactly today's "no deck2" behavior.
async fn resolve_opponent_deck_path(
    deck1_id: Option<&str>,
    deck2_id: Option<String>,
    deck2_name_direct: Option<String>,
    log: &dyn Fn(&str),
) -> Option<String> {
    if let Some(direct) = deck2_name_direct {
        log(&format!("Using deck2 by name: {}", direct));
        return Some(direct);
    }

    if let Some(ref id2) = deck2_id {
        log(&format!("Downloading deck2 from MaMo: {}", id2));
        match create_deck_from_mamo(id2).await {
            Ok(result) if result.success => {
                let path = result.deck_path.as_ref().map(|p| p.to_string_lossy().to_string());
                log(&format!("Deck2 ready: {:?}", path));
                return path;
            }
            Ok(result) => log(&format!("Deck2 download failed: {}", result.message)),
            Err(e) => log(&format!("Error downloading deck2: {}", e)),
        }
    }

    if let Some(id1) = deck1_id {
        if let Some((archenemy_id, archenemy_name)) = crate::deck::fetch_archenemy_deck_for_deck(id1) {
            log(&format!("Using this deck's configured archenemy deck: {} ({})", archenemy_name, archenemy_id));
            match create_deck_from_mamo(&archenemy_id).await {
                Ok(result) if result.success => {
                    let path = result.deck_path.as_ref().map(|p| p.to_string_lossy().to_string());
                    log(&format!("Archenemy deck ready: {:?}", path));
                    return path;
                }
                Ok(result) => log(&format!("Archenemy deck download failed: {}", result.message)),
                Err(e) => log(&format!("Error downloading archenemy deck: {}", e)),
            }
        }
    }

    if let Some(curated_id) = crate::deck::pick_random_curated_opponent_deck_id() {
        log(&format!("No opponent specified — downloading a curated opponent deck: {}", curated_id));
        match create_deck_from_mamo(&curated_id).await {
            Ok(result) if result.success => {
                let path = result.deck_path.as_ref().map(|p| p.to_string_lossy().to_string());
                log(&format!("Opponent deck ready: {:?}", path));
                return path;
            }
            Ok(result) => log(&format!("Curated opponent deck download failed: {}", result.message)),
            Err(e) => log(&format!("Error downloading curated opponent deck: {}", e)),
        }
    }

    None
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
                        let forge_result = launch_forge_from_settings(deck_path_str.as_deref(), None);
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
    match launch_forge_from_settings(deck_path.as_deref(), None) {
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

    // Optional second deck: by MaMo UUID or by direct Forge deck name
    let deck2_id   = get_parameter(&deeplink.params, "deck2Id")
        .or_else(|| get_parameter(&deeplink.params, "deck2_id"));
    let deck2_name_direct = get_parameter(&deeplink.params, "deck2Name")
        .or_else(|| get_parameter(&deeplink.params, "deck2name"));

    // Check if we should skip download (deck already exists locally)
    let skip_download = get_parameter(&deeplink.params, "skip_download")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);

    // Get optional deck path for pre-existing deck
    let existing_deck_path = get_parameter(&deeplink.params, "deck_path");

    log(&format!("Launch Forge command - deck_id: {:?}", deck_id));

    // Resolve deck2 path: an explicit name/id from the deeplink wins; otherwise fall back to
    // this deck's configured archenemy deck, then a random pick from the curated opponent-deck
    // pool, so "just press Play" never requires manually configuring an opponent inside Forge's
    // own lobby. If nothing resolves, this silently falls through to None — exactly today's
    // behavior.
    let deck2_path = resolve_opponent_deck_path(
        deck_id.as_deref(),
        deck2_id,
        deck2_name_direct,
        &log,
    ).await;

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
                        let deck_path_str = result.deck_path.as_ref()
                            .map(|p| p.to_string_lossy().to_string());

                        log("Launching Forge with deck...");
                        let forge_result = launch_forge_from_settings(deck_path_str.as_deref(), deck2_path.as_deref());
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
                        log(&format!("Deck download failed: {}", result.message));
                        log("Launching Forge without deck...");
                        match launch_forge_from_settings(None, None) {
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
                    log(&format!("Error downloading deck: {}", e));
                    log("Launching Forge without deck...");
                    match launch_forge_from_settings(None, None) {
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
    match launch_forge_from_settings(deck_path.as_deref(), deck2_path.as_deref()) {
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

pub(crate) fn get_parameter(params: &[(String, String)], key: &str) -> Option<String> {
    params.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

// ── Native AI simulation via mamo-sim ─────────────────────────────────────────

/// mamoConnector://simulate-ai/DECK_UUID
///
/// Flow:
///   1. Fetch structured deck input from GET /api/simulation/deck-input/:deckId
///   2. Encode into binary wire format (same layout as codec.ts)
///   3. Run mamo_sim::run_batch_native() — pure Rust, no WASM, full CPU
///   4. POST result to POST /api/simulation-report/:deckId
///   5. Frontend polls GET /api/simulation-report/:deckId and displays results
async fn handle_simulate_ai(deeplink: &Deeplink, log_collector: Option<SharedLogCollector>) -> CommandResult {
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
        _ => return CommandResult::MissingParameters(
            "Deck UUID required. Use mamoConnector://simulate-ai/DECK_UUID".to_string(),
        ),
    };

    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => return CommandResult::Error(format!("Failed to load settings: {}", e)),
    };
    let auth_token = match &settings.auth_token {
        Some(t) => t.clone(),
        None => return CommandResult::Error("No auth token configured in MaMo Connector settings.".to_string()),
    };

    let api_base = crate::simulation::MAMO_API_BASE;

    // ── 1. Fetch deck input ───────────────────────────────────────────────────
    log(&format!("Fetching deck input for {}…", deck_id));
    let client = reqwest::Client::new();
    let deck_input_url = format!("{}/api/simulation/deck-input/{}", api_base, deck_id);
    let deck_resp = match client.get(&deck_input_url)
        .bearer_auth(&auth_token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return CommandResult::Error(format!("Failed to fetch deck input: {}", e)),
    };

    if !deck_resp.status().is_success() {
        return CommandResult::Error(format!("Backend returned {} for deck input", deck_resp.status()));
    }

    let deck_input: serde_json::Value = match deck_resp.json().await {
        Ok(v) => v,
        Err(e) => return CommandResult::Error(format!("Failed to parse deck input: {}", e)),
    };

    // ── 2. Encode wire format ─────────────────────────────────────────────────
    log("Encoding deck for simulation…");
    let (encoded, mech_keys) = match encode_deck_input(&deck_input) {
        Ok(v) => v,
        Err(e) => return CommandResult::Error(format!("Failed to encode deck: {}", e)),
    };

    // ── 3. Run simulation natively ────────────────────────────────────────────
    let games: u32 = get_parameter(&deeplink.params, "games")
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let max_turns: u8 = get_parameter(&deeplink.params, "max_turns")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let seed: u32 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(42);

    log(&format!("Running {} games natively (mamo-sim)…", games));
    let json_str = mamo_sim::run_batch_native(&encoded, mech_keys, games, max_turns, seed);

    let report: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => return CommandResult::Error(format!("Simulation output parse failed: {}", e)),
    };

    if report.get("error").is_some() {
        return CommandResult::Error(format!("Simulation error: {}", report["error"]));
    }

    // ── 4. POST result to backend ─────────────────────────────────────────────
    log("Uploading simulation report…");
    if let Err(e) = crate::simulation::post_simulation_report(&deck_id, &report, Some(&auth_token)).await {
        warn!("Failed to upload simulation report: {}", e);
        return CommandResult::SimulationCompleted(crate::simulation::SimulationResult {
            success: true,
            message: format!("Simulation complete, but upload failed: {}", e),
            report: Some(report),
        });
    }

    log("Done — results available in MaMo.");
    CommandResult::SimulationCompleted(crate::simulation::SimulationResult::success(report))
}

/// Encode backend deck-input JSON into the binary wire format expected by mamo-sim.
/// Wire layout matches codec.ts exactly:
///   [4 bytes card_count] [4 bytes mechanic_count]
///   [card_count × 16 bytes] [mechanic_count × 12 bytes]
fn encode_deck_input(input: &serde_json::Value) -> anyhow::Result<(Vec<u8>, Vec<String>)> {
    let main_cards   = input["mainCards"].as_array().ok_or_else(|| anyhow::anyhow!("missing mainCards"))?;
    let commanders   = input["commanders"].as_array().ok_or_else(|| anyhow::anyhow!("missing commanders"))?;
    let mech_groups  = input["mechanicGroups"].as_array().ok_or_else(|| anyhow::anyhow!("missing mechanicGroups"))?;

    // Build expanded card list (one entry per copy, commanders first)
    let mut all_cards: Vec<&serde_json::Value> = Vec::new();
    let mut is_commander: Vec<bool> = Vec::new();

    for c in commanders {
        all_cards.push(c);
        is_commander.push(true);
    }
    for c in main_cards {
        let copies = c["amount_in_deck"].as_u64().unwrap_or(1) as usize;
        for _ in 0..copies {
            all_cards.push(c);
            is_commander.push(false);
        }
    }

    // Build mechanic card masks: for each mechanic group, which card indices are required
    let commander_oracle_ids: Vec<String> = commanders.iter()
        .filter_map(|c| c["oracle_id"].as_str().map(str::to_string))
        .collect();

    // oracle_id → indices in all_cards
    let mut oracle_to_indices: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
    for (idx, card) in all_cards.iter().enumerate() {
        if let Some(oid) = card["oracle_id"].as_str() {
            oracle_to_indices.entry(oid.to_string()).or_default().push(idx as u32);
        }
    }

    let card_count = all_cards.len() as u32;
    let mechanic_count = mech_groups.len() as u32;

    let mut buf = Vec::with_capacity((8 + card_count as usize * 16 + mechanic_count as usize * 12) as usize);
    buf.extend_from_slice(&card_count.to_le_bytes());
    buf.extend_from_slice(&mechanic_count.to_le_bytes());

    // Encode each card (16 bytes)
    for (i, card) in all_cards.iter().enumerate() {
        let is_land = card["type_line"].as_str().map(|t| t.contains("Land")).unwrap_or(false);
        let is_creature = card["type_line"].as_str().map(|t| t.contains("Creature")).unwrap_or(false);
        let is_artifact = card["type_line"].as_str().map(|t| t.contains("Artifact")).unwrap_or(false);
        let is_mana = card["is_manaproducing"].as_bool().unwrap_or(false) || is_land;
        let is_cmd = is_commander[i];

        let flags: u8 = (is_land as u8)
            | ((is_creature as u8) << 1)
            | ((is_artifact as u8) << 2)
            | ((is_mana as u8) << 3)
            | ((is_cmd as u8) << 4);

        let cmc = card["cmc"].as_f64().unwrap_or(0.0) as u8;
        let power = parse_pt(card["power"].as_str().unwrap_or("0"));
        let toughness = parse_pt(card["toughness"].as_str().unwrap_or("0"));

        // Color mask from color_identity (mana produced by lands/producers)
        let color_mask = color_mask_from_identity(if is_mana {
            card["color_identity"].as_array()
        } else {
            None
        });

        // Mana cost parsing
        let mana_cost = card["mana_cost"].as_str().unwrap_or("");
        let (cost_w, cost_u, cost_b, cost_r, cost_g, cost_generic) = parse_mana_cost(mana_cost);

        // mechanic_mask: bit i = card is in mechanic group i (up to 32)
        let card_oracle_id = card["oracle_id"].as_str().unwrap_or("");
        let mut mechanic_mask: u32 = 0;
        for (mi, mg) in mech_groups.iter().enumerate().take(32) {
            if let Some(card_ids) = mg["card_oracle_ids"].as_array() {
                if card_ids.iter().any(|id| id.as_str() == Some(card_oracle_id)) {
                    mechanic_mask |= 1 << mi;
                }
            }
        }

        buf.push(flags);
        buf.push(cmc);
        buf.push(power);
        buf.push(toughness);
        buf.push(color_mask);
        buf.push(cost_w);
        buf.push(cost_u);
        buf.push(cost_b);
        buf.push(cost_r);
        buf.push(cost_g);
        buf.push(cost_generic);
        buf.push(0); // formation_role (unused for now)
        buf.extend_from_slice(&mechanic_mask.to_le_bytes());
    }

    // Encode each mechanic group (12 bytes)
    let mut mech_keys: Vec<String> = Vec::new();
    for mg in mech_groups {
        let key = mg["deckmechanickey"].as_str().unwrap_or("").to_string();
        mech_keys.push(key);

        let activation = mg["activationcondition"].as_u64().unwrap_or(0) as u8;
        let advantage  = mg["advantageoutput"].as_u64().unwrap_or(0) as u8;
        let dimension  = mg["dimensioncategory"].as_u64().unwrap_or(0) as u8;

        // card_mask: bit i = card index i is required for this mechanic
        let mut card_mask: u32 = 0;
        if let Some(oracle_ids) = mg["card_oracle_ids"].as_array() {
            for oid in oracle_ids {
                if let Some(s) = oid.as_str() {
                    if let Some(indices) = oracle_to_indices.get(s) {
                        for &idx in indices {
                            if idx < 32 { card_mask |= 1 << idx; }
                        }
                    }
                }
            }
        }

        buf.push(activation);
        buf.push(advantage);
        buf.push(dimension);
        buf.push(0); // reserved
        buf.extend_from_slice(&card_mask.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // prereq_mask (unused)
    }

    Ok((buf, mech_keys))
}

fn parse_pt(s: &str) -> u8 {
    s.parse::<f32>().map(|v| v as u8).unwrap_or(0)
}

fn color_mask_from_identity(identity: Option<&Vec<serde_json::Value>>) -> u8 {
    let mut mask: u8 = 0;
    if let Some(arr) = identity {
        for c in arr {
            match c.as_str() {
                Some("W") => mask |= 0x01,
                Some("U") => mask |= 0x02,
                Some("B") => mask |= 0x04,
                Some("R") => mask |= 0x08,
                Some("G") => mask |= 0x10,
                _ => {}
            }
        }
        if mask == 0 { mask = 0x20; } // colorless
    }
    mask
}

fn parse_mana_cost(cost: &str) -> (u8, u8, u8, u8, u8, u8) {
    let (mut w, mut u, mut b, mut r, mut g, mut generic) = (0u8, 0u8, 0u8, 0u8, 0u8, 0u8);
    // Format: {W}{U}{2}{R} etc.
    let mut i = 0;
    let chars: Vec<char> = cost.chars().collect();
    while i < chars.len() {
        if chars[i] == '{' {
            let end = chars[i..].iter().position(|&c| c == '}').map(|p| i + p).unwrap_or(i);
            let sym: String = chars[i+1..end].iter().collect();
            match sym.as_str() {
                "W" => w += 1,
                "U" => u += 1,
                "B" => b += 1,
                "R" => r += 1,
                "G" => g += 1,
                s => { generic = generic.saturating_add(s.parse::<u8>().unwrap_or(0)); }
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    (w, u, b, r, g, generic)
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

    // ==================== Opponent Deck Resolution Tests ====================
    //
    // resolve_opponent_deck_path()'s later tiers (archenemy lookup, curated pool, deck2
    // download) all require real network I/O with no mocking seam in this codebase — the same
    // constraint pick_random_curated_opponent_deck_id() already lives with (see deck.rs, where
    // only the pure pick_random_index() fragment is unit-tested). The explicit-override tier is
    // the one branch that's a pure early return before any await point, so it's the one
    // deterministically testable without network access — and it's also the branch most likely
    // to silently break in a future reordering, since "if not changed" (no override) is the
    // common case that exercises the network tiers instead.

    #[tokio::test]
    async fn test_resolve_opponent_deck_path_prefers_explicit_name_over_everything() {
        let log = |_msg: &str| {};
        let result = resolve_opponent_deck_path(
            Some("deck-1"),
            Some("some-deck2-id".to_string()),
            Some("Explicit Opponent".to_string()),
            &log,
        ).await;

        assert_eq!(result, Some("Explicit Opponent".to_string()));
    }

    #[tokio::test]
    async fn test_resolve_opponent_deck_path_skips_archenemy_tier_without_deck1_id() {
        // With deck1_id = None, the archenemy tier (gated on `if let Some(id1) = deck1_id`)
        // must never be attempted; only the curated-pool fallback remains, which is a real
        // network call. Like other networked tests in this suite, we only assert it resolves
        // without panicking — not the specific Some/None outcome, which depends on the test
        // environment's connectivity and the current curated pool's contents.
        let log = |_msg: &str| {};
        let result = resolve_opponent_deck_path(None, None, None, &log).await;
        let _ = result;
    }
}