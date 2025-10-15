use anyhow::Result;
use eframe::{NativeOptions, egui};

use crate::deeplink::Deeplink;
use crate::registration::{RegistrationOutcome, RegistrationStatus};

#[derive(Clone)]
struct AppState {
    registration: RegistrationOutcome,
    args: Vec<String>,
    deeplink: Option<Deeplink>,
}

pub fn launch(
    registration: RegistrationOutcome,
    args: Vec<String>,
    deeplink: Option<Deeplink>,
) -> Result<()> {
    let state = AppState {
        registration,
        args,
        deeplink,
    };

    let native_options = NativeOptions::default();
    eframe::run_native(
        "Mamo Connector",
        native_options,
        Box::new(move |_cc| Box::new(LauncherApp::new(state.clone()))),
    )?;

    Ok(())
}

struct LauncherApp {
    state: AppState,
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Mamo Connector");

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

            ui.heading("Command-line arguments");
            if self.state.args.is_empty() {
                ui.label("No arguments were provided to the application.");
            } else {
                for arg in &self.state.args {
                    ui.monospace(arg);
                }
            }

            ui.separator();

            ui.heading("Deep link details");
            if let Some(deeplink) = &self.state.deeplink {
                ui.monospace(&deeplink.raw);
                if deeplink.action.is_empty() {
                    ui.label("No action detected in link");
                } else {
                    ui.label(format!("Action: {}", deeplink.action));
                }

                if deeplink.params.is_empty() {
                    ui.label("No parameters found.");
                } else {
                    ui.label("Parameters:");
                    for (key, value) in &deeplink.params {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("{key}"));
                            ui.label(":");
                            ui.monospace(value);
                        });
                    }
                }

                if let Some(token) = &deeplink.token {
                    ui.separator();
                    ui.label("Token");
                    ui.monospace(token);
                }

                if let Some(doc) = &deeplink.doc {
                    ui.separator();
                    ui.label("Document ID");
                    ui.monospace(doc);
                }
            } else {
                ui.label("No mamoConnector deep link detected in the arguments.");
            }
        });
    }
}
