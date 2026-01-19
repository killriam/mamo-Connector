use anyhow::Result;
use eframe::{NativeOptions, egui};
use std::sync::{Arc, Mutex};

use crate::commands::CommandResult;
use crate::deck::{create_deck_from_id, create_deck_from_moxfield, MoxfieldDeckEntry, list_moxfield_user_decks, import_selected_decks};
use crate::deeplink::Deeplink;
use crate::registration::{RegistrationOutcome, RegistrationStatus};

#[derive(Clone, PartialEq, Eq)]
enum Tab {
    Status,
    SingleDeck,
    UserDecks,
}

#[derive(Clone, Default)]
struct UserDecksState {
    username_input: String,
    decks: Vec<MoxfieldDeckEntry>,
    selected_decks: Vec<bool>,
    is_loading: bool,
    error_message: Option<String>,
    import_result: Option<String>,
}

#[derive(Clone, Default)]
struct SingleDeckState {
    is_loading: bool,
    result_message: Option<String>,
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
    deck_url_input: String,
    api_url_input: String,
    use_direct_mode: bool,  // Direct Moxfield access using curl (no backend)
    current_tab: Tab,
    user_decks_state: Arc<Mutex<UserDecksState>>,
    single_deck_state: Arc<Mutex<SingleDeckState>>,
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        Self {
            state,
            deck_url_input: String::new(),
            api_url_input: "http://localhost:3001".to_string(),
            use_direct_mode: true,  // Default to direct mode (no backend needed)
            current_tab: Tab::Status,
            user_decks_state: Arc::new(Mutex::new(UserDecksState::default())),
            single_deck_state: Arc::new(Mutex::new(SingleDeckState::default())),
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
                    if ui.selectable_label(self.current_tab == Tab::SingleDeck, "Single Deck").clicked() {
                        self.current_tab = Tab::SingleDeck;
                    }
                    if ui.selectable_label(self.current_tab == Tab::UserDecks, "User Decks").clicked() {
                        self.current_tab = Tab::UserDecks;
                    }
                });
                ui.separator();
                
                // Tab content
                match self.current_tab {
                    Tab::Status => self.render_status_tab(ui),
                    Tab::SingleDeck => self.render_single_deck_tab(ui, ctx),
                    Tab::UserDecks => self.render_user_decks_tab(ui, ctx),
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
    
    fn render_single_deck_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label("Import a single deck from Moxfield");
        ui.add_space(10.0);
        
        // Direct mode checkbox
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.use_direct_mode, "Direct mode (no backend needed)");
        });
        ui.small("Uses curl to fetch directly from Moxfield. Works without running the backend.");
        ui.add_space(10.0);
        
        // Only show API URL if not using direct mode
        if !self.use_direct_mode {
            ui.horizontal(|ui| {
                ui.label("API URL:");
                ui.text_edit_singleline(&mut self.api_url_input);
            });
            ui.add_space(5.0);
        }
        
        ui.horizontal(|ui| {
            ui.label("Deck URL:");
            ui.text_edit_singleline(&mut self.deck_url_input);
        });
        
        ui.add_space(5.0);
        ui.small("Example: https://moxfield.com/decks/abc123 or just the deck ID");
        ui.add_space(10.0);
        
        // Get current state
        let (is_loading, result_message) = {
            let state = self.single_deck_state.lock().unwrap();
            (state.is_loading, state.result_message.clone())
        };
        
        if ui.add_enabled(!is_loading, egui::Button::new("Download Deck")).clicked() {
            self.handle_download_deck(ctx);
        }
        
        if is_loading {
            ui.spinner();
            ui.label("Loading...");
        }
        
        if let Some(ref result) = result_message {
            ui.separator();
            ui.label(result);
        }
    }
    
    fn render_user_decks_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label("Import decks from a Moxfield user profile");
        ui.add_space(10.0);
        
        ui.horizontal(|ui| {
            ui.label("API URL:");
            ui.text_edit_singleline(&mut self.api_url_input);
        });
        ui.add_space(5.0);
        
        // Get a snapshot of the state values we need
        let (is_loading, username_input, has_decks, has_error, error_msg, decks_info) = {
            let state = self.user_decks_state.lock().unwrap();
            (
                state.is_loading,
                state.username_input.clone(),
                !state.decks.is_empty(),
                state.error_message.is_some(),
                state.error_message.clone(),
                state.decks.iter().enumerate().map(|(i, d)| {
                    (i, d.public_id.clone(), d.name.clone(), d.format.clone(), d.view_count, state.selected_decks.get(i).copied().unwrap_or(false))
                }).collect::<Vec<_>>(),
            )
        };
        
        // Username input and fetch button
        let mut new_username = username_input.clone();
        ui.horizontal(|ui| {
            ui.label("Username:");
            ui.text_edit_singleline(&mut new_username);
        });
        
        // Update username if changed
        if new_username != username_input {
            let mut state = self.user_decks_state.lock().unwrap();
            state.username_input = new_username.clone();
        }
        
        let can_fetch = !is_loading && !new_username.trim().is_empty();
        if ui.add_enabled(can_fetch, egui::Button::new("Fetch Decks")).clicked() {
            let username = new_username.trim().to_string();
            let api_url = self.api_url_input.clone();
            let state_clone = Arc::clone(&self.user_decks_state);
            let ctx_clone = ctx.clone();
            
            {
                let mut state = self.user_decks_state.lock().unwrap();
                state.is_loading = true;
                state.error_message = None;
                state.decks.clear();
                state.selected_decks.clear();
                state.import_result = None;
            }
            
            tokio::spawn(async move {
                let result = list_moxfield_user_decks(&username, &api_url).await;
                
                let mut state = state_clone.lock().unwrap();
                state.is_loading = false;
                
                match result {
                    Ok(decks) => {
                        state.selected_decks = vec![false; decks.len()];
                        state.decks = decks;
                        state.error_message = None;
                    }
                    Err(e) => {
                        state.error_message = Some(format!("Failed to fetch decks: {}", e));
                    }
                }
                
                ctx_clone.request_repaint();
            });
        }
        
        ui.add_space(5.0);
        ui.small("Enter a Moxfield username (e.g., IceMagma)");
        ui.add_space(10.0);
        
        if is_loading {
            ui.spinner();
            ui.label("Loading decks...");
            return;
        }
        
        if has_error {
            if let Some(error) = error_msg {
                ui.label(egui::RichText::new(error).color(egui::Color32::from_rgb(176, 0, 32)));
            }
            return;
        }
        
        if has_decks {
            ui.separator();
            
            // Selection controls
            ui.horizontal(|ui| {
                if ui.button("Select All").clicked() {
                    let mut state = self.user_decks_state.lock().unwrap();
                    for selected in &mut state.selected_decks {
                        *selected = true;
                    }
                }
                if ui.button("Select None").clicked() {
                    let mut state = self.user_decks_state.lock().unwrap();
                    for selected in &mut state.selected_decks {
                        *selected = false;
                    }
                }
                
                let selected_count = decks_info.iter().filter(|(_, _, _, _, _, s)| *s).count();
                ui.label(format!("{}/{} selected", selected_count, decks_info.len()));
            });
            
            ui.add_space(5.0);
            
            // Deck list with scrolling
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for (i, _deck_id, name, format, views, is_selected) in &decks_info {
                        let mut selected = *is_selected;
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut selected, "").changed() {
                                let mut state = self.user_decks_state.lock().unwrap();
                                if let Some(s) = state.selected_decks.get_mut(*i) {
                                    *s = selected;
                                }
                            }
                            ui.label(name);
                            let format_str = format.as_deref().unwrap_or("Unknown");
                            ui.label(egui::RichText::new(format_str).weak());
                            ui.label(egui::RichText::new(format!("{} views", views)).weak());
                        });
                    }
                });
            
            ui.add_space(10.0);
            
            // Import button
            let selected_count = decks_info.iter().filter(|(_, _, _, _, _, s)| *s).count();
            let selected_deck_ids: Vec<String> = decks_info.iter()
                .filter(|(_, _, _, _, _, s)| *s)
                .map(|(_, id, _, _, _, _)| id.clone())
                .collect();
            
            if ui.add_enabled(selected_count > 0, egui::Button::new(format!("Import {} Selected Decks", selected_count))).clicked() {
                let api_url = self.api_url_input.clone();
                let username = new_username.clone();
                let state_clone = Arc::clone(&self.user_decks_state);
                let ctx_clone = ctx.clone();
                
                {
                    let mut state = self.user_decks_state.lock().unwrap();
                    state.is_loading = true;
                    state.import_result = None;
                }
                
                tokio::spawn(async move {
                    let result = import_selected_decks(&selected_deck_ids, &api_url, &username).await;
                    
                    let mut state = state_clone.lock().unwrap();
                    state.is_loading = false;
                    
                    match result {
                        Ok(import_result) => {
                            let success_count = import_result.imported_decks.iter().filter(|d| d.success).count();
                            state.import_result = Some(format!(
                                "Successfully imported {} of {} decks",
                                success_count,
                                import_result.total_decks
                            ));
                        }
                        Err(e) => {
                            state.error_message = Some(format!("Import failed: {}", e));
                        }
                    }
                    
                    ctx_clone.request_repaint();
                });
            }
            
            // Show import result
            let import_result = {
                let state = self.user_decks_state.lock().unwrap();
                state.import_result.clone()
            };
            if let Some(result) = import_result {
                ui.label(egui::RichText::new(result).color(egui::Color32::from_rgb(0, 128, 0)));
            }
        }
    }
    
    fn handle_download_deck(&mut self, ctx: &egui::Context) {
        // Extract deck ID from URL
        let deck_id = if let Some(id) = self.extract_deck_id_from_url(&self.deck_url_input) {
            id
        } else {
            let mut state = self.single_deck_state.lock().unwrap();
            state.result_message = Some("Error: Invalid deck URL format. Expected format like: https://moxfield.com/decks/DECK_ID".to_string());
            return;
        };
        
        let use_direct = self.use_direct_mode;
        let api_url = self.api_url_input.clone();
        let deck_id_clone = deck_id.clone();
        let state_clone = Arc::clone(&self.single_deck_state);
        
        // Set loading state
        {
            let mut state = self.single_deck_state.lock().unwrap();
            state.is_loading = true;
            let mode_str = if use_direct { "directly from Moxfield" } else { "via backend" };
            state.result_message = Some(format!("Downloading deck {} {}...", deck_id, mode_str));
        }
        
        // Create async task
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            let result = if use_direct {
                // Direct mode: use curl to fetch from Moxfield
                create_deck_from_moxfield(&deck_id_clone).await
            } else {
                // Backend mode: use the API proxy
                create_deck_from_id(&deck_id_clone, &api_url).await
            };
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            
            match result {
                Ok(deck_result) => {
                    log::info!("Deck creation result: {}", deck_result.message);
                    state.result_message = Some(deck_result.message);
                }
                Err(e) => {
                    log::error!("Failed to create deck: {:?}", e);
                    state.result_message = Some(format!("Error: {}", e));
                }
            }
            
            ctx_clone.request_repaint();
        });
    }
    
    fn extract_deck_id_from_url(&self, url: &str) -> Option<String> {
        // Handle Moxfield URLs: https://moxfield.com/decks/DECK_ID
        if url.contains("moxfield.com/decks/") {
            return url.split("/decks/").nth(1).map(|s| {
                // Remove any trailing slashes or query parameters
                s.split(&['/', '?', '#'][..]).next().unwrap_or(s).to_string()
            });
        }
        
        // If it's just a deck ID (no URL), use it directly
        if !url.contains("://") && !url.is_empty() {
            return Some(url.trim().to_string());
        }
        
        None
    }
}
