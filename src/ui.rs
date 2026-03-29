use anyhow::Result;
use chrono::Local;
use eframe::{NativeOptions, egui};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::commands::CommandResult;
use crate::deck::{create_deck_from_moxfield, MoxfieldDeckEntry, MamoDeckEntry, DeckStatus, fetch_user_decks_direct, create_deck_from_archidekt, create_deck_from_deckstats, create_deck_from_mamo, parse_archidekt_url, parse_deckstats_url, parse_mamo_url, parse_mamo_user_url, fetch_mamo_user_decks, sync_moxfield_deck, sync_moxfield_user_decks, sync_archidekt_deck, sync_deckstats_deck, sync_mamo_deck, DeckSyncResult, SyncStatus, get_deck_directory_display};
use rfd::FileDialog;
use crate::deeplink::Deeplink;
use crate::forge::{get_default_forge_path, resolve_latest_forge_jar, validate_forge_path, launch_forge_from_settings};
use crate::gamelog::{GameLogConfig, GameLogProcessResult, ScanSummary, get_default_forge_log_directory, validate_directory, scan_directory, load_processed_files, save_processed_files, DeckMappings, fetch_my_decks, suggest_deck_matches, load_cached_decks, save_cached_decks, process_new_logs_with_filter, GameLogFilterOptions, preview_scan, FilePreviewInfo};
use crate::registration::{RegistrationOutcome, RegistrationStatus};
use crate::settings::{Settings, SavedLink, SavedLinkType};
use crate::get_pending_command_path;

#[derive(Clone, PartialEq, Eq)]
enum Tab {
    Status,
    Activity,
    Import,
    Sync,
    GameLogs,
    Settings,
}

/// Detected URL type for auto-detection
#[derive(Clone, PartialEq, Eq, Debug)]
enum UrlType {
    MoxfieldDeck(String),         // Deck ID
    MoxfieldUser(String),         // Username
    ArchidektDeck(String),        // Deck ID
    DeckstatsDeck(String, String), // Owner ID, Deck ID
    MamoDeck(String),             // MaMo Deck UUID
    MamoUser(String),             // MaMo Username
    Unknown,
    Empty,
}

#[derive(Clone, Default)]
struct ImportState {
    is_loading: bool,
    result_message: Option<String>,
    // For Moxfield user decks
    decks: Vec<MoxfieldDeckEntry>,
    selected_decks: Vec<bool>,
    // For MaMo user decks
    mamo_decks: Vec<MamoDeckEntry>,
    selected_mamo_decks: Vec<bool>,
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
#[allow(dead_code)]
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
    /// User's decks from backend (for deck mapping)
    user_decks: Vec<crate::gamelog::UserDeck>,
    /// Is currently fetching decks
    is_fetching_decks: bool,
    /// Deck mappings (deck name from log -> MaMo deck ID)
    deck_mappings: crate::gamelog::DeckMappings,
    /// Show deck mapping dialog
    show_deck_mapping_dialog: bool,
    /// Currently selected deck name for mapping
    mapping_deck_name: Option<String>,
    /// Search filter for deck list
    deck_search_filter: String,
    /// Days filter - only upload logs from last N days (0 = no filter)
    days_filter: u32,
    /// Days filter input string for editing
    days_filter_input: String,
    /// Selected deck names to filter by (empty = all decks)
    selected_deck_filters: HashSet<String>,
    /// Show deck filter dropdown
    show_deck_filter_dropdown: bool,
    /// Preview scan results (files to be uploaded with detected decks)
    preview_results: Vec<FilePreviewInfo>,
    /// Is preview scan running
    is_previewing: bool,
}

/// State for the settings tab (includes Forge configuration)
#[derive(Clone, Default)]
struct SettingsState {
    /// Forge executable path input
    forge_path_input: String,
    /// Is the Forge path valid
    forge_path_valid: bool,
    /// Auto-launch Forge after deck download
    forge_auto_launch: bool,
    /// MaMo API authentication token
    auth_token_input: String,
    /// Status message
    status_message: Option<String>,
}

/// A single log entry for the activity log
#[derive(Clone)]
struct ActivityLogEntry {
    timestamp: String,
    message: String,
    is_error: bool,
    is_success: bool,
}

impl ActivityLogEntry {
    fn info(message: impl Into<String>) -> Self {
        Self {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            message: message.into(),
            is_error: false,
            is_success: false,
        }
    }
    
    fn success(message: impl Into<String>) -> Self {
        Self {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            message: message.into(),
            is_error: false,
            is_success: true,
        }
    }
    
    fn error(message: impl Into<String>) -> Self {
        Self {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            message: message.into(),
            is_error: true,
            is_success: false,
        }
    }
}

/// State for the activity log panel
#[derive(Clone, Default)]
struct ActivityLogState {
    /// Log entries (newest first)
    entries: Vec<ActivityLogEntry>,
    /// Maximum number of entries to keep
    max_entries: usize,
}

impl ActivityLogState {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 100,
        }
    }
    
    fn log(&mut self, entry: ActivityLogEntry) {
        self.entries.insert(0, entry);
        if self.entries.len() > self.max_entries {
            self.entries.truncate(self.max_entries);
        }
    }
    
    fn log_info(&mut self, message: impl Into<String>) {
        self.log(ActivityLogEntry::info(message));
    }
    
    fn log_success(&mut self, message: impl Into<String>) {
        self.log(ActivityLogEntry::success(message));
    }
    
    fn log_error(&mut self, message: impl Into<String>) {
        self.log(ActivityLogEntry::error(message));
    }
    
    fn clear(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone)]
#[allow(dead_code)]
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
    settings_state: Arc<Mutex<SettingsState>>,
    activity_log: Arc<Mutex<ActivityLogState>>,
    settings: Arc<Mutex<Settings>>,
    last_pending_check: Instant,
    /// Whether we have a pending initial deeplink to process (set once, consumed on first update)
    pending_initial_deeplink: Option<Deeplink>,
    /// PID of Forge launcher process (may exit quickly if forge.exe is a wrapper)
    forge_pid: Arc<Mutex<Option<u32>>>,
    /// When Forge monitoring started (for startup grace period)
    forge_monitoring_since: Arc<Mutex<Option<Instant>>>,
    /// Whether the Forge window has been observed open at least once during this monitoring session.
    /// Used to distinguish "window not yet open" from "window was open and now closed".
    forge_window_seen: bool,
    /// Timestamp of last automatic gamelog scan
    last_auto_gamelog_scan: Option<Instant>,
}

impl LauncherApp {
    fn new(state: AppState) -> Self {
        // Load settings
        let mut settings = Settings::load().unwrap_or_default();
        
        // Sync auth_token to gamelog_config if needed
        if settings.auth_token.is_some() && settings.gamelog_config.auth_token.is_none() {
            settings.gamelog_config.auth_token = settings.auth_token.clone();
        }
        
        // Load processed files for game log
        let processed_files = load_processed_files().unwrap_or_default();
        
        // Load deck mappings
        let deck_mappings = DeckMappings::load().unwrap_or_default();
        
        // Load cached user decks
        let cached_decks = load_cached_decks()
            .map(|c| c.decks)
            .unwrap_or_default();
        
        // Initialize gamelog state with settings
        let gamelog_state = GameLogState {
            directory_input: settings.gamelog_config.watch_directory.clone(),
            directory_valid: validate_directory(&settings.gamelog_config.watch_directory).unwrap_or(false),
            background_enabled: settings.gamelog_config.background_scan_enabled,
            processed_files,
            deck_mappings,
            user_decks: cached_decks,
            ..Default::default()
        };
        
        // Initialize settings state with Forge config and auth token
        let settings_state = SettingsState {
            forge_path_input: settings.forge_path.clone().unwrap_or_default(),
            forge_path_valid: settings.forge_path.as_ref().map(|p| validate_forge_path(p)).unwrap_or(false),
            forge_auto_launch: settings.forge_auto_launch,
            auth_token_input: settings.auth_token.clone().unwrap_or_default(),
            status_message: None,
        };
        
        // Initialize activity log with startup entry
        let mut activity_log = ActivityLogState::new();
        activity_log.log_info("MaMo Connector started");
        
        // Store deeplink for deferred processing with progress logging
        let started_with_deeplink = state.deeplink.is_some();
        let pending_initial_deeplink = state.deeplink.clone();
        
        // Log the command result if already present (pre-processed, e.g. auth)
        if let Some(ref result) = state.command_result {
            match result {
                CommandResult::DeckCreated(deck_result) => {
                    if deck_result.success {
                        activity_log.log_success(&deck_result.message);
                    } else {
                        activity_log.log_error(&deck_result.message);
                    }
                }
                CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                    if deck_result.success {
                        activity_log.log_success(&deck_result.message);
                    } else {
                        activity_log.log_error(&deck_result.message);
                    }
                    if forge_result.already_running {
                        activity_log.log_info(&forge_result.message);
                    } else if forge_result.success {
                        activity_log.log_success(&forge_result.message);
                    } else {
                        activity_log.log_error(&forge_result.message);
                    }
                }
                CommandResult::ForgeLaunched(forge_result) => {
                    if forge_result.already_running {
                        activity_log.log_info(&forge_result.message);
                    } else if forge_result.success {
                        activity_log.log_success(&forge_result.message);
                    } else {
                        activity_log.log_error(&forge_result.message);
                    }
                }
                CommandResult::AuthTokenSaved(msg) => {
                    activity_log.log_success(msg);
                }
                CommandResult::Error(err) => {
                    activity_log.log_error(err);
                }
                CommandResult::UnknownAction(action) => {
                    activity_log.log_error(format!("Unknown action: {}", action));
                }
                CommandResult::MissingParameters(msg) => {
                    activity_log.log_error(format!("Missing parameters: {}", msg));
                }
                CommandResult::UserDecksImported(result) => {
                    activity_log.log_info(&result.message);
                }
                CommandResult::UserDecksList(decks) => {
                    activity_log.log_info(format!("Found {} decks", decks.len()));
                }
            }
        }
        
