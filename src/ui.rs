use anyhow::Result;
use eframe::{NativeOptions, egui};
use std::sync::{Arc, Mutex};

use crate::commands::CommandResult;
use crate::deck::{create_deck_from_moxfield, MoxfieldDeckEntry, DeckStatus, fetch_user_decks_direct, create_deck_from_archidekt, create_deck_from_deckstats, create_deck_from_mamo, parse_archidekt_url, parse_deckstats_url, parse_mamo_url};
use crate::deeplink::Deeplink;
use crate::registration::{RegistrationOutcome, RegistrationStatus};

#[derive(Clone, PartialEq, Eq)]
enum Tab {
    Status,
    Import,
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
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        Self {
            state,
            url_input: String::new(),
            current_tab: Tab::Import,
            import_state: Arc::new(Mutex::new(ImportState::default())),
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
                });
                ui.separator();
                
                // Tab content
                match self.current_tab {
                    Tab::Status => self.render_status_tab(ui),
                    Tab::Import => self.render_import_tab(ui, ctx),
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
}
