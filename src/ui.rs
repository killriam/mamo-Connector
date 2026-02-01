use anyhow::Result;
use eframe::{NativeOptions, egui};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::commands::CommandResult;
use crate::deck::{create_deck_from_moxfield, MoxfieldDeckEntry, DeckStatus, fetch_user_decks_direct, create_deck_from_archidekt, create_deck_from_deckstats, create_deck_from_mamo, parse_archidekt_url, parse_deckstats_url, parse_mamo_url, sync_moxfield_deck, sync_moxfield_user_decks, sync_archidekt_deck, sync_deckstats_deck, sync_mamo_deck, DeckSyncResult, SyncStatus, get_deck_directory_display};
use crate::deeplink::Deeplink;
use crate::gamelog::{GameLogConfig, GameLogProcessResult, ScanSummary, get_default_forge_log_directory, validate_directory, scan_directory, process_new_logs, load_processed_files, save_processed_files};
use crate::registration::{RegistrationOutcome, RegistrationStatus};
use crate::settings::{Settings, SavedLink, SavedLinkType};

#[derive(Clone, PartialEq, Eq)]
enum Tab {
    Status,
    Import,
    Sync,
    GameLogs,
}

/// Detected URL type for auto-detection
#[derive(Clone, PartialEq, Eq, Debug)]
enum UrlType {
    MoxfieldDeck(String),         // Deck ID
    MoxfieldUser(String),         // Username
    ArchidektDeck(String),        // Deck ID
    DeckstatsDeck(String, String), // Owner ID, Deck ID
    MamoDeck(String),             // MaMo Deck UUID
    Unknown,
    Empty,
}

#[derive(Clone, Default)]
struct ImportState {
    is_loading: bool,
    result_message: Option<String>,
    // For user decks
    decks: Vec<MoxfieldDeckEntry>,
    selected_decks: Vec<bool>,
}

/// State for the sync tab
#[derive(Clone, Default)]
struct SyncState {
    is_syncing: bool,
    sync_results: Vec<DeckSyncResult>,
    sync_message: Option<String>,
    // For editing links
    edit_link_id: Option<String>,
    edit_link_name: String,
    // For adding new links
    show_add_dialog: bool,
    add_url_input: String,
    add_name_input: String,
}

/// State for the game log tab
#[derive(Clone, Default)]
struct GameLogState {
    /// Is a scan currently running
    is_scanning: bool,
    /// Is background scanning enabled
    background_enabled: bool,
    /// Directory input for editing
    directory_input: String,
    /// Is the directory valid
    directory_valid: bool,
    /// Number of files in directory
    file_count: Option<usize>,
    /// Status message
    status_message: Option<String>,
    /// Last scan results
    scan_results: Vec<GameLogProcessResult>,
    /// Summary from last scan
    last_scan_summary: Option<ScanSummary>,
    /// Processed files set
    processed_files: HashSet<String>,
}

#[derive(Clone)]
struct AppState {
    registration: RegistrationOutcome,
    args: Vec<String>,
    deeplink: Option<Deeplink>,
    command_result: Option<CommandResult>,
}

pub fn launch(
    registration: RegistrationOutcome,
    args: Vec<String>,
    deeplink: Option<Deeplink>,
    command_result: Option<CommandResult>,
) -> Result<()> {
    let state = AppState {
        registration,
        args,
        deeplink,
        command_result,
    };

    let native_options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        "Mamo Connector",
        native_options,
        Box::new(move |cc| {
            // Force light theme with explicit text colors
            let mut visuals = egui::Visuals::light();
            visuals.override_text_color = Some(egui::Color32::BLACK);
            cc.egui_ctx.set_visuals(visuals);
            
            // Ensure default fonts are loaded with larger sizes
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles = [
                (egui::TextStyle::Heading, egui::FontId::new(28.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Body, egui::FontId::new(18.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Monospace, egui::FontId::new(16.0, egui::FontFamily::Monospace)),
                (egui::TextStyle::Button, egui::FontId::new(18.0, egui::FontFamily::Proportional)),
                (egui::TextStyle::Small, egui::FontId::new(14.0, egui::FontFamily::Proportional)),
            ].into();
            cc.egui_ctx.set_style(style);
            
            Ok(Box::new(LauncherApp::new(state.clone())))
        }),
    ).map_err(|e| anyhow::anyhow!("Failed to run native app: {}", e))?;

    Ok(())
}

struct LauncherApp {
    state: AppState,
    url_input: String,
    current_tab: Tab,
    import_state: Arc<Mutex<ImportState>>,
    sync_state: Arc<Mutex<SyncState>>,
    gamelog_state: Arc<Mutex<GameLogState>>,
    settings: Arc<Mutex<Settings>>,
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        // Load settings
        let settings = Settings::load().unwrap_or_default();
        
        // Load processed files for game log
        let processed_files = load_processed_files().unwrap_or_default();
        
        // Initialize gamelog state with settings
        let gamelog_state = GameLogState {
            directory_input: settings.gamelog_config.watch_directory.clone(),
            directory_valid: validate_directory(&settings.gamelog_config.watch_directory).unwrap_or(false),
            background_enabled: settings.gamelog_config.background_scan_enabled,
            processed_files,
            ..Default::default()
        };
        
        Self {
            state,
            url_input: String::new(),
            current_tab: Tab::Import,
            import_state: Arc::new(Mutex::new(ImportState::default())),
            sync_state: Arc::new(Mutex::new(SyncState::default())),
            gamelog_state: Arc::new(Mutex::new(gamelog_state)),
            settings: Arc::new(Mutex::new(settings)),
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::WHITE))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
                ui.visuals_mut().panel_fill = egui::Color32::WHITE;
                
                ui.heading("Mamo Connector");
                ui.separator();
                
                // Tab bar
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.current_tab == Tab::Status, "Status").clicked() {
                        self.current_tab = Tab::Status;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Import, "Import Decks").clicked() {
                        self.current_tab = Tab::Import;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Sync, "Sync").clicked() {
                        self.current_tab = Tab::Sync;
                    }
                    if ui.selectable_label(self.current_tab == Tab::GameLogs, "Game Logs").clicked() {
                        self.current_tab = Tab::GameLogs;
                    }
                });
                ui.separator();
                
                // Tab content
                match self.current_tab {
                    Tab::Status => self.render_status_tab(ui),
                    Tab::Import => self.render_import_tab(ui, ctx),
                    Tab::Sync => self.render_sync_tab(ui, ctx),
                    Tab::GameLogs => self.render_gamelog_tab(ui, ctx),
                }
            });
    }
}

