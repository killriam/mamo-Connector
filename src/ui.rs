use anyhow::Result;
use eframe::{NativeOptions, egui};

use crate::commands::CommandResult;
use crate::deck::create_deck_from_id;
use crate::deeplink::Deeplink;
use crate::registration::{RegistrationOutcome, RegistrationStatus};

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
    is_loading: bool,
    manual_result: Option<String>,
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        Self {
            state,
            deck_url_input: String::new(),
            api_url_input: "http://localhost:8000".to_string(),
            is_loading: false,
            manual_result: None,
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
                
                ui.label("TEST: Registration Status");
                let status_text = match self.state.registration.status {
                RegistrationStatus::Registered => {
                    egui::RichText::new("Custom URI scheme registered")
                        .color(egui::Color32::from_rgb(0, 128, 0))
                }
                RegistrationStatus::Failed => {
                    egui::RichText::new("Failed to register custom URI scheme")
                        .color(egui::Color32::from_rgb(176, 0, 32))
                }
                RegistrationStatus::Skipped => {
                    egui::RichText::new("Scheme registration not supported on this platform")
                        .color(egui::Color32::from_rgb(196, 112, 0))
                }
            };

                ui.label(status_text);
                ui.small(&self.state.registration.message);

                ui.separator();
                ui.label("End of UI");
            });
    }
}

impl LauncherApp {
    fn handle_download_deck(&mut self, ctx: &egui::Context) {
        // Extract deck ID from URL
        let deck_id = if let Some(id) = self.extract_deck_id_from_url(&self.deck_url_input) {
            id
        } else {
            self.manual_result = Some("Error: Invalid deck URL format. Expected format like: https://moxfield.com/decks/DECK_ID".to_string());
            return;
        };
        
        let api_url = self.api_url_input.clone();
        let deck_id_clone = deck_id.clone();
        
        // Set loading state
        self.is_loading = true;
        self.manual_result = None;
        
        // Create async task
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            let result = create_deck_from_id(&deck_id_clone, &api_url).await;
            
            // Note: In a real application, you'd need to send the result back to the UI
            // This is a simplified version that just logs the result
            match result {
                Ok(deck_result) => {
                    log::info!("Deck creation result: {}", deck_result.message);
                }
                Err(e) => {
                    log::error!("Failed to create deck: {:?}", e);
                }
            }
            
            ctx_clone.request_repaint();
        });
        
        // For now, show a message that the download started
        self.manual_result = Some(format!("Started downloading deck with ID: {}...", deck_id));
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