        // Switch to Activity tab if started with a deeplink
        let initial_tab = if started_with_deeplink { Tab::Activity } else { Tab::Import };
        
        Self {
            state,
            url_input: String::new(),
            current_tab: initial_tab,
            import_state: Arc::new(Mutex::new(ImportState::default())),
            sync_state: Arc::new(Mutex::new(SyncState::default())),
            gamelog_state: Arc::new(Mutex::new(gamelog_state)),
            settings_state: Arc::new(Mutex::new(settings_state)),
            activity_log: Arc::new(Mutex::new(activity_log)),
            settings: Arc::new(Mutex::new(settings)),
            last_pending_check: Instant::now(),
            pending_initial_deeplink,
            forge_pid: Arc::new(Mutex::new(None)),
            forge_monitoring_since: Arc::new(Mutex::new(None)),
            forge_window_seen: false,
            last_auto_gamelog_scan: None,
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process initial deeplink on first frame (with progress logging)
        if let Some(deeplink) = self.pending_initial_deeplink.take() {
            self.process_deeplink_with_progress(deeplink, ctx);
        }
        
        // Check for pending commands from secondary instances every 500ms
        let now = Instant::now();
        if now.duration_since(self.last_pending_check).as_millis() > 500 {
            self.last_pending_check = now;
            self.check_pending_commands(ctx);
        }
        
        // Auto gamelog scanning after deeplink Forge launch
        // Two-phase detection: first track launcher PID, then switch to window-based
        // detection since forge.exe is a launcher that spawns java.exe and exits.
        let monitoring_since = *self.forge_monitoring_since.lock().unwrap();
        if let Some(start_time) = monitoring_since {
            let forge_pid_value = *self.forge_pid.lock().unwrap();
            let pid_alive = forge_pid_value.map(|p| crate::forge::is_process_running(p)).unwrap_or(false);
            let window_open = crate::forge::is_forge_window_open();
            let forge_alive = pid_alive || window_open;
            let is_scanning = self.gamelog_state.lock().unwrap().is_scanning;
            let elapsed = now.duration_since(start_time);
            
            // Clear launcher PID once it exits (launcher is just a wrapper)
            if !pid_alive && forge_pid_value.is_some() {
                *self.forge_pid.lock().unwrap() = None;
            }
            
            // Track if we've ever seen the Forge window open.
            // Only count it after the launcher PID has exited — while the launcher is
            // still alive, any "Forge" window belongs to the launcher itself (not the
            // real Java game), so counting it would incorrectly reduce close_threshold
            // from 120 s to 20 s before Java has had a chance to start.
            if window_open && !pid_alive {
                self.forge_window_seen = true;
            }
            
            if !forge_alive {
                // Determine whether to declare Forge truly closed:
                // - If the window was never observed: give up to 120 s for Java to start
                //   (launcher exits almost immediately, Java window can be slow to appear)
                // - If the window was observed before: only need the normal 20 s grace period
                //   so we don't delay the final scan unnecessarily
                let close_threshold = if self.forge_window_seen { 20 } else { 120 };
                if elapsed.as_secs() < close_threshold {
                    // Still within grace period - Java/Forge window may not have appeared yet
                } else {
                    // Forge is truly closed (no PID, no window, past grace period)
                    if !is_scanning {
                        if let Ok(mut log) = self.activity_log.lock() {
                            log.log_info("\u{1F3AE} Forge closed - triggering final gamelog scan");
                        }
                        self.start_auto_gamelog_scan(ctx);
                    }
                    *self.forge_monitoring_since.lock().unwrap() = None;
                    self.forge_window_seen = false;
                    self.last_auto_gamelog_scan = None;
                }
            } else if !is_scanning {
                // Forge is running - handle periodic scans
                let should_scan = match self.last_auto_gamelog_scan {
                    None => {
                        self.last_auto_gamelog_scan = Some(now);
                        if let Ok(mut log) = self.activity_log.lock() {
                            log.log_info("\u{1F3AE} Forge running - auto gamelog scanning active (every 5 min)");
                        }
                        false
                    }
                    Some(last) => now.duration_since(last).as_secs() >= 300,
                };
                
                if should_scan {
                    if let Ok(mut log) = self.activity_log.lock() {
                        log.log_info("\u{1F504} Auto gamelog scan (periodic 5 min)");
                    }
                    self.start_auto_gamelog_scan(ctx);
                    self.last_auto_gamelog_scan = Some(now);
                }
            }
        }
        
        // Request a repaint in 500ms to keep checking for pending commands
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::WHITE))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
                ui.visuals_mut().panel_fill = egui::Color32::WHITE;
                
                // Title with version info
                ui.horizontal(|ui| {
                    ui.heading("Mamo Connector");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small(egui::RichText::new(format!("v{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")))
                            .color(egui::Color32::GRAY));
                    });
                });
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
                    if ui.selectable_label(self.current_tab == Tab::Activity, "📋 Activity").clicked() {
                        self.current_tab = Tab::Activity;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Settings, "⚙ Settings").clicked() {
                        self.current_tab = Tab::Settings;
                    }
                });
                ui.separator();
                
                // Tab content
                match self.current_tab {
                    Tab::Status => self.render_status_tab(ui),
                    Tab::Activity => self.render_activity_tab(ui),
                    Tab::Import => self.render_import_tab(ui, ctx),
                    Tab::Sync => self.render_sync_tab(ui, ctx),
                    Tab::GameLogs => self.render_gamelog_tab(ui, ctx),
                    Tab::Settings => self.render_settings_tab(ui, ctx),
                }
            });
    }
}

impl LauncherApp {
    /// Process a deeplink with real-time progress logging to the Activity tab
    fn process_deeplink_with_progress(&mut self, deeplink: Deeplink, ctx: &egui::Context) {
        use crate::commands::{self, SharedLogCollector};
        use log::info;
        
        info!("Processing deeplink with progress: {}", deeplink.raw);
        
        // Switch to Activity tab to show progress
        self.current_tab = Tab::Activity;
        
        // Log the incoming command
        if let Ok(mut log) = self.activity_log.lock() {
            log.log_info(format!("Received command: {}", deeplink.raw));
            log.log_info(format!("Processing action: {}", deeplink.action));
            if let Some(ref deck_id) = deeplink.deck_id {
                log.log_info(format!("Deck ID: {}", deck_id));
            }
            log.log_info("Starting command execution...");
        }
        
        // Request immediate repaint to show the initial logs
        ctx.request_repaint();
        
        // Handle the command in a background thread
        let settings = self.settings.clone();
        let settings_state = self.settings_state.clone();
        let activity_log = self.activity_log.clone();
        let activity_log_for_polling = self.activity_log.clone();
        let forge_pid = self.forge_pid.clone();
        let forge_monitoring_since = self.forge_monitoring_since.clone();
        let ctx_clone = ctx.clone();
        let ctx_for_polling = ctx.clone();
        
        // Create a log collector for the command handler
        let log_collector: SharedLogCollector = Arc::new(Mutex::new(Vec::new()));
        let log_collector_for_command = log_collector.clone();
        
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            
            let result = runtime.block_on(async {
                // Spawn a polling task to transfer logs to activity_log in real-time
                let collector_for_polling = log_collector.clone();
                let poll_handle = tokio::spawn(async move {
                    let mut last_len = 0;
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        if let Ok(logs) = collector_for_polling.lock() {
                            let current_len = logs.len();
                            if current_len > last_len {
                                if let Ok(mut activity) = activity_log_for_polling.lock() {
                                    for i in last_len..current_len {
                                        activity.log_info(&logs[i]);
                                    }
                                }
                                ctx_for_polling.request_repaint();
                                last_len = current_len;
                            }
                        }
                    }
                });
                
                let result = commands::handle_command_with_logger(&deeplink, Some(log_collector_for_command)).await;
                
                // Stop the polling task
                poll_handle.abort();
                
                result
            });
            