impl LauncherApp {
    fn render_status_tab(&self, ui: &mut egui::Ui) {
        ui.label("Registration Status");
        let status_text = match self.state.registration.status {
            RegistrationStatus::Registered => {
                egui::RichText::new("[OK] Custom URI scheme registered")
                    .color(egui::Color32::from_rgb(0, 128, 0))
            }
            RegistrationStatus::Failed => {
                egui::RichText::new("[FAIL] Failed to register custom URI scheme")
                    .color(egui::Color32::from_rgb(176, 0, 32))
            }
            RegistrationStatus::Skipped => {
                egui::RichText::new("[SKIP] Scheme registration not supported")
                    .color(egui::Color32::from_rgb(196, 112, 0))
            }
        };
        ui.label(status_text);
        ui.small(&self.state.registration.message);
        
        ui.separator();
        
        // Show deeplink info if available
        if let Some(ref deeplink) = self.state.deeplink {
            ui.label(egui::RichText::new("Deeplink Received:").strong());
            if let Some(ref deck_id) = deeplink.deck_id {
                ui.label(format!("  Deck ID: {}", deck_id));
            }
            if let Some(ref username) = deeplink.username {
                ui.label(format!("  Username: {}", username));
            }
            ui.small(format!("  Raw URI: {}", &deeplink.raw));
        }
        
        // Show command result if available
        if let Some(ref result) = self.state.command_result {
            ui.separator();
            ui.label(egui::RichText::new("Command Result:").strong());
            match result {
                CommandResult::DeckCreated(deck_result) => {
                    ui.label(egui::RichText::new(&deck_result.message)
                        .color(egui::Color32::from_rgb(0, 128, 0)));
                }
                CommandResult::Error(err) => {
                    ui.label(egui::RichText::new(err)
                        .color(egui::Color32::from_rgb(176, 0, 32)));
                }
                CommandResult::UserDecksImported(result) => {
                    let success_count = result.imported_decks.iter().filter(|d| d.success).count();
                    ui.label(format!("Imported {} of {} decks", success_count, result.total_decks));
                    ui.small(&result.message);
                }
                CommandResult::UserDecksList(decks) => {
                    ui.label(format!("Found {} decks for user", decks.len()));
                    for deck in decks.iter().take(5) {
                        let format_str = deck.format.as_deref().unwrap_or("Unknown");
                        ui.small(format!("  - {} ({})", deck.name, format_str));
                    }
                    if decks.len() > 5 {
                        ui.small(format!("  ... and {} more", decks.len() - 5));
                    }
                }
                CommandResult::UnknownAction(action) => {
                    ui.label(egui::RichText::new(format!("Unknown action: {}", action))
                        .color(egui::Color32::from_rgb(176, 0, 32)));
                }
                CommandResult::MissingParameters(msg) => {
                    ui.label(egui::RichText::new(format!("Missing parameters: {}", msg))
                        .color(egui::Color32::from_rgb(176, 0, 32)));
                }
            }
        }
    }
    
    fn detect_url_type(&self, url: &str) -> UrlType {
        let url = url.trim();
        
        if url.is_empty() {
            return UrlType::Empty;
        }
        
        // Moxfield user: https://moxfield.com/users/USERNAME
        if url.contains("moxfield.com/users/") {
            if let Some(username) = url.split("/users/").nth(1) {
                let username = username.split(&['/', '?', '#'][..]).next().unwrap_or(username);
                if !username.is_empty() {
                    return UrlType::MoxfieldUser(username.to_string());
                }
            }
        }
        
        // Moxfield deck: https://moxfield.com/decks/DECK_ID
        if url.contains("moxfield.com/decks/") {
            if let Some(deck_id) = url.split("/decks/").nth(1) {
                let deck_id = deck_id.split(&['/', '?', '#'][..]).next().unwrap_or(deck_id);
                if !deck_id.is_empty() {
                    return UrlType::MoxfieldDeck(deck_id.to_string());
                }
            }
        }
        
        // Archidekt: https://archidekt.com/decks/12345678/deck_name
        if let Some(deck_id) = parse_archidekt_url(url) {
            return UrlType::ArchidektDeck(deck_id);
        }
        
        // Deckstats: https://deckstats.net/decks/123456/7890123-deck_name
        if let Some((owner_id, deck_id)) = parse_deckstats_url(url) {
            return UrlType::DeckstatsDeck(owner_id, deck_id);
        }
        
        // MaMo: https://ma-mo-frontend.vercel.app/deckId=UUID or similar
        if let Some(deck_uuid) = parse_mamo_url(url) {
            return UrlType::MamoDeck(deck_uuid);
        }
        
        // Plain Moxfield deck ID (no URL)
        if !url.contains("://") && !url.contains(".") && url.len() > 5 {
            return UrlType::MoxfieldDeck(url.to_string());
        }
        
        UrlType::Unknown
    }
    