            // Log the final result
            if let Ok(mut log) = activity_log.lock() {
                match &result {
                    commands::CommandResult::DeckCreated(deck_result) => {
                        if deck_result.success {
                            log.log_success(&deck_result.message);
                        } else {
                            log.log_error(&deck_result.message);
                        }
                    }
                    commands::CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                        if deck_result.success {
                            log.log_success(&deck_result.message);
                        } else {
                            log.log_error(&deck_result.message);
                        }
                        if forge_result.already_running {
                            log.log_info(&forge_result.message);
                        } else if forge_result.success {
                            log.log_success(&forge_result.message);
                        } else {
                            log.log_error(&forge_result.message);
                        }
                    }
                    commands::CommandResult::ForgeLaunched(forge_result) => {
                        if forge_result.already_running {
                            log.log_info(&forge_result.message);
                        } else if forge_result.success {
                            log.log_success(&forge_result.message);
                        } else {
                            log.log_error(&forge_result.message);
                        }
                    }
                    commands::CommandResult::AuthTokenSaved(msg) => {
                        log.log_success(msg);
                    }
                    commands::CommandResult::Error(err) => {
                        log.log_error(err);
                    }
                    commands::CommandResult::UnknownAction(action) => {
                        log.log_error(format!("Unknown action: {}", action));
                    }
                    commands::CommandResult::MissingParameters(msg) => {
                        log.log_error(format!("Missing parameters: {}", msg));
                    }
                    commands::CommandResult::UserDecksImported(result) => {
                        log.log_info(&result.message);
                    }
                    commands::CommandResult::UserDecksList(decks) => {
                        log.log_info(format!("Found {} decks", decks.len()));
                    }
                }
            }
            
            // Track Forge PID for auto gamelog scanning
            match &result {
                commands::CommandResult::DeckCreatedAndLaunched(_, forge_result) if forge_result.success => {
                    if let Some(pid) = forge_result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                }
                commands::CommandResult::ForgeLaunched(forge_result) if forge_result.success => {
                    if let Some(pid) = forge_result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                }
                _ => {}
            }
            
            // Handle auth token saved result
            if let commands::CommandResult::AuthTokenSaved(ref token) = result {
                info!("Auth token saved via initial deeplink: {}", 
                    if token.len() > 20 { format!("{}...", &token[..20]) } else { token.clone() });
                
                // Reload settings from disk to get the updated auth_token
                if let Ok(reloaded_settings) = crate::settings::Settings::load() {
                    let auth_token = reloaded_settings.auth_token.clone();
                    
                    if let Ok(mut settings_guard) = settings.lock() {
                        *settings_guard = reloaded_settings;
                    }
                    
                    if let Some(token) = auth_token {
                        if let Ok(mut state_guard) = settings_state.lock() {
                            state_guard.auth_token_input = token;
                            state_guard.status_message = Some("✓ Connected to MaMo".to_string());
                        }
                    }
                }
            }
            
            ctx_clone.request_repaint();
        });
    }
    
    /// Check for pending commands from secondary instances
    fn check_pending_commands(&mut self, ctx: &egui::Context) {
        use crate::commands::{self, SharedLogCollector};
        use crate::deeplink;
        use log::info;
        
        let pending_path = get_pending_command_path();
        if pending_path.exists() {
            if let Ok(raw_command) = std::fs::read_to_string(&pending_path) {
                let raw_command = raw_command.trim();
                if !raw_command.is_empty() {
                    info!("Processing pending command: {}", raw_command);
                    
                    // Switch to Activity tab to show progress
                    self.current_tab = Tab::Activity;
                    
                    // Log the incoming command
                    if let Ok(mut log) = self.activity_log.lock() {
                        log.log_info(format!("Received command: {}", raw_command));
                    }
                    
                    // Parse the deeplink
                    if let Some(deeplink) = deeplink::parse_deeplink(&[raw_command.to_string()], "mamoConnector://") {
                        // Log what we're doing
                        if let Ok(mut log) = self.activity_log.lock() {
                            log.log_info(format!("Processing action: {}", deeplink.action));
                            if let Some(ref deck_id) = deeplink.deck_id {
                                log.log_info(format!("Deck ID: {}", deck_id));
                            }
                            log.log_info("Starting command execution...");
                        }
                        
                        // Create a log collector for real-time progress updates
                        let log_collector: SharedLogCollector = Arc::new(Mutex::new(Vec::new()));
                        
                        // Handle the command in a background thread
                        let settings = self.settings.clone();
                        let settings_state = self.settings_state.clone();
                        let activity_log = self.activity_log.clone();
                        let activity_log_for_polling = self.activity_log.clone();
                        let forge_pid = self.forge_pid.clone();
                        let forge_monitoring_since = self.forge_monitoring_since.clone();
                        let log_collector_for_command = log_collector.clone();
                        let ctx_clone = ctx.clone();
                        let ctx_for_polling = ctx.clone();
                        
                        std::thread::spawn(move || {
                            let runtime = tokio::runtime::Runtime::new().unwrap();
                            
                            let result = runtime.block_on(async {
                                // Spawn a polling task to transfer logs to activity_log in real-time
                                let collector_for_polling = log_collector.clone();
                                let poll_handle = tokio::spawn(async move {
                                    let mut last_len = 0;
                                    loop {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                        if let Ok(logs) = collector_for_polling.lock() {
                                            let current_len = logs.len();
                                            if current_len > last_len {
                                                if let Ok(mut activity) = activity_log_for_polling.lock() {
                                                    for i in last_len..current_len {
                                                        activity.log_info(&logs[i]);
                                                    }
                                                }
                                                ctx_for_polling.request_repaint();
                                                last_len = current_len;
                                            }
                                        }
                                    }
                                });
                                
                                let result = commands::handle_command_with_logger(&deeplink, Some(log_collector_for_command)).await;
                                
                                // Stop the polling task
                                poll_handle.abort();
                                
                                result
                            });
                            
                            // Log the final result
                            if let Ok(mut log) = activity_log.lock() {
                                match &result {
                                    commands::CommandResult::DeckCreated(deck_result) => {
                                        if deck_result.success {
                                            log.log_success(&deck_result.message);
                                        } else {
                                            log.log_error(&deck_result.message);
                                        }
                                    }
                                    commands::CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                                        if deck_result.success {
                                            log.log_success(&deck_result.message);
                                        } else {
                                            log.log_error(&deck_result.message);
                                        }
                                        if forge_result.already_running {
                                            log.log_info(&forge_result.message);
                                        } else if forge_result.success {
                                            log.log_success(&forge_result.message);
                                        } else {
                                            log.log_error(&forge_result.message);
                                        }
                                    }
                                    commands::CommandResult::ForgeLaunched(forge_result) => {
                                        if forge_result.already_running {
                                            log.log_info(&forge_result.message);
                                        } else if forge_result.success {
                                            log.log_success(&forge_result.message);
                                        } else {
                                            log.log_error(&forge_result.message);
                                        }
                                    }
                                    commands::CommandResult::AuthTokenSaved(msg) => {
                                        log.log_success(msg);
                                    }
                                    commands::CommandResult::Error(err) => {
                                        log.log_error(err);
                                    }
                                    commands::CommandResult::UnknownAction(action) => {
                                        log.log_error(format!("Unknown action: {}", action));
                                    }
                                    commands::CommandResult::MissingParameters(msg) => {
                                        log.log_error(format!("Missing parameters: {}", msg));
                                    }
                                    commands::CommandResult::UserDecksImported(result) => {
                                        log.log_info(&result.message);
                                    }
                                    commands::CommandResult::UserDecksList(decks) => {
                                        log.log_info(format!("Found {} decks", decks.len()));
                                    }
                                }
                            }
                            
                            // Track Forge PID for auto gamelog scanning
                            match &result {
                                commands::CommandResult::DeckCreatedAndLaunched(_, forge_result) if forge_result.success => {
                                    if let Some(pid) = forge_result.pid {
                                        *forge_pid.lock().unwrap() = Some(pid);
                                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                                    }
                                }
                                commands::CommandResult::ForgeLaunched(forge_result) if forge_result.success => {
                                    if let Some(pid) = forge_result.pid {
                                        *forge_pid.lock().unwrap() = Some(pid);
                                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                                    }
                                }
                                _ => {}
                            }
                            
                            // Handle auth token saved result
                            if let commands::CommandResult::AuthTokenSaved(ref token) = result {
                                info!("Auth token saved via pending command: {}", 
                                    if token.len() > 20 { format!("{}...", &token[..20]) } else { token.clone() });
                                
                                // Reload settings from disk to get the updated auth_token
                                if let Ok(reloaded_settings) = crate::settings::Settings::load() {
                                    // Get the auth token before updating settings
                                    let auth_token = reloaded_settings.auth_token.clone();
                                    
                                    // Update the settings
                                    if let Ok(mut settings_guard) = settings.lock() {
                                        *settings_guard = reloaded_settings;
                                    }
                                    
                                    // Update the settings state UI fields with the token we captured
                                    if let Some(token) = auth_token {
                                        if let Ok(mut state_guard) = settings_state.lock() {
                                            state_guard.auth_token_input = token;
                                            state_guard.status_message = Some("✓ Connected to MaMo".to_string());
                                        }
                                    }
                                }
                            }
                            
                            ctx_clone.request_repaint();
                        });
                    }
                    
                    // Delete the pending command file
                    let _ = std::fs::remove_file(&pending_path);
                }
            }
        }
    }
    
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
                CommandResult::DeckCreatedAndLaunched(deck_result, forge_result) => {
                    ui.label(egui::RichText::new(&deck_result.message)
                        .color(egui::Color32::from_rgb(0, 128, 0)));
                    ui.label(egui::RichText::new(&forge_result.message)
                        .color(if forge_result.already_running {
                            egui::Color32::from_rgb(180, 120, 0)
                        } else if forge_result.success {
                            egui::Color32::from_rgb(0, 128, 0)
                        } else {
                            egui::Color32::from_rgb(176, 0, 32)
                        }));
                }
                CommandResult::ForgeLaunched(forge_result) => {
                    ui.label(egui::RichText::new(&forge_result.message)
                        .color(if forge_result.already_running {
                            egui::Color32::from_rgb(180, 120, 0)
                        } else if forge_result.success {
                            egui::Color32::from_rgb(0, 128, 0)
                        } else {
                            egui::Color32::from_rgb(176, 0, 32)
                        }));
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
                CommandResult::AuthTokenSaved(msg) => {
                    ui.label(egui::RichText::new(msg)
                        .color(egui::Color32::from_rgb(0, 128, 0)));
                    // Reload settings from disk to get the new token
                    if let Ok(new_settings) = Settings::load() {
                        if let Some(ref token) = new_settings.auth_token {
                            // Update the in-memory settings
                            {
                                let mut settings = self.settings.lock().unwrap();
                                settings.auth_token = Some(token.clone());
                                settings.gamelog_config.auth_token = Some(token.clone());
                            }
                            // Update the settings state UI
                            {
                                let mut state = self.settings_state.lock().unwrap();
                                state.auth_token_input = token.clone();
                                state.status_message = Some("Connected via deeplink!".to_string());
                            }
                        }
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
        
        // Build info section at the bottom
        ui.separator();
        ui.label(egui::RichText::new("Build Information").strong());
        ui.horizontal(|ui| {
            ui.label("Version:");
            ui.label(egui::RichText::new(env!("CARGO_PKG_VERSION")).monospace());
        });
        ui.horizontal(|ui| {
            ui.label("Git Commit:");
            ui.label(egui::RichText::new(env!("GIT_HASH")).monospace());
        });
        ui.horizontal(|ui| {
            ui.label("Branch:");
            ui.label(egui::RichText::new(env!("GIT_BRANCH")).monospace());
        });
        ui.horizontal(|ui| {
            ui.label("Built:");
            ui.label(egui::RichText::new(env!("BUILD_TIME")).monospace());
        });
    }
    
    fn render_activity_tab(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Activity Log");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    if let Ok(mut log) = self.activity_log.lock() {
                        log.clear();
                    }
                }
            });
        });
        ui.separator();
        ui.small("Shows progress when processing deeplink commands (e.g., playtest links)");
        ui.add_space(8.0);
        
        // Scrollable log area
        let available_height = ui.available_height().max(200.0);
        egui::ScrollArea::vertical()
            .max_height(available_height)
            .auto_shrink([false, false])
            .stick_to_bottom(false)
            .show(ui, |ui| {
                if let Ok(log) = self.activity_log.lock() {
                    if log.entries.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(egui::RichText::new("No activity yet").italics().color(egui::Color32::GRAY));
                            ui.add_space(8.0);
                            ui.small("Activity will appear here when you use playtest links");
                        });
                    } else {
                        for entry in &log.entries {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&entry.timestamp)
                                    .monospace()
                                    .color(egui::Color32::GRAY));
                                
                                let color = if entry.is_error {
                                    egui::Color32::from_rgb(176, 0, 32)
                                } else if entry.is_success {
                                    egui::Color32::from_rgb(0, 128, 0)
                                } else {
                                    egui::Color32::BLACK
                                };
                                
                                let prefix = if entry.is_error {
                                    "❌ "
                                } else if entry.is_success {
                                    "✅ "
                                } else {
                                    "ℹ️ "
                                };
                                
                                ui.label(egui::RichText::new(format!("{}{}", prefix, &entry.message))
                                    .color(color));
                            });
                        }
                    }
                }
            });
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
        
        // MaMo user: https://ma-mo-frontend.vercel.app/user/USERNAME
        if let Some(username) = parse_mamo_user_url(url) {
            return UrlType::MamoUser(username);
        }
        
        // MaMo deck: https://ma-mo-frontend.vercel.app/deckId=UUID or similar
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
            UrlType::MamoUser(username) => {
                ui.label(egui::RichText::new(format!("✓ MaMo User: {} → will list all decks", username)).color(egui::Color32::from_rgb(0, 128, 0)));
            }
            UrlType::Unknown => {
                ui.label(egui::RichText::new("⚠ Unknown URL format").color(egui::Color32::from_rgb(200, 100, 0)));
            }
            UrlType::Empty => {}
        }
        
        ui.add_space(10.0);
        
        // Get current state for Moxfield decks
        let (is_loading, result_message, has_moxfield_decks, decks_info) = {
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
        
        // Get MaMo decks state
        let (has_mamo_decks, mamo_decks_info) = {
            let state = self.import_state.lock().unwrap();
            (
                !state.mamo_decks.is_empty(),
                state.mamo_decks.iter().enumerate().map(|(i, d)| {
                    (i, d.deck_id.clone(), d.deck_name.clone(), d.format.clone(),
                     state.selected_mamo_decks.get(i).copied().unwrap_or(false),
                     d.local_status.clone(), d.commander_name.clone())
                }).collect::<Vec<_>>(),
            )
        };
        
        let _has_decks = has_moxfield_decks || has_mamo_decks;
        
        // Main action button based on URL type
        match &url_type {
            UrlType::MoxfieldUser(username) => {
                if !has_moxfield_decks {
                    // Show "Fetch Decks" button
                    if ui.add_enabled(!is_loading, egui::Button::new("Fetch User Decks")).clicked() {
                        self.fetch_user_decks(username.clone(), ctx);
                    }
                }
            }
            UrlType::MamoUser(username) => {
                if !has_mamo_decks {
                    // Show "Fetch MaMo Decks" button
                    if ui.add_enabled(!is_loading, egui::Button::new("Fetch MaMo User Decks")).clicked() {
                        self.fetch_mamo_user_decks(username.clone(), ctx);
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
        
        // Show MaMo user decks list if available
        if has_mamo_decks {
            ui.separator();
            ui.add_space(5.0);
            ui.label(egui::RichText::new("MaMo User Decks").strong());
            
            // Selection controls
            ui.horizontal(|ui| {
                if ui.button("Select All").clicked() {
                    let mut state = self.import_state.lock().unwrap();
                    for selected in &mut state.selected_mamo_decks {
                        *selected = true;
                    }
                }
                if ui.button("Select None").clicked() {
                    let mut state = self.import_state.lock().unwrap();
                    for selected in &mut state.selected_mamo_decks {
                        *selected = false;
                    }
                }
                
                let selected_count = mamo_decks_info.iter().filter(|(_, _, _, _, s, _, _)| *s).count();
                ui.label(format!("{}/{} selected", selected_count, mamo_decks_info.len()));
            });
            
            ui.add_space(5.0);
            
            // MaMo Deck list with scrolling
            let available_height = (ui.available_height() - 60.0) / 2.0;
            egui::ScrollArea::vertical()
                .id_source("mamo_decks_scroll")
                .max_height(available_height.max(100.0))
                .show(ui, |ui: &mut egui::Ui| {
                    for (i, _deck_id, name, format, is_selected, local_status, commander) in &mamo_decks_info {
                        let mut selected = *is_selected;
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut selected, "").changed() {
                                let mut state = self.import_state.lock().unwrap();
                                if let Some(s) = state.selected_mamo_decks.get_mut(*i) {
                                    *s = selected;
                                }
                            }
                            
                            // Status indicator
                            let status_text = match local_status {
                                Some(DeckStatus::New) => egui::RichText::new("●").color(egui::Color32::from_rgb(0, 150, 0)),
                                Some(DeckStatus::NeedsUpdate) => egui::RichText::new("●").color(egui::Color32::from_rgb(255, 165, 0)),
                                Some(DeckStatus::UpToDate) => egui::RichText::new("●").color(egui::Color32::from_rgb(100, 100, 100)),
                                None => egui::RichText::new("●").color(egui::Color32::from_rgb(0, 150, 0)),
                            };
                            ui.label(status_text);
                            
                            ui.label(name);
                            if let Some(fmt) = format {
                                ui.label(egui::RichText::new(format!("[{}]", fmt)).weak());
                            }
                            if let Some(cmdr) = commander {
                                ui.label(egui::RichText::new(format!("({})", cmdr)).weak());
                            }
                        });
                    }
                });
            
            // Import button for MaMo decks
            let selected_count = mamo_decks_info.iter().filter(|(_, _, _, _, s, _, _)| *s).count();
            if selected_count > 0 {
                if ui.add_enabled(!is_loading, egui::Button::new(format!("Import {} MaMo Deck(s)", selected_count))).clicked() {
                    self.import_selected_mamo_decks(ctx);
                }
            }
        }
        
        // Show Moxfield user decks list if available
        if has_moxfield_decks {
            ui.separator();
            ui.add_space(5.0);
            ui.label(egui::RichText::new("Moxfield User Decks").strong());
            
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
                .id_source("moxfield_decks_scroll")
                .max_height(available_height.max(100.0))
                .show(ui, |ui: &mut egui::Ui| {
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
    
    /// Fetch decks for a MaMo user
    fn fetch_mamo_user_decks(&mut self, username: String, ctx: &egui::Context) {
        let state_clone = Arc::clone(&self.import_state);
        let ctx_clone = ctx.clone();
        
        {
            let mut state = self.import_state.lock().unwrap();
            state.is_loading = true;
            state.result_message = None;
            state.mamo_decks.clear();
            state.selected_mamo_decks.clear();
        }
        
        tokio::spawn(async move {
            let result = fetch_mamo_user_decks(&username).await;
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            
            match result {
                Ok(decks) => {
                    state.selected_mamo_decks = vec![false; decks.len()];
                    state.result_message = Some(format!("Found {} MaMo decks for {}", decks.len(), username));
                    state.mamo_decks = decks;
                }
                Err(e) => {
                    state.result_message = Some(format!("Error: Failed to fetch MaMo decks: {}", e));
                }
            }
            
            ctx_clone.request_repaint();
        });
    }
    
    /// Import selected MaMo decks
    fn import_selected_mamo_decks(&mut self, ctx: &egui::Context) {
        let deck_ids: Vec<String> = {
            let state = self.import_state.lock().unwrap();
            state.mamo_decks.iter()
                .enumerate()
                .filter(|(i, _)| state.selected_mamo_decks.get(*i).copied().unwrap_or(false))
                .map(|(_, d)| d.deck_id.clone())
                .collect()
        };
        
        if deck_ids.is_empty() {
            return;
        }
        
        let state_clone = Arc::clone(&self.import_state);
        let ctx_clone = ctx.clone();
        let total = deck_ids.len();
        
        {
            let mut state = self.import_state.lock().unwrap();
            state.is_loading = true;
            state.result_message = Some(format!("Importing {} MaMo decks...", total));
        }
        
        tokio::spawn(async move {
            let mut success_count = 0;
            let mut fail_count = 0;
            
            for deck_id in &deck_ids {
                let result = create_deck_from_mamo(deck_id).await;
                
                match result {
                    Ok(deck_result) if deck_result.success => success_count += 1,
                    Ok(_) => fail_count += 1,
                    Err(e) => {
                        log::warn!("Failed to import MaMo deck {}: {}", deck_id, e);
                        fail_count += 1;
                    }
                }
            }
            
            let mut state = state_clone.lock().unwrap();
            state.is_loading = false;
            state.result_message = Some(format!(
                "Imported {} of {} MaMo decks ({} failed)",
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
                    UrlType::MamoUser(username) => {
                        ui.label(egui::RichText::new(format!("✓ MaMo User: {} (all decks)", username)).color(egui::Color32::from_rgb(0, 128, 0)));
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
                    // Use native file dialog to pick folder
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_title("Select Forge Game Log Directory")
                        .pick_folder()
                    {
                        let folder_str = folder.to_string_lossy().to_string();
                        let valid = validate_directory(&folder_str).unwrap_or(false);
                        let mut state = self.gamelog_state.lock().unwrap();
                        state.directory_input = folder_str;
                        state.directory_valid = valid;
                        state.file_count = None;
                    }
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
        
        // Filter options section
        ui.group(|ui| {
            ui.label(egui::RichText::new("🔧 Filter Options").strong());
            ui.add_space(5.0);
            
            // Days filter
            ui.horizontal(|ui| {
                ui.label("Only upload logs from last");
                let mut days_input = {
                    let state = self.gamelog_state.lock().unwrap();
                    state.days_filter_input.clone()
                };
                let response = ui.add(
                    egui::TextEdit::singleline(&mut days_input)
                        .desired_width(50.0)
                        .hint_text("0")
                );
                if response.changed() {
                    let mut state = self.gamelog_state.lock().unwrap();
                    state.days_filter_input = days_input.clone();
                    // Parse and update days filter
                    state.days_filter = days_input.parse().unwrap_or(0);
                }
                ui.label("days (0 = all)");
            });
            
            ui.add_space(5.0);
            
            // Deck filter dropdown
            let (user_decks, selected_deck_filters, show_dropdown) = {
                let state = self.gamelog_state.lock().unwrap();
                (state.user_decks.clone(), state.selected_deck_filters.clone(), state.show_deck_filter_dropdown)
            };
            
            ui.horizontal(|ui| {
                ui.label("Filter by decks:");
                
                let button_text = if selected_deck_filters.is_empty() {
                    "All Decks ▼".to_string()
                } else {
                    format!("{} deck(s) selected ▼", selected_deck_filters.len())
                };
                
                if ui.button(&button_text).clicked() {
                    let mut state = self.gamelog_state.lock().unwrap();
                    state.show_deck_filter_dropdown = !state.show_deck_filter_dropdown;
                }
                
                if !selected_deck_filters.is_empty() {
                    if ui.button("Clear").clicked() {
                        let mut state = self.gamelog_state.lock().unwrap();
                        state.selected_deck_filters.clear();
                    }
                }
            });
            
            // Deck filter dropdown content
            if show_dropdown && !user_decks.is_empty() {
                ui.indent("deck_filter_dropdown", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for deck in &user_decks {
                                let mut selected = selected_deck_filters.contains(&deck.deck_name);
                                if ui.checkbox(&mut selected, &deck.deck_name).changed() {
                                    let mut state = self.gamelog_state.lock().unwrap();
                                    if selected {
                                        state.selected_deck_filters.insert(deck.deck_name.clone());
                                    } else {
                                        state.selected_deck_filters.remove(&deck.deck_name);
                                    }
                                }
                            }
                        });
                });
            } else if show_dropdown && user_decks.is_empty() {
                ui.label(egui::RichText::new("No decks loaded. Click 'Refresh Decks' below.").small().weak());
            }
            
            // Show current filter summary
            let days_filter = {
                let state = self.gamelog_state.lock().unwrap();
                state.days_filter
            };
            if days_filter > 0 || !selected_deck_filters.is_empty() {
                ui.add_space(3.0);
                let mut filter_parts = Vec::new();
                if days_filter > 0 {
                    filter_parts.push(format!("Last {} days", days_filter));
                }
                if !selected_deck_filters.is_empty() {
                    filter_parts.push(format!("{} decks", selected_deck_filters.len()));
                }
                ui.label(egui::RichText::new(format!("Active filters: {}", filter_parts.join(", "))).small().color(egui::Color32::from_rgb(0, 100, 200)));
            }
        });
        
        ui.add_space(10.0);
        
        // Scan controls section
        let (is_previewing, preview_results) = {
            let state = self.gamelog_state.lock().unwrap();
            (state.is_previewing, state.preview_results.clone())
        };
        
        ui.group(|ui| {
            ui.label(egui::RichText::new("🔍 Scan Controls").strong());
            ui.add_space(5.0);
            
            ui.horizontal(|ui| {
                // Preview button - shows what would be uploaded
                if ui.add_enabled(!is_scanning && !is_previewing && directory_valid, egui::Button::new("👁 Preview")).clicked() {
                    self.preview_gamelog_scan();
                }
                
                // Manual scan button
                if ui.add_enabled(!is_scanning && directory_valid, egui::Button::new("🔄 Upload")).clicked() {
                    self.start_gamelog_scan(ctx);
                }
                
                // Background scanning toggle
                let mut bg_enabled = background_enabled;
                if ui.checkbox(&mut bg_enabled, "Enable Background Scanning").changed() {
                    self.toggle_background_scanning(bg_enabled);
                }
                
                if is_scanning {
                    ui.spinner();
                    ui.label("Uploading...");
                }
                if is_previewing {
                    ui.spinner();
                    ui.label("Scanning...");
                }
            });
            
            if background_enabled {
                ui.label(egui::RichText::new("Background scanning is active. New logs will be uploaded automatically.").small().weak());
            }
        });
        
        // Preview results section
        if !preview_results.is_empty() {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.label(egui::RichText::new(format!("📋 Preview: {} files to upload", preview_results.len())).strong());
                ui.add_space(5.0);
                
                // Count by deck
                let mut deck_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for p in &preview_results {
                    let deck = p.detected_deck.clone().unwrap_or_else(|| "Unknown".to_string());
                    *deck_counts.entry(deck).or_insert(0) += 1;
                }
                
                // Show deck summary
                ui.label(egui::RichText::new("Decks detected:").small());
                for (deck, count) in &deck_counts {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("  • {} ({})", deck, count)).small().color(egui::Color32::from_rgb(100, 149, 237)));
                    });
                }
                
                ui.add_space(5.0);
                
                // File list
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for preview in &preview_results {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("○").color(egui::Color32::GRAY));
                                ui.label(&preview.filename);
                                ui.label(egui::RichText::new(format!("({} bytes)", preview.file_size)).small().weak());
                                if let Some(ref deck) = preview.detected_deck {
                                    ui.label(egui::RichText::new(format!("→ {}", deck)).small().color(egui::Color32::from_rgb(100, 149, 237)));
                                } else {
                                    ui.label(egui::RichText::new("→ ?").small().color(egui::Color32::GRAY));
                                }
                            });
                        }
                    });
                
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear Preview").clicked() {
                        let mut state = self.gamelog_state.lock().unwrap();
                        state.preview_results.clear();
                    }
                });
            });
        }
        
        ui.add_space(10.0);
        
        // Deck Mapping section
        self.render_deck_mapping_section(ui, ctx);
        
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
                                // Show detected deck if available
                                if let Some(ref deck) = result.deck_identifier {
                                    ui.label(egui::RichText::new(format!("→ {}", deck)).small().color(egui::Color32::from_rgb(100, 149, 237)));
                                }
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

    fn preview_gamelog_scan(&mut self) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        
        // Get filter options
        let filter_options = {
            let state = gamelog_state.lock().unwrap();
            GameLogFilterOptions {
                days_filter: state.days_filter,
                deck_filter: state.selected_deck_filters.clone(),
            }
        };
        
        // Mark as previewing
        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_previewing = true;
            state.preview_results.clear();
        }
        
        // Run preview synchronously (it's fast, just reads files)
        let config = {
            let settings = settings.lock().unwrap();
            settings.gamelog_config.clone()
        };
        
        let processed_files = {
            let state = gamelog_state.lock().unwrap();
            state.processed_files.clone()
        };
        
        let result = preview_scan(&config, &processed_files, &filter_options);
        
        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_previewing = false;
            
            match result {
                Ok(previews) => {
                    let count = previews.len();
                    state.preview_results = previews.into_iter().map(|p| FilePreviewInfo {
                        filename: p.filename,
                        file_size: p.file_size,
                        detected_deck: p.detected_deck,
                        modified_date: p.modified_date,
                    }).collect();
                    state.status_message = Some(format!("Preview: {} files ready to upload", count));
                }
                Err(e) => {
                    state.status_message = Some(format!("Preview error: {}", e));
                }
            }
        }
    }

    fn start_gamelog_scan(&mut self, ctx: &egui::Context) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let ctx_clone = ctx.clone();
        
        // Get filter options
        let filter_options = {
            let state = gamelog_state.lock().unwrap();
            GameLogFilterOptions {
                days_filter: state.days_filter,
                deck_filter: state.selected_deck_filters.clone(),
            }
        };
        
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
            
            let result = process_new_logs_with_filter(&config, &processed_files, &filter_options).await;
            
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

    /// Start an automatic gamelog scan (no filters, triggered by Forge process tracking)
    fn start_auto_gamelog_scan(&mut self, ctx: &egui::Context) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let activity_log = Arc::clone(&self.activity_log);
        let ctx_clone = ctx.clone();
        
        // Don't scan if already scanning
        {
            let state = gamelog_state.lock().unwrap();
            if state.is_scanning {
                return;
            }
        }
        
        // Mark as scanning
        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_scanning = true;
        }
        
        // No filters for auto-scan - scan all new logs
        let filter_options = GameLogFilterOptions {
            days_filter: 0,
            deck_filter: HashSet::new(),
        };
        
        tokio::spawn(async move {
            let config = {
                let settings = settings.lock().unwrap();
                settings.gamelog_config.clone()
            };
            
            let processed_files = {
                let state = gamelog_state.lock().unwrap();
                Arc::new(Mutex::new(state.processed_files.clone()))
            };
            
            let result = process_new_logs_with_filter(&config, &processed_files, &filter_options).await;
            
            {
                let mut state = gamelog_state.lock().unwrap();
                state.is_scanning = false;
                
                match result {
                    Ok(summary) => {
                        state.scan_results = summary.results.clone();
                        
                        // Update processed files
                        let new_processed = processed_files.lock().unwrap().clone();
                        state.processed_files = new_processed.clone();
                        let _ = save_processed_files(&new_processed);
                        
                        // Log to activity
                        if summary.new_files > 0 || summary.failed_uploads > 0 {
                            if let Ok(mut log) = activity_log.lock() {
                                if summary.failed_uploads > 0 && summary.successfully_uploaded == 0 {
                                    // All failed — find first distinct error message
                                    let first_error = summary.results.iter()
                                        .find(|r| !r.success)
                                        .map(|r| r.message.as_str())
                                        .unwrap_or("Unknown error");
                                    log.log_error(format!(
                                        "\u{1F4CB} Auto-scan: {} new files, 0 uploaded, {} failed — {}",
                                        summary.new_files, summary.failed_uploads, first_error
                                    ));
                                } else if summary.failed_uploads > 0 {
                                    // Partial failure
                                    let first_error = summary.results.iter()
                                        .find(|r| !r.success)
                                        .map(|r| r.message.as_str())
                                        .unwrap_or("Unknown error");
                                    log.log_success(format!(
                                        "\u{1F4CB} Auto-scan: {} new files, {} uploaded, {} failed — {}",
                                        summary.new_files, summary.successfully_uploaded, summary.failed_uploads, first_error
                                    ));
                                } else {
                                    log.log_success(format!(
                                        "\u{1F4CB} Auto-scan: {} new files, {} uploaded, {} failed",
                                        summary.new_files, summary.successfully_uploaded, summary.failed_uploads
                                    ));
                                }
                            }
                        }
                        
                        state.last_scan_summary = Some(summary);
                    }
                    Err(e) => {
                        if let Ok(mut log) = activity_log.lock() {
                            log.log_error(format!("Auto-scan error: {}", e));
                        }
                    }
                }
            }
            
            ctx_clone.request_repaint();
        });
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

    // ==================== Deck Mapping ====================

    fn render_deck_mapping_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (user_decks, is_fetching, deck_mappings, deck_search_filter) = {
            let state = self.gamelog_state.lock().unwrap();
            (
                state.user_decks.clone(),
                state.is_fetching_decks,
                state.deck_mappings.clone(),
                state.deck_search_filter.clone(),
            )
        };
        
        ui.group(|ui| {
            ui.label(egui::RichText::new("🎯 Deck Mapping").strong());
            ui.add_space(5.0);
            ui.label(egui::RichText::new("Map deck names from game logs to your MaMo decks.").small().weak());
            ui.add_space(5.0);
            
            ui.horizontal(|ui| {
                if ui.add_enabled(!is_fetching, egui::Button::new("🔄 Fetch My Decks")).clicked() {
                    self.fetch_my_mamo_decks(ctx);
                }
                
                if is_fetching {
                    ui.spinner();
                    ui.label("Fetching...");
                } else {
                    ui.label(format!("{} decks loaded", user_decks.len()));
                }
            });
            
            // Show all loaded decks in a collapsible section
            if !user_decks.is_empty() {
                egui::CollapsingHeader::new("📋 My Decks")
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for deck in &user_decks {
                                    let colors = deck.color_identity.as_ref()
                                        .map(|c| c.join(""))
                                        .unwrap_or_else(|| "C".to_string());
                                    ui.label(format!("• {} [{}]", deck.deck_name, colors));
                                }
                            });
                    });
            }
            
            // Show current mappings
            if !deck_mappings.mappings.is_empty() {
                ui.add_space(5.0);
                ui.label(egui::RichText::new("Current Mappings:").small());
                
                egui::ScrollArea::vertical()
                    .max_height(100.0)
                    .show(ui, |ui| {
                        let mappings_to_remove: Vec<String> = {
                            let mut to_remove = Vec::new();
                            for (log_name, deck_id) in &deck_mappings.mappings {
                                ui.horizontal(|ui| {
                                    // Find deck name for this ID
                                    let deck_name = user_decks.iter()
                                        .find(|d| &d.deck_id == deck_id)
                                        .map(|d| d.deck_name.as_str())
                                        .unwrap_or("(Unknown deck)");
                                    
                                    ui.label(format!("\"{}\"", log_name));
                                    ui.label("→");
                                    ui.label(egui::RichText::new(deck_name).color(egui::Color32::from_rgb(0, 128, 0)));
                                    
                                    if ui.small_button("✕").clicked() {
                                        to_remove.push(log_name.clone());
                                    }
                                });
                            }
                            to_remove
                        };
                        
                        // Remove mappings outside the borrow
                        if !mappings_to_remove.is_empty() {
                            let mut state = self.gamelog_state.lock().unwrap();
                            for name in mappings_to_remove {
                                state.deck_mappings.remove_mapping(&name);
                            }
                            let _ = state.deck_mappings.save();
                        }
                    });
            }
            
            // Add new mapping section
            if !user_decks.is_empty() {
                ui.add_space(5.0);
                ui.separator();
                ui.label(egui::RichText::new("Add New Mapping:").small());
                
                ui.horizontal(|ui| {
                    ui.label("Deck name in logs:");
                    let mut mapping_name = {
                        let state = self.gamelog_state.lock().unwrap();
                        state.mapping_deck_name.clone().unwrap_or_default()
                    };
                    if ui.text_edit_singleline(&mut mapping_name).changed() {
                        let mut state = self.gamelog_state.lock().unwrap();
                        state.mapping_deck_name = if mapping_name.is_empty() { None } else { Some(mapping_name) };
                    }
                });
                
                // Show suggested matches if we have a deck name
                let mapping_deck_name = {
                    let state = self.gamelog_state.lock().unwrap();
                    state.mapping_deck_name.clone()
                };
                
                if let Some(ref name) = mapping_deck_name {
                    if !name.is_empty() {
                        let suggestions = suggest_deck_matches(name, &user_decks, 5);
                        
                        if !suggestions.is_empty() {
                            ui.label(egui::RichText::new("Suggested matches:").small().weak());
                            
                            for suggestion in suggestions {
                                let score_pct = (suggestion.score * 100.0) as u32;
                                let label = format!("{} ({}%)", suggestion.deck.deck_name, score_pct);
                                
                                if ui.button(&label).clicked() {
                                    // Save the mapping
                                    {
                                        let mut state = self.gamelog_state.lock().unwrap();
                                        state.deck_mappings.set_mapping(name, &suggestion.deck.deck_id);
                                        let _ = state.deck_mappings.save();
                                        state.mapping_deck_name = None;
                                        state.status_message = Some(format!(
                                            "Mapped \"{}\" → \"{}\"", 
                                            name, 
                                            suggestion.deck.deck_name
                                        ));
                                    }
                                }
                            }
                        }
                        
                        // Also show full deck list with search
                        ui.add_space(5.0);
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            let mut filter = deck_search_filter.clone();
                            if ui.text_edit_singleline(&mut filter).changed() {
                                let mut state = self.gamelog_state.lock().unwrap();
                                state.deck_search_filter = filter;
                            }
                        });
                        
                        let filter_lower = deck_search_filter.to_lowercase();
                        let filtered_decks: Vec<_> = user_decks.iter()
                            .filter(|d| {
                                filter_lower.is_empty() || 
                                d.deck_name.to_lowercase().contains(&filter_lower)
                                // Note: We only have commander IDs, not names, so we can only filter by deck name
                            })
                            .take(10)
                            .collect();
                        
                        if !filtered_decks.is_empty() {
                            egui::ScrollArea::vertical()
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    for deck in filtered_decks {
                                        // Display deck name (commander names would require additional lookup)
                                        let deck_label = deck.deck_name.clone();
                                        
                                        if ui.button(&deck_label).clicked() {
                                            // Save the mapping
                                            {
                                                let mut state = self.gamelog_state.lock().unwrap();
                                                state.deck_mappings.set_mapping(name, &deck.deck_id);
                                                let _ = state.deck_mappings.save();
                                                state.mapping_deck_name = None;
                                                state.status_message = Some(format!(
                                                    "Mapped \"{}\" → \"{}\"", 
                                                    name, 
                                                    deck.deck_name
                                                ));
                                            }
                                        }
                                    }
                                });
                        }
                    }
                }
            }
        });
    }

    fn fetch_my_mamo_decks(&mut self, ctx: &egui::Context) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let ctx_clone = ctx.clone();
        
        // Mark as fetching
        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_fetching_decks = true;
        }
        
        tokio::spawn(async move {
            let config = {
                let settings = settings.lock().unwrap();
                settings.gamelog_config.clone()
            };
            
            let result = fetch_my_decks(&config).await;
            
            {
                let mut state = gamelog_state.lock().unwrap();
                state.is_fetching_decks = false;
                
                match result {
                    Ok(decks) => {
                        // Save to cache
                        if let Err(e) = save_cached_decks(&decks) {
                            log::warn!("Failed to cache decks: {}", e);
                        }
                        state.user_decks = decks;
                        state.status_message = Some(format!("Loaded {} decks from MaMo", state.user_decks.len()));
                    }
                    Err(e) => {
                        state.status_message = Some(format!("Failed to fetch decks: {}", e));
                    }
                }
            }
            
            ctx_clone.request_repaint();
        });
    }

    // ==================== Settings Tab ====================

    fn render_settings_tab(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.label(egui::RichText::new("⚙ Settings").strong());
        ui.add_space(10.0);
        
        // Get current state
        let (forge_path_input, forge_path_valid, forge_auto_launch, status_message) = {
            let state = self.settings_state.lock().unwrap();
            (
                state.forge_path_input.clone(),
                state.forge_path_valid,
                state.forge_auto_launch,
                state.status_message.clone(),
            )
        };
        
        // Forge Configuration Section
        ui.group(|ui| {
            ui.label(egui::RichText::new("🎮 Forge Integration").strong());
            ui.add_space(5.0);
            
            ui.label("Configure Forge MTG for playtesting decks directly from MaMo.");
            ui.add_space(10.0);
            
            // Forge path input
            ui.horizontal(|ui| {
                ui.label("Forge Executable:");
                
                let mut path_input = forge_path_input.clone();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut path_input)
                        .desired_width(400.0)
                        .hint_text("Path to forge.exe, .jar, Forge.app, or target/ dir")
                );
                
                if response.changed() {
                    let mut state = self.settings_state.lock().unwrap();
                    state.forge_path_input = path_input.clone();
                    state.forge_path_valid = validate_forge_path(&path_input);
                }
                
                // Status indicator
                if !forge_path_input.is_empty() {
                    if forge_path_valid {
                        ui.label(egui::RichText::new("✓").color(egui::Color32::from_rgb(0, 128, 0)));
                    } else {
                        ui.label(egui::RichText::new("✗").color(egui::Color32::from_rgb(176, 0, 32)));
                    }
                }
            });
            
            ui.add_space(5.0);
            
            // Auto-detect and Browse buttons
            ui.horizontal(|ui| {
                if ui.button("🔍 Auto-detect").clicked() {
                    if let Some(path) = get_default_forge_path() {
                        let path_str = path.to_string_lossy().to_string();
                        let mut state = self.settings_state.lock().unwrap();
                        state.forge_path_input = path_str.clone();
                        state.forge_path_valid = true;
                        state.status_message = Some(format!("Found Forge at: {}", path_str));
                    } else {
                        let mut state = self.settings_state.lock().unwrap();
                        state.status_message = Some("Could not find Forge installation automatically.".to_string());
                    }
                }
                
                if ui.button("📁 Browse...").clicked() {
                    let dialog = FileDialog::new()
                        .add_filter("Forge Executable", &["exe", "jar", "bat"])
                        .add_filter("All Files", &["*"])
                        .set_title("Select Forge Executable");
                    
                    if let Some(path) = dialog.pick_file() {
                        let path_str = path.to_string_lossy().to_string();
                        let is_valid = validate_forge_path(&path_str);
                        let mut state = self.settings_state.lock().unwrap();
                        state.forge_path_input = path_str.clone();
                        state.forge_path_valid = is_valid;
                        if is_valid {
                            state.status_message = Some(format!("Selected: {}", path_str));
                        } else {
                            state.status_message = Some(format!("Warning: {} may not be a valid Forge executable", path_str));
                        }
                    }
                }
                
                if ui.button("� Folder...").clicked() {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_title("Select Forge Directory (e.g. forge-gui-desktop/target/)")
                        .pick_folder()
                    {
                        let path_str = folder.to_string_lossy().to_string();
                        let is_valid = validate_forge_path(&path_str);
                        let mut state = self.settings_state.lock().unwrap();
                        state.forge_path_input = path_str.clone();
                        state.forge_path_valid = is_valid;
                        if is_valid {
                            if let Some(jar) = resolve_latest_forge_jar(&folder) {
                                state.status_message = Some(format!(
                                    "Folder OK — will launch: {}",
                                    jar.file_name().unwrap_or_default().to_string_lossy()
                                ));
                            }
                        } else {
                            state.status_message = Some(format!(
                                "No forge-gui-desktop JAR found in: {}", path_str
                            ));
                        }
                    }
                }

                if ui.button("💾 Save").clicked() {
                    self.save_forge_settings();
                }
            });

            ui.add_space(5.0);

            // If a directory is configured, show which JAR will be launched
            if forge_path_valid {
                let p = std::path::Path::new(&forge_path_input);
                if p.is_dir() {
                    if let Some(jar) = resolve_latest_forge_jar(p) {
                        ui.label(
                            egui::RichText::new(format!(
                                "→  {}",
                                jar.file_name().unwrap_or_default().to_string_lossy()
                            ))
                            .color(egui::Color32::from_rgb(80, 130, 200))
                            .small(),
                        );
                        ui.add_space(3.0);
                    }
                }
            }

            // Auto-launch checkbox
            let mut auto_launch = forge_auto_launch;
            if ui.checkbox(&mut auto_launch, "Auto-launch Forge after downloading deck").changed() {
                let mut state = self.settings_state.lock().unwrap();
                state.forge_auto_launch = auto_launch;
            }
            
            ui.add_space(5.0);
            
            // Test launch button
            if ui.add_enabled(forge_path_valid, egui::Button::new("🚀 Test Launch Forge")).clicked() {
                match launch_forge_from_settings(None) {
                    Ok(result) => {
                        let mut state = self.settings_state.lock().unwrap();
                        state.status_message = Some(result.message);
                    }
                    Err(e) => {
                        let mut state = self.settings_state.lock().unwrap();
                        state.status_message = Some(format!("Launch failed: {}", e));
                    }
                }
            }
        });
        
        ui.add_space(15.0);
        
        // Authentication Section
        ui.group(|ui| {
            ui.label(egui::RichText::new("🔐 MaMo Authentication").strong());
            ui.add_space(5.0);
            
            // Check if we have a token
            let has_token = {
                let state = self.settings_state.lock().unwrap();
                !state.auth_token_input.is_empty()
            };
            
            if has_token {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("✓ Connected to MaMo").color(egui::Color32::from_rgb(0, 128, 0)));
                    ui.label("- Game log uploads enabled");
                });
                ui.add_space(5.0);
                if ui.button("🔓 Disconnect").clicked() {
                    {
                        let mut state = self.settings_state.lock().unwrap();
                        state.auth_token_input.clear();
                    }
                    self.save_auth_token();
                }
            } else {
                ui.label("Not connected. Click 'Connect MaMo Connector' in MaMo settings to enable game log uploads.");
                ui.add_space(5.0);
                ui.label(egui::RichText::new("Or manually enter your token:").small().weak());
            }
            
            // Manual token input (collapsed if already connected)
            if !has_token {
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.label("Token:");
                    
                    let mut token_input = {
                        let state = self.settings_state.lock().unwrap();
                        state.auth_token_input.clone()
                    };
                    
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut token_input)
                            .desired_width(350.0)
                            .password(true)
                            .hint_text("Paste token here")
                    );
                    
                    if response.changed() {
                        let mut state = self.settings_state.lock().unwrap();
                        state.auth_token_input = token_input;
                    }
                    
                    if ui.button("💾 Save").clicked() {
                        self.save_auth_token();
                    }
                });
            }
        });
        
        ui.add_space(15.0);
        
        // URL Scheme Info
        ui.group(|ui| {
            ui.label(egui::RichText::new("🔗 Deeplink Commands").strong());
            ui.add_space(5.0);
            
            ui.label("MaMo Connector responds to the following deeplink commands:");
            ui.add_space(5.0);
            
            egui::Grid::new("deeplink_commands")
                .num_columns(2)
                .spacing([20.0, 4.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Command").strong());
                    ui.label(egui::RichText::new("Description").strong());
                    ui.end_row();
                    
                    ui.label("mamoConnector://playtest/DECK_UUID");
                    ui.label("Download deck & launch Forge");
                    ui.end_row();
                    
                    ui.label("mamoConnector://mamo/DECK_UUID");
                    ui.label("Download deck from MaMo");
                    ui.end_row();
                    
                    ui.label("mamoConnector://deck/MOXFIELD_ID");
                    ui.label("Download deck from Moxfield");
                    ui.end_row();
                    
                    ui.label("mamoConnector://launch-forge");
                    ui.label("Launch Forge (no deck)");
                    ui.end_row();
                    
                    ui.label("mamoConnector://auth?token=XXX");
                    ui.label("Set auth token for gamelog uploads");
                    ui.end_row();
                });
        });
        
        // Status message
        if let Some(msg) = status_message {
            ui.add_space(10.0);
            let color = if msg.contains("failed") || msg.contains("Could not") || msg.contains("Error") {
                egui::Color32::from_rgb(176, 0, 32)
            } else if msg.contains("Found") || msg.contains("Saved") || msg.contains("success") {
                egui::Color32::from_rgb(0, 128, 0)
            } else {
                egui::Color32::from_rgb(100, 100, 100)
            };
            ui.label(egui::RichText::new(msg).color(color));
        }
    }

    fn save_forge_settings(&mut self) {
        let (forge_path, auto_launch) = {
            let state = self.settings_state.lock().unwrap();
            (state.forge_path_input.clone(), state.forge_auto_launch)
        };
        
        // Save to settings
        {
            let mut settings = self.settings.lock().unwrap();
            settings.forge_path = if forge_path.is_empty() { None } else { Some(forge_path.clone()) };
            settings.forge_auto_launch = auto_launch;
            
            if let Err(e) = settings.save() {
                let mut state = self.settings_state.lock().unwrap();
                state.status_message = Some(format!("Failed to save settings: {}", e));
                return;
            }
        }
        
        let mut state = self.settings_state.lock().unwrap();
        state.status_message = Some("Settings saved successfully!".to_string());
    }

    fn save_auth_token(&mut self) {
        let auth_token = {
            let state = self.settings_state.lock().unwrap();
            state.auth_token_input.clone()
        };
        
        // Save to settings and gamelog config
        {
            let mut settings = self.settings.lock().unwrap();
            settings.auth_token = if auth_token.is_empty() { None } else { Some(auth_token.clone()) };
            // Also update gamelog config's auth token
            settings.gamelog_config.auth_token = settings.auth_token.clone();
            
            if let Err(e) = settings.save() {
                let mut state = self.settings_state.lock().unwrap();
                state.status_message = Some(format!("Failed to save token: {}", e));
                return;
            }
        }
        
        let mut state = self.settings_state.lock().unwrap();
        if auth_token.is_empty() {
            state.status_message = Some("Token cleared.".to_string());
        } else {
            state.status_message = Some("Token saved successfully!".to_string());
        }
    }
}