    fn render_import_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new("Import Decks").strong());
        ui.add_space(5.0);
        
        // Description
        ui.label("Paste a URL or username/deck ID to import decks. Supported sources:");
        ui.add_space(3.0);
        
        egui::Grid::new("sources_grid")
            .num_columns(2)
            .spacing([20.0, 4.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Moxfield Deck:").strong());
                ui.label("https://moxfield.com/decks/DECK_ID or just the deck ID");
                ui.end_row();
                
                ui.label(egui::RichText::new("Moxfield User:").strong());
                ui.label("https://moxfield.com/users/USERNAME → lists all user decks");
                ui.end_row();
                
                ui.label(egui::RichText::new("Archidekt:").strong());
                ui.label("https://archidekt.com/decks/12345678/deck_name");
                ui.end_row();
                
                ui.label(egui::RichText::new("Deckstats:").strong());
                ui.label("https://deckstats.net/decks/123456/7890123-deck_name");
                ui.end_row();
                
                ui.label(egui::RichText::new("MaMo:").strong());
                ui.label("https://ma-mo-frontend.vercel.app/deckId=UUID");
                ui.end_row();
            });
        
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);
        
        // URL input
        ui.horizontal(|ui| {
            ui.label("URL / ID:");
            let response = ui.add(egui::TextEdit::singleline(&mut self.url_input).desired_width(500.0));
            if response.changed() {
                // Clear state when URL changes
                let mut state = self.import_state.lock().unwrap();
                state.decks.clear();
                state.selected_decks.clear();
                state.result_message = None;
            }
        });
        
        ui.add_space(10.0);
        
        // Detect URL type
        let url_type = self.detect_url_type(&self.url_input);
        
        // Show detection result
        match &url_type {
            UrlType::MoxfieldDeck(id) => {
                ui.label(egui::RichText::new(format!("✓ Moxfield Deck: {}", id)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::MoxfieldUser(username) => {
                ui.label(egui::RichText::new(format!("✓ Moxfield User: {} → will list all decks", username)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::ArchidektDeck(id) => {
                ui.label(egui::RichText::new(format!("✓ Archidekt Deck: {}", id)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::DeckstatsDeck(owner, deck) => {
                ui.label(egui::RichText::new(format!("✓ Deckstats Deck: {}/{}", owner, deck)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::MamoDeck(uuid) => {
                ui.label(egui::RichText::new(format!("✓ MaMo Deck: {}", uuid)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::Unknown => {
                ui.label(egui::RichText::new("⚠ Unknown URL format").color(egui::Color32::from_rgb(200, 100, 0)));
            }
            UrlType::Empty => {}
        }
        
        ui.add_space(10.0);
        
        // Get current state
        let (is_loading, result_message, has_decks, decks_info) = {
            let state = self.import_state.lock().unwrap();
            (
                state.is_loading,
                state.result_message.clone(),
                !state.decks.is_empty(),
                state.decks.iter().enumerate().map(|(i, d)| {
                    (i, d.public_id.clone(), d.name.clone(), d.format.clone(), 
                     state.selected_decks.get(i).copied().unwrap_or(false),
                     d.local_status.clone(), d.local_date.clone(),
                     d.last_updated_at_utc.as_ref().and_then(|dt| dt.split('T').next()).map(|s| s.to_string()))
                }).collect::<Vec<_>>(),
            )
        };
        
        // Main action button based on URL type
        match &url_type {
            UrlType::MoxfieldUser(username) => {
                if !has_decks {
                    // Show "Fetch Decks" button
                    if ui.add_enabled(!is_loading, egui::Button::new("Fetch User Decks")).clicked() {
                        self.fetch_user_decks(username.clone(), ctx);
                    }
                }
            }
            UrlType::MoxfieldDeck(_) | UrlType::ArchidektDeck(_) | UrlType::DeckstatsDeck(_, _) | UrlType::MamoDeck(_) => {
                if ui.add_enabled(!is_loading, egui::Button::new("Import Deck")).clicked() {
                    self.import_single_deck(&url_type, ctx);
                }
            }
            _ => {}
        }
        
        if is_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading...");
            });
        }
        
        // Show user decks list if available
        if has_decks {
            ui.separator();
            ui.add_space(5.0);
            
            // Selection controls
            ui.horizontal(|ui| {
                if ui.button("Select All").clicked() {
                    let mut state = self.import_state.lock().unwrap();
                    for selected in &mut state.selected_decks {
                        *selected = true;
                    }
                }
                if ui.button("Select None").clicked() {
                    let mut state = self.import_state.lock().unwrap();
                    for selected in &mut state.selected_decks {
                        *selected = false;
                    }
                }
                if ui.button("Select New/Updated").clicked() {
                    let mut state = self.import_state.lock().unwrap();
                    let indices_to_select: Vec<usize> = state.decks.iter().enumerate()
                        .filter(|(_, deck)| deck.local_status.as_ref() != Some(&DeckStatus::UpToDate))
                        .map(|(i, _)| i)
                        .collect();
                    for (i, selected) in state.selected_decks.iter_mut().enumerate() {
                        *selected = indices_to_select.contains(&i);
                    }
                }
                
                let selected_count = decks_info.iter().filter(|(_, _, _, _, s, _, _, _)| *s).count();
                ui.label(format!("{}/{} selected", selected_count, decks_info.len()));
            });
            
            // Status legend
            ui.horizontal(|ui| {
                ui.label("Status: ");
                ui.label(egui::RichText::new("● New").color(egui::Color32::from_rgb(0, 150, 0)));
                ui.label(egui::RichText::new("● Needs Update").color(egui::Color32::from_rgb(255, 165, 0)));
                ui.label(egui::RichText::new("● Up to date").color(egui::Color32::from_rgb(100, 100, 100)));
            });
            
            ui.add_space(5.0);
            
            // Deck list with scrolling
            let available_height = ui.available_height() - 60.0;
            egui::ScrollArea::vertical()
                .max_height(available_height.max(100.0))
                .show(ui, |ui| {
                    for (i, _deck_id, name, format, is_selected, local_status, local_date, moxfield_date) in &decks_info {
                        let mut selected = *is_selected;
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut selected, "").changed() {
                                let mut state = self.import_state.lock().unwrap();
                                if let Some(s) = state.selected_decks.get_mut(*i) {
                                    *s = selected;
                                }
                            }
                            
                            // Status indicator
                            let (status_char, status_color) = match local_status {
                                Some(DeckStatus::New) => ("●", egui::Color32::from_rgb(0, 150, 0)),
                                Some(DeckStatus::NeedsUpdate) => ("●", egui::Color32::from_rgb(255, 165, 0)),
                                Some(DeckStatus::UpToDate) => ("●", egui::Color32::from_rgb(100, 100, 100)),
                                None => ("?", egui::Color32::GRAY),
                            };
                            ui.label(egui::RichText::new(status_char).color(status_color));
                            
                            ui.label(name);
                            let format_str = format.as_deref().unwrap_or("Unknown");
                            ui.label(egui::RichText::new(format!("[{}]", format_str)).weak());
                            
                            if let Some(mox_date) = moxfield_date {
                                ui.label(egui::RichText::new(format!("Moxfield: {}", mox_date)).weak().small());
                            }
                            if let Some(loc_date) = local_date {
                                ui.label(egui::RichText::new(format!("Local: {}", loc_date)).weak().small());
                            }
                        });
                    }
                });
            
            ui.add_space(10.0);
            
            // Import selected button
            let selected_count = decks_info.iter().filter(|(_, _, _, _, s, _, _, _)| *s).count();
            let selected_deck_ids: Vec<String> = decks_info.iter()
                .filter(|(_, _, _, _, s, _, _, _)| *s)
                .map(|(_, id, _, _, _, _, _, _)| id.clone())
                .collect();
            
            if ui.add_enabled(selected_count > 0 && !is_loading, egui::Button::new(format!("Import {} Selected Decks", selected_count))).clicked() {
                self.import_selected_decks(selected_deck_ids, ctx);
            }
        }
        
        // Show result message
        if let Some(msg) = result_message {
            ui.separator();
            let color = if msg.starts_with("Error") || msg.contains("failed") {
                egui::Color32::from_rgb(176, 0, 32)
            } else if msg.contains("Successfully") || msg.contains("Imported") {
                egui::Color32::from_rgb(0, 128, 0)
            } else {
                egui::Color32::DARK_GRAY
            };
            ui.label(egui::RichText::new(msg).color(color));
        }
    }
    
    fn fetch_user_decks(&mut self, username: String, ctx: &egui::Context) {
        let state_clone = Arc::clone(&self.import_state);
        let ctx_clone = ctx.clone();
        
        {
            let mut state = self.import_state.lock().unwrap();
            state.is_loading = true;
            state.result_message = None;
            state.decks.clear();
            state.selected_decks.clear();
        }
        
        tokio::spawn(async move {
            let result = fetch_user_decks_direct(&username);
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            
            match result {
                Ok(decks) => {
                    state.selected_decks = vec![false; decks.len()];
                    state.result_message = Some(format!("Found {} decks for {}", decks.len(), username));
                    state.decks = decks;
                }
                Err(e) => {
                    state.result_message = Some(format!("Error: Failed to fetch decks: {}", e));
                }
            }
            
            ctx_clone.request_repaint();
        });
    }
    
    fn import_single_deck(&mut self, url_type: &UrlType, ctx: &egui::Context) {
        let url_type = url_type.clone();
        let state_clone = Arc::clone(&self.import_state);
        let ctx_clone = ctx.clone();
        
        {
            let mut state = self.import_state.lock().unwrap();
            state.is_loading = true;
            state.result_message = Some("Fetching deck...".to_string());
        }
        
        tokio::spawn(async move {
            let result = match url_type {
                UrlType::MoxfieldDeck(deck_id) => {
                    create_deck_from_moxfield(&deck_id).await
                }
                UrlType::ArchidektDeck(deck_id) => {
                    create_deck_from_archidekt(&deck_id).await
                }
                UrlType::DeckstatsDeck(owner_id, deck_id) => {
                    create_deck_from_deckstats(&owner_id, &deck_id).await
                }
                UrlType::MamoDeck(deck_uuid) => {
                    create_deck_from_mamo(&deck_uuid).await
                }
                _ => Err(anyhow::anyhow!("Invalid URL type for single deck import"))
            };
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            
            match result {
                Ok(deck_result) => {
                    state.result_message = Some(deck_result.message);
                }
                Err(e) => {
                    state.result_message = Some(format!("Error: {}", e));
                }
            }
            
            ctx_clone.request_repaint();
        });
    }
    
    fn import_selected_decks(&mut self, deck_ids: Vec<String>, ctx: &egui::Context) {
        let state_clone = Arc::clone(&self.import_state);
        let ctx_clone = ctx.clone();
        let total = deck_ids.len();
        
        {
            let mut state = self.import_state.lock().unwrap();
            state.is_loading = true;
            state.result_message = Some(format!("Importing {} decks...", total));
        }
        
        tokio::spawn(async move {
            let mut success_count = 0;
            let mut fail_count = 0;
            
            for deck_id in &deck_ids {
                let result = create_deck_from_moxfield(deck_id).await;
                
                match result {
                    Ok(_) => success_count += 1,
                    Err(e) => {
                        log::warn!("Failed to import deck {}: {}", deck_id, e);
                        fail_count += 1;
                    }
                }
            }
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            state.result_message = Some(format!(
                "Imported {} of {} decks ({} failed)",
                success_count, total, fail_count
            ));
            
            ctx_clone.request_repaint();
        });
    }

    // ==================== Sync Tab ====================

    fn render_sync_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new("Deck Synchronization").strong());
        ui.add_space(5.0);
        ui.label(egui::RichText::new(format!("Deck folder: {}", get_deck_directory_display())).weak().small());
        ui.add_space(10.0);
        
        // Get current state
        let (is_syncing, sync_message, sync_results) = {
            let state = self.sync_state.lock().unwrap();
            (state.is_syncing, state.sync_message.clone(), state.sync_results.clone())
        };
        
        let (show_add_dialog, edit_link_id) = {
            let state = self.sync_state.lock().unwrap();
            (state.show_add_dialog, state.edit_link_id.clone())
        };
        
        // Main sync button
        ui.horizontal(|ui| {
            if ui.add_enabled(!is_syncing, egui::Button::new("🔄 Sync All Decks")).clicked() {
                self.sync_all_decks(ctx);
            }
            
            if ui.button("➕ Add Link").clicked() {
                let mut state = self.sync_state.lock().unwrap();
                state.show_add_dialog = true;
                state.add_url_input.clear();
                state.add_name_input.clear();
            }
            
            if is_syncing {
                ui.spinner();
                ui.label("Syncing...");
            }
        });
        
        // Add dialog
        if show_add_dialog {
            self.render_add_link_dialog(ui, ctx);
        }
        
        ui.add_space(10.0);
        ui.separator();
        
        // Saved links list
        ui.label(egui::RichText::new("Saved Links").strong());
        ui.add_space(5.0);
        
        let saved_links: Vec<SavedLink> = {
            let settings = self.settings.lock().unwrap();
            settings.saved_links.clone()
        };
        
        if saved_links.is_empty() {
            ui.label(egui::RichText::new("No saved links yet. Add a deck or user link to enable sync.").weak());
        } else {
            let available_height = if !sync_results.is_empty() { 
                ui.available_height() / 2.0 - 30.0 
            } else { 
                ui.available_height() - 100.0 
            };
            
            egui::ScrollArea::vertical()
                .id_source("saved_links_scroll")
                .max_height(available_height.max(100.0))
                .show(ui, |ui: &mut egui::Ui| {
                    let mut link_to_delete: Option<String> = None;
                    
                    for link in &saved_links {
                        let is_editing = edit_link_id.as_ref() == Some(&link.id);
                        
                        ui.horizontal(|ui: &mut egui::Ui| {
                            // Enable/disable checkbox
                            let mut enabled = link.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                let mut settings = self.settings.lock().unwrap();
                                settings.update_link(&link.id, link.name.clone(), enabled);
                                let _ = settings.save();
                            }
                            
                            // Type icon
                            let type_icon = match link.link_type {
                                SavedLinkType::MoxfieldDeck => "🃏",
                                SavedLinkType::MoxfieldUser => "👤",
                                SavedLinkType::ArchidektDeck => "📚",
                                SavedLinkType::DeckstatsDeck => "📊",
                                SavedLinkType::MamoDeck => "🎯",
                            };
                            ui.label(type_icon);
                            
                            if is_editing {
                                // Edit mode
                                let mut edit_name = {
                                    let state = self.sync_state.lock().unwrap();
                                    state.edit_link_name.clone()
                                };
                                
                                let response = ui.add(egui::TextEdit::singleline(&mut edit_name).desired_width(200.0));
                                
                                if response.changed() {
                                    let mut state = self.sync_state.lock().unwrap();
                                    state.edit_link_name = edit_name.clone();
                                }
                                
                                if ui.button("✓").clicked() {
                                    let mut settings = self.settings.lock().unwrap();
                                    settings.update_link(&link.id, edit_name, link.enabled);
                                    let _ = settings.save();
                                    
                                    let mut state = self.sync_state.lock().unwrap();
                                    state.edit_link_id = None;
                                }
                                
                                if ui.button("✗").clicked() {
                                    let mut state = self.sync_state.lock().unwrap();
                                    state.edit_link_id = None;
                                }
                            } else {
                                // Display mode
                                ui.label(&link.name);
                                ui.label(egui::RichText::new(format!("[{}]", link.link_type.display_name())).weak().small());
                                
                                if let Some(last_synced) = &link.last_synced {
                                    ui.label(egui::RichText::new(format!("Last sync: {}", last_synced)).weak().small());
                                }
                                
                                // Edit button
                                if ui.small_button("✏").clicked() {
                                    let mut state = self.sync_state.lock().unwrap();
                                    state.edit_link_id = Some(link.id.clone());
                                    state.edit_link_name = link.name.clone();
                                }
                                
                                // Delete button
                                if ui.small_button("🗑").clicked() {
                                    link_to_delete = Some(link.id.clone());
                                }
                            }
                        });
                    }
                    
                    // Process delete outside the loop
                    if let Some(id) = link_to_delete {
                        let mut settings = self.settings.lock().unwrap();
                        settings.remove_link(&id);
                        let _ = settings.save();
                    }
                });
        }
        
        // Sync results
        if !sync_results.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new("Sync Results").strong());
            
            let updated = sync_results.iter().filter(|r| r.status == SyncStatus::Updated).count();
            let new = sync_results.iter().filter(|r| r.status == SyncStatus::NewDownloaded).count();
            let up_to_date = sync_results.iter().filter(|r| r.status == SyncStatus::AlreadyUpToDate).count();
            let failed = sync_results.iter().filter(|r| r.status == SyncStatus::Failed).count();
            
            ui.horizontal(|ui| {
                if updated > 0 {
                    ui.label(egui::RichText::new(format!("📥 {} updated", updated)).color(egui::Color32::from_rgb(0, 128, 0)));
                }
                if new > 0 {
                    ui.label(egui::RichText::new(format!("🆕 {} new", new)).color(egui::Color32::from_rgb(0, 100, 200)));
                }
                if up_to_date > 0 {
                    ui.label(egui::RichText::new(format!("✓ {} up to date", up_to_date)).color(egui::Color32::GRAY));
                }
                if failed > 0 {
                    ui.label(egui::RichText::new(format!("❌ {} failed", failed)).color(egui::Color32::from_rgb(200, 0, 0)));
                }
            });
            
            egui::ScrollArea::vertical()
                .id_source("sync_results_scroll")
                .max_height(150.0)
                .show(ui, |ui: &mut egui::Ui| {
                    for result in &sync_results {
                        let (icon, color) = match result.status {
                            SyncStatus::Updated => ("📥", egui::Color32::from_rgb(0, 128, 0)),
                            SyncStatus::NewDownloaded => ("🆕", egui::Color32::from_rgb(0, 100, 200)),
                            SyncStatus::AlreadyUpToDate => ("✓", egui::Color32::GRAY),
                            SyncStatus::Failed => ("❌", egui::Color32::from_rgb(200, 0, 0)),
                            SyncStatus::Skipped => ("⏭", egui::Color32::from_rgb(150, 150, 0)),
                        };
                        ui.label(egui::RichText::new(format!("{} {}", icon, result.message)).color(color).small());
                    }
                });
        }
        
        // Show sync message
        if let Some(msg) = sync_message {
            ui.add_space(5.0);
            let color = if msg.contains("Error") || msg.contains("failed") {
                egui::Color32::from_rgb(176, 0, 32)
            } else {
                egui::Color32::from_rgb(0, 128, 0)
            };
            ui.label(egui::RichText::new(msg).color(color));
        }
    }

    fn render_add_link_dialog(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        egui::Frame::default()
            .fill(egui::Color32::from_rgb(245, 245, 245))
            .inner_margin(10.0)
            .rounding(5.0)
            .stroke(egui::Stroke::new(1.0, egui::Color32::GRAY))
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Add New Link").strong());
                ui.add_space(5.0);
                
                // URL input
                ui.horizontal(|ui| {
                    ui.label("URL / ID:");
                    let mut url = {
                        let state = self.sync_state.lock().unwrap();
                        state.add_url_input.clone()
                    };
                    if ui.add(egui::TextEdit::singleline(&mut url).desired_width(400.0)).changed() {
                        let mut state = self.sync_state.lock().unwrap();
                        state.add_url_input = url;
                    }
                });
                
                // Detect URL type
                let url_type = {
                    let state = self.sync_state.lock().unwrap();
                    self.detect_url_type(&state.add_url_input)
                };
                
                // Show detected type
                match &url_type {
                    UrlType::MoxfieldDeck(id) => {
                        ui.label(egui::RichText::new(format!("✓ Moxfield Deck: {}", id)).color(egui::Color32::from_rgb(0, 128, 0)));
                    }
                    UrlType::MoxfieldUser(username) => {
                        ui.label(egui::RichText::new(format!("✓ Moxfield User: {} (all decks)", username)).color(egui::Color32::from_rgb(0, 128, 0)));
                    }
                    UrlType::ArchidektDeck(id) => {
                        ui.label(egui::RichText::new(format!("✓ Archidekt Deck: {}", id)).color(egui::Color32::from_rgb(0, 128, 0)));
                    }
                    UrlType::DeckstatsDeck(owner, deck) => {
                        ui.label(egui::RichText::new(format!("✓ Deckstats Deck: {}/{}", owner, deck)).color(egui::Color32::from_rgb(0, 128, 0)));
                    }
                    UrlType::MamoDeck(uuid) => {
                        ui.label(egui::RichText::new(format!("✓ MaMo Deck: {}", uuid)).color(egui::Color32::from_rgb(0, 128, 0)));
                    }
                    UrlType::Unknown => {
                        ui.label(egui::RichText::new("⚠ Unknown URL format").color(egui::Color32::from_rgb(200, 100, 0)));
                    }
                    UrlType::Empty => {}
                }
                
                // Name input
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let mut name = {
                        let state = self.sync_state.lock().unwrap();
                        state.add_name_input.clone()
                    };
                    if ui.add(egui::TextEdit::singleline(&mut name).desired_width(300.0).hint_text("Optional - auto-detected if empty")).changed() {
                        let mut state = self.sync_state.lock().unwrap();
                        state.add_name_input = name;
                    }
                });
                
                ui.add_space(5.0);
                
                // Buttons
                ui.horizontal(|ui| {
                    let can_add = !matches!(url_type, UrlType::Empty | UrlType::Unknown);
                    
                    if ui.add_enabled(can_add, egui::Button::new("Add")).clicked() {
                        self.add_saved_link(&url_type);
                    }
                    
                    if ui.button("Cancel").clicked() {
                        let mut state = self.sync_state.lock().unwrap();
                        state.show_add_dialog = false;
                    }
                });
            });
    }

    fn add_saved_link(&mut self, url_type: &UrlType) {
        let (name_input, _url_input) = {
            let state = self.sync_state.lock().unwrap();
            (state.add_name_input.clone(), state.add_url_input.clone())
        };
        
        let link = match url_type {
            UrlType::MoxfieldDeck(id) => {
                let name = if name_input.is_empty() { 
                    format!("Moxfield Deck {}", id) 
                } else { 
                    name_input 
                };
                SavedLink::new(name, SavedLinkType::MoxfieldDeck, id.clone())
            }
            UrlType::MoxfieldUser(username) => {
                let name = if name_input.is_empty() { 
                    format!("Moxfield User: {}", username) 
                } else { 
                    name_input 
                };
                SavedLink::new(name, SavedLinkType::MoxfieldUser, username.clone())
            }
            UrlType::ArchidektDeck(id) => {
                let name = if name_input.is_empty() { 
                    format!("Archidekt Deck {}", id) 
                } else { 
                    name_input 
                };
                SavedLink::new(name, SavedLinkType::ArchidektDeck, id.clone())
            }
            UrlType::DeckstatsDeck(owner, deck) => {
                let name = if name_input.is_empty() { 
                    format!("Deckstats Deck {}", deck) 
                } else { 
                    name_input 
                };
                SavedLink::new_deckstats(name, owner.clone(), deck.clone())
            }
            UrlType::MamoDeck(uuid) => {
                let name = if name_input.is_empty() { 
                    format!("MaMo Deck {}", &uuid[..8]) 
                } else { 
                    name_input 
                };
                SavedLink::new(name, SavedLinkType::MamoDeck, uuid.clone())
            }
            _ => return,
        };
        
        {
            let mut settings = self.settings.lock().unwrap();
            settings.add_link(link);
            let _ = settings.save();
        }
        
        {
            let mut state = self.sync_state.lock().unwrap();
            state.show_add_dialog = false;
            state.add_url_input.clear();
            state.add_name_input.clear();
        }
    }

    fn sync_all_decks(&mut self, ctx: &egui::Context) {
        let settings_clone = Arc::clone(&self.settings);
        let sync_state_clone = Arc::clone(&self.sync_state);
        let ctx_clone = ctx.clone();
        
        // Get enabled links
        let links: Vec<SavedLink> = {
            let settings = self.settings.lock().unwrap();
            settings.get_enabled_links().iter().map(|l| (*l).clone()).collect()
        };
        
        if links.is_empty() {
            let mut state = self.sync_state.lock().unwrap();
            state.sync_message = Some("No enabled links to sync".to_string());
            return;
        }
        
        {
            let mut state = self.sync_state.lock().unwrap();
            state.is_syncing = true;
            state.sync_results.clear();
            state.sync_message = Some(format!("Syncing {} link(s)...", links.len()));
        }
        
        tokio::spawn(async move {
            let mut all_results = Vec::new();
            
            for link in &links {
                let results = match link.link_type {
                    SavedLinkType::MoxfieldDeck => {
                        match sync_moxfield_deck(&link.url).await {
                            Ok(result) => vec![result],
                            Err(e) => vec![DeckSyncResult::failed(link.name.clone(), e.to_string())],
                        }
                    }
                    SavedLinkType::MoxfieldUser => {
                        match sync_moxfield_user_decks(&link.url).await {
                            Ok(results) => results,
                            Err(e) => vec![DeckSyncResult::failed(link.name.clone(), e.to_string())],
                        }
                    }
                    SavedLinkType::ArchidektDeck => {
                        match sync_archidekt_deck(&link.url).await {
                            Ok(result) => vec![result],
                            Err(e) => vec![DeckSyncResult::failed(link.name.clone(), e.to_string())],
                        }
                    }
                    SavedLinkType::DeckstatsDeck => {
                        let owner_id = link.owner_id.as_deref().unwrap_or("");
                        match sync_deckstats_deck(owner_id, &link.url).await {
                            Ok(result) => vec![result],
                            Err(e) => vec![DeckSyncResult::failed(link.name.clone(), e.to_string())],
                        }
                    }
                    SavedLinkType::MamoDeck => {
                        match sync_mamo_deck(&link.url).await {
                            Ok(result) => vec![result],
                            Err(e) => vec![DeckSyncResult::failed(link.name.clone(), e.to_string())],
                        }
                    }
                };
                
                all_results.extend(results);
                
                // Mark link as synced
                {
                    let mut settings = settings_clone.lock().unwrap();
                    settings.mark_link_synced(&link.id);
                    let _ = settings.save();
                }
            }
            
            let updated = all_results.iter().filter(|r| r.status == SyncStatus::Updated).count();
            let new = all_results.iter().filter(|r| r.status == SyncStatus::NewDownloaded).count();
            let failed = all_results.iter().filter(|r| r.status == SyncStatus::Failed).count();
            
            {
                let mut state = sync_state_clone.lock().unwrap();
                state.is_syncing = false;
                state.sync_results = all_results;
                state.sync_message = Some(format!(
                    "Sync complete: {} updated, {} new, {} failed",
                    updated, new, failed
                ));
            }
            
            ctx_clone.request_repaint();
        });
    }

    // ==================== Game Log Tab ====================

    fn render_gamelog_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new("Game Log Reader").strong());
        ui.add_space(5.0);
        ui.label("Monitor Forge game logs and upload them to MaMo for analysis.");
        ui.add_space(10.0);
        
        // Get current state
        let (is_scanning, background_enabled, directory_input, directory_valid, file_count, status_message) = {
            let state = self.gamelog_state.lock().unwrap();
            (
                state.is_scanning,
                state.background_enabled,
                state.directory_input.clone(),
                state.directory_valid,
                state.file_count,
                state.status_message.clone(),
            )
        };
        
        // Directory configuration section
        ui.group(|ui| {
            ui.label(egui::RichText::new("📁 Directory Configuration").strong());
            ui.add_space(5.0);
            
            ui.horizontal(|ui| {
                ui.label("Watch Directory:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.gamelog_state.lock().unwrap().directory_input)
                        .desired_width(400.0)
                        .hint_text("Path to Forge game logs directory")
                );
                
                if response.changed() {
                    // Validate directory on change
                    let new_path = self.gamelog_state.lock().unwrap().directory_input.clone();
                    let valid = validate_directory(&new_path).unwrap_or(false);
                    let mut state = self.gamelog_state.lock().unwrap();
                    state.directory_valid = valid;
                    state.file_count = None;
                }
                
                if ui.button("Browse...").clicked() {
                    // Note: Native file dialogs require additional dependencies
                    // For now, show a message
                    let mut state = self.gamelog_state.lock().unwrap();
                    state.status_message = Some("Please enter the path manually or use the default".to_string());
                }
            });
            
            ui.horizontal(|ui| {
                if ui.button("Use Default").clicked() {
                    let default_dir = get_default_forge_log_directory();
                    let valid = validate_directory(&default_dir).unwrap_or(false);
                    let mut state = self.gamelog_state.lock().unwrap();
                    state.directory_input = default_dir;
                    state.directory_valid = valid;
                    state.file_count = None;
                }
                
                if ui.button("Save").clicked() {
                    self.save_gamelog_directory();
                }
                
                // Show validation status
                if directory_valid {
                    ui.label(egui::RichText::new("✓ Valid").color(egui::Color32::from_rgb(0, 128, 0)));
                    if let Some(count) = file_count {
                        ui.label(format!("({} log files)", count));
                    }
                } else if !directory_input.is_empty() {
                    ui.label(egui::RichText::new("✗ Invalid or inaccessible").color(egui::Color32::from_rgb(176, 0, 32)));
                }
            });
        });
        
        ui.add_space(10.0);
        
        // Scan controls section
        ui.group(|ui| {
            ui.label(egui::RichText::new("🔍 Scan Controls").strong());
            ui.add_space(5.0);
            
            ui.horizontal(|ui| {
                // Manual scan button
                if ui.add_enabled(!is_scanning && directory_valid, egui::Button::new("🔄 Scan Now")).clicked() {
                    self.start_gamelog_scan(ctx);
                }
                
                // Background scanning toggle
                let mut bg_enabled = background_enabled;
                if ui.checkbox(&mut bg_enabled, "Enable Background Scanning").changed() {
                    self.toggle_background_scanning(bg_enabled);
                }
                
                if is_scanning {
                    ui.spinner();
                    ui.label("Scanning...");
                }
            });
            
            if background_enabled {
                ui.label(egui::RichText::new("Background scanning is active. New logs will be uploaded automatically.").small().weak());
            }
        });
        
        ui.add_space(10.0);
        
        // Status message
        if let Some(msg) = status_message {
            let color = if msg.contains("Error") || msg.contains("failed") {
                egui::Color32::from_rgb(176, 0, 32)
            } else if msg.contains("Success") || msg.contains("uploaded") {
                egui::Color32::from_rgb(0, 128, 0)
            } else {
                egui::Color32::DARK_GRAY
            };
            ui.label(egui::RichText::new(&msg).color(color));
            ui.add_space(5.0);
        }
        
        // Results section
        let scan_results: Vec<GameLogProcessResult> = {
            let state = self.gamelog_state.lock().unwrap();
            state.scan_results.clone()
        };
        
        if !scan_results.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new("📋 Scan Results").strong());
            ui.add_space(5.0);
            
            // Summary
            let successful = scan_results.iter().filter(|r| r.success).count();
            let failed = scan_results.len() - successful;
            
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("✓ {} uploaded", successful)).color(egui::Color32::from_rgb(0, 128, 0)));
                if failed > 0 {
                    ui.label(egui::RichText::new(format!("✗ {} failed", failed)).color(egui::Color32::from_rgb(176, 0, 32)));
                }
            });
            
            ui.add_space(5.0);
            
            // Results list
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    for result in &scan_results {
                        ui.horizontal(|ui| {
                            let (icon, color) = if result.success {
                                ("✓", egui::Color32::from_rgb(0, 128, 0))
                            } else {
                                ("✗", egui::Color32::from_rgb(176, 0, 32))
                            };
                            ui.label(egui::RichText::new(icon).color(color));
                            ui.label(&result.filename);
                            if result.success {
                                ui.label(egui::RichText::new(format!("({} bytes)", result.file_size)).small().weak());
                            } else {
                                ui.label(egui::RichText::new(&result.message).small().color(egui::Color32::from_rgb(176, 0, 32)));
                            }
                        });
                    }
                });
        }
        
        // Processed files info
        let processed_count = {
            let state = self.gamelog_state.lock().unwrap();
            state.processed_files.len()
        };
        
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Total files processed: {}", processed_count)).small().weak());
            if ui.small_button("Clear History").clicked() {
                self.clear_processed_history();
            }
        });
    }

    fn save_gamelog_directory(&mut self) {
        let directory_input = {
            let state = self.gamelog_state.lock().unwrap();
            state.directory_input.clone()
        };
        
        // Validate and save
        let valid = validate_directory(&directory_input).unwrap_or(false);
        
        {
            let mut state = self.gamelog_state.lock().unwrap();
            state.directory_valid = valid;
            
            if valid {
                // Update file count
                let config = GameLogConfig {
                    watch_directory: directory_input.clone(),
                    ..Default::default()
                };
                state.file_count = scan_directory(&config).ok().map(|f| f.len());
            }
        }
        
        // Save to settings
        {
            let mut settings = self.settings.lock().unwrap();
            settings.gamelog_config.watch_directory = directory_input;
            let _ = settings.save();
        }
        
        let mut state = self.gamelog_state.lock().unwrap();
        if valid {
            state.status_message = Some("Directory saved successfully".to_string());
        } else {
            state.status_message = Some("Error: Directory is not valid or accessible".to_string());
        }
    }

    fn start_gamelog_scan(&mut self, ctx: &egui::Context) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let ctx_clone = ctx.clone();
        
        // Mark as scanning
        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_scanning = true;
            state.status_message = Some("Scanning for new game logs...".to_string());
            state.scan_results.clear();
        }
        
        tokio::spawn(async move {
            let config = {
                let settings = settings.lock().unwrap();
                settings.gamelog_config.clone()
            };
            
            let processed_files = {
                let state = gamelog_state.lock().unwrap();
                Arc::new(Mutex::new(state.processed_files.clone()))
            };
            
            let result = process_new_logs(&config, &processed_files).await;
            
            {
                let mut state = gamelog_state.lock().unwrap();
                state.is_scanning = false;
                
                match result {
                    Ok(summary) => {
                        // Clone results before moving
                        let results = summary.results.clone();
                        state.scan_results = results;
                        
                        // Update processed files
                        let new_processed = processed_files.lock().unwrap().clone();
                        state.processed_files = new_processed.clone();
                        
                        // Save processed files to disk
                        let _ = save_processed_files(&new_processed);
                        
                        if summary.new_files == 0 {
                            state.status_message = Some("No new files to process".to_string());
                        } else {
                            state.status_message = Some(format!(
                                "Scan complete: {} new files, {} uploaded, {} failed",
                                summary.new_files, summary.successfully_uploaded, summary.failed_uploads
                            ));
                        }
                        
                        state.last_scan_summary = Some(summary);
                    }
                    Err(e) => {
                        state.status_message = Some(format!("Error: {}", e));
                    }
                }
            }
            
            ctx_clone.request_repaint();
        });
    }

    fn toggle_background_scanning(&mut self, enabled: bool) {
        // Update state
        {
            let mut state = self.gamelog_state.lock().unwrap();
            state.background_enabled = enabled;
        }
        
        // Save to settings
        {
            let mut settings = self.settings.lock().unwrap();
            settings.gamelog_config.background_scan_enabled = enabled;
            let _ = settings.save();
        }
        
        // Note: Actual background scanning would require a separate thread/task
        // that periodically calls process_new_logs. For now, this just saves the preference.
        let mut state = self.gamelog_state.lock().unwrap();
        if enabled {
            state.status_message = Some("Background scanning enabled. Use 'Scan Now' to manually trigger.".to_string());
        } else {
            state.status_message = Some("Background scanning disabled.".to_string());
        }
    }

    fn clear_processed_history(&mut self) {
        {
            let mut state = self.gamelog_state.lock().unwrap();
            state.processed_files.clear();
            state.scan_results.clear();
            state.status_message = Some("Processed history cleared".to_string());
        }
        
        // Save empty set to disk
        let _ = save_processed_files(&HashSet::new());
    }
}
