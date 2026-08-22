use anyhow::Result;
use chrono::Local;
use eframe::{NativeOptions, egui};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::commands::CommandResult;
use crate::deck::{create_deck_from_moxfield, MoxfieldDeckEntry, MamoDeckEntry, DeckStatus, fetch_user_decks_direct, create_deck_from_archidekt, create_deck_from_deckstats, create_deck_from_mamo, parse_archidekt_url, parse_deckstats_url, parse_mamo_url, parse_mamo_user_url, fetch_mamo_user_decks, sync_moxfield_deck, sync_moxfield_user_decks, sync_archidekt_deck, sync_deckstats_deck, sync_mamo_deck, DeckSyncResult, SyncStatus, get_deck_directory_display};
use rfd::FileDialog;
use crate::deeplink::Deeplink;
use crate::forge::{get_default_forge_path, resolve_latest_forge_jar, validate_forge_path, launch_forge_from_settings, list_forge_decks, ForgeLaunchResult};
use crate::gamelog::{GameLogConfig, GameLogProcessResult, ScanSummary, get_default_forge_log_directory, validate_directory, scan_directory, load_processed_files, save_processed_files, DeckMappings, fetch_my_decks, suggest_deck_matches, load_cached_decks, save_cached_decks, process_new_logs_with_filter, GameLogFilterOptions, FilePreviewInfo};
use crate::registration::{RegistrationOutcome, RegistrationStatus};
use crate::settings::{Settings, SavedLink, SavedLinkType};

#[derive(Clone, PartialEq, Eq)]
enum Tab {
    Play,
    Decks,
    Setup,
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

/// State for the background version check and in-app update
#[derive(Clone, Default)]
struct UpdateCheckState {
    /// Some(version_string) when a newer release is available
    available_version: Option<String>,
    asset: Option<crate::download::ConnectorAsset>,
    staged_path: Option<std::path::PathBuf>,
    is_downloading: bool,
    busy: bool,
    dismissed: bool,
    error: Option<String>,
}

/// A downloaded-but-not-yet-installed Forge update, waiting for Forge to not be running so it
/// can be safely swapped into place (see the periodic check in `update()`).
#[derive(Clone)]
struct StagedForgeUpdate {
    staged_path: std::path::PathBuf,
    asset: crate::download::ForgeAsset,
}

/// State for the background MaMo Forge update check — only meaningful when the configured
/// Forge install is one the Connector downloaded itself (see `is_connector_managed_forge`).
/// The whole flow is automatic: detecting an update starts the download immediately (no click),
/// and it's installed the moment Forge is confirmed not running — a click is only ever needed
/// to dismiss an error or to ask for an out-of-schedule check ("Check now").
#[derive(Clone, Default)]
struct ForgeUpdateCheckState {
    /// Set once a background download finishes and is staged, waiting to be swapped in.
    staged: Option<StagedForgeUpdate>,
    /// True while a check or download is actively in flight — guards against starting a second
    /// one (e.g. from "Check now") while one's already running.
    busy: bool,
    dismissed: bool,
}

/// Destructive actions that require a confirmation dialog
#[derive(Clone, PartialEq, Eq)]
enum ConfirmAction {
    ResetFirstRun,
    Uninstall,
}

/// The MaMo website's own base URL — the one place the Connector links back out to it.
const MAMO_WEBSITE_URL: &str = "https://ma-mo-frontend.vercel.app";

/// Drives the Play tab's persistent status strip and activity timeline. This mirrors what
/// Connector is actually doing right now — launching, playing, scanning, uploading — rather
/// than the activity log's scrolling lines, so it stays legible even after tabbing away, and
/// it's the same story for a deck launched here or a playtest started from the website.
#[derive(Clone, Default, PartialEq)]
enum PlaySession {
    /// Nothing in progress — idle between games (this is also where things settle back to
    /// after an upload, so it never reads as a dead end).
    #[default]
    Watching,
    /// A deck is being downloaded/prepared and Forge is about to start.
    Launching,
    /// Forge is open and being monitored.
    Playing,
    /// Forge just closed; checking what was played.
    Scanning,
    /// A found game log is being sent to MaMo.
    Uploading,
    /// Upload succeeded — `deck_id` (if the log matched a known deck) lets the UI link
    /// straight to that deck's analysis instead of just reporting a filename.
    Uploaded { deck_id: Option<String>, filename: String },
    /// Upload attempted but failed, or the scan itself errored.
    UploadIssue { message: String },
}

/// One line describing what's happening right now, for the persistent status strip shown on
/// every tab, and whether that's an "actively doing something" state (accent dot) vs. settled
/// (green dot) — it never goes blank, so it's always visible that Connector is still watching.
fn play_session_strip(ps: &PlaySession) -> (String, bool) {
    match ps {
        PlaySession::Watching => (
            "Watching for a game to start — keep this window open (minimized is fine) while you play".to_string(),
            false,
        ),
        PlaySession::Launching => ("Launching Forge…".to_string(), true),
        PlaySession::Playing => (
            "Game in progress — still watching, no need to close Connector".to_string(),
            true,
        ),
        PlaySession::Scanning => ("Scanning for your game log…".to_string(), true),
        PlaySession::Uploading => ("Uploading…".to_string(), true),
        PlaySession::Uploaded { filename, .. } => (
            format!("Uploaded {filename} — back to watching for your next game"),
            false,
        ),
        PlaySession::UploadIssue { message } => (format!("Upload issue — {message}"), false),
    }
}

/// Index of `ps` within the linear Watching→Uploaded sequence, for coloring the Play tab's
/// timeline cards (earlier = done, this one = active, later = pending). `UploadIssue` maps to
/// the Uploading slot — that's the step that actually failed — and is rendered with its own
/// error styling there instead of the normal "active" styling.
fn play_session_step_index(ps: &PlaySession) -> usize {
    match ps {
        PlaySession::Watching => 0,
        PlaySession::Launching => 1,
        PlaySession::Playing => 2,
        PlaySession::Scanning => 3,
        PlaySession::Uploading => 4,
        PlaySession::UploadIssue { .. } => 4,
        PlaySession::Uploaded { .. } => 5,
    }
}

/// Coordinates the single gamelog-scan "slot" shared by the periodic auto-scan, the
/// Forge-closed final scan, and the manual "Upload Logs" button — only one
/// `process_new_logs_with_filter` call may run at a time. Also tracks whether a caller that
/// wants the Play tab's timeline resolved (moved to Uploaded/UploadIssue/Watching) is still
/// waiting: if a final or manual scan asks to run while another scan already holds the slot,
/// that intent isn't lost — recorded regardless of whether the slot was actually claimed, so
/// whichever scan currently holds it will resolve play_session with its own results when it
/// finishes, instead of leaving the Play tab's timeline stuck mid-step forever.
#[derive(Clone, Default)]
struct ScanSlot {
    busy: Arc<AtomicBool>,
    resolution_owed: Arc<AtomicBool>,
}

impl ScanSlot {
    /// Attempts to claim the slot for a new scan. `wants_resolution` is recorded unconditionally
    /// (even if the slot can't be claimed), so an overlapping request's intent still gets
    /// honored by whichever scan is actually running. Returns `true` if the caller may proceed
    /// to spawn a scan; `false` means another scan already holds the slot.
    fn try_begin(&self, wants_resolution: bool) -> bool {
        if wants_resolution {
            self.resolution_owed.store(true, Ordering::SeqCst);
        }
        self.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    /// Releases the slot. Returns (and clears) whether a play_session resolution is owed.
    fn finish(&self) -> bool {
        self.busy.store(false, Ordering::SeqCst);
        self.resolution_owed.swap(false, Ordering::SeqCst)
    }
}

/// Steps of the first-run setup wizard
#[derive(Clone, PartialEq, Eq, Default)]
enum WizardStep {
    #[default]
    Welcome,
    DownloadForge,
    ConfigureForge,
    Done,
}

/// Status of the in-wizard Forge test launch
#[derive(Clone)]
enum WizardTestStatus {
    Testing,
    Ok,
    Err(String),
}

/// Live download progress shared between the download thread and the UI
#[derive(Clone, Default)]
struct DownloadProgress {
    bytes_done: u64,
    total_bytes: Option<u64>,
    status_text: String,
    finished: bool,
    error: Option<String>,
}

/// Terminal result from the Forge download background thread
#[derive(Clone)]
enum DownloadResult {
    Success { jar_dir: String },
    Failed(String),
    Cancelled,
}

/// State for the setup wizard (first run / forge misconfigured)
struct SetupWizardState {
    step: WizardStep,
    forge_path_input: String,
    forge_path_valid: bool,
    test_status: Option<WizardTestStatus>,
    /// Written by the test-launch background thread; polled in update()
    pending_test_result: Option<Arc<Mutex<Option<WizardTestStatus>>>>,
    /// Live progress written by the download thread; polled every frame
    download_progress: Option<Arc<Mutex<DownloadProgress>>>,
    /// Terminal download result written by the download thread; polled in update()
    download_result: Option<Arc<Mutex<Option<DownloadResult>>>>,
    /// Set by the UI Cancel button; read by the download thread
    download_cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Cached Java detection result; None = not checked yet this session
    java_status: Option<crate::forge::JavaStatus>,
}

impl Default for SetupWizardState {
    fn default() -> Self {
        Self {
            step: WizardStep::default(),
            forge_path_input: String::new(),
            forge_path_valid: false,
            test_status: None,
            pending_test_result: None,
            download_progress: None,
            download_result: None,
            download_cancelled: None,
            java_status: None,
        }
    }
}

/// Human-readable download progress string: "42.3 MB / 156.0 MB (27%)" or "42.3 MB"
fn format_download_status(done: u64, total: Option<u64>) -> String {
    let done_mb = done as f64 / (1024.0 * 1024.0);
    match total {
        Some(t) if t > 0 => {
            let total_mb = t as f64 / (1024.0 * 1024.0);
            let pct = (done as f64 / t as f64 * 100.0) as u32;
            format!("Downloading… {done_mb:.1} MB / {total_mb:.1} MB ({pct}%)")
        }
        _ => format!("Downloading… {done_mb:.1} MB"),
    }
}

/// Returns true if `remote` is a higher semver than `current` (both "X.Y.Z" strings)
fn is_newer_version(remote: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let mut parts = s.splitn(3, '.').map(|p| p.parse::<u32>().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    };
    parse(remote) > parse(current)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PillStatus {
    Success,
    Warning,
    Error,
    Neutral,
}

fn render_status_pill(ui: &mut egui::Ui, text: &str, status: PillStatus) {
    let (bg_color, fg_color, dot_color) = match status {
        PillStatus::Success => (
            egui::Color32::from_rgb(220, 245, 225),
            egui::Color32::from_rgb(20, 110, 40),
            egui::Color32::from_rgb(34, 160, 60),
        ),
        PillStatus::Warning => (
            egui::Color32::from_rgb(255, 243, 205),
            egui::Color32::from_rgb(133, 100, 4),
            egui::Color32::from_rgb(200, 140, 0),
        ),
        PillStatus::Error => (
            egui::Color32::from_rgb(253, 232, 232),
            egui::Color32::from_rgb(176, 0, 32),
            egui::Color32::from_rgb(210, 30, 30),
        ),
        PillStatus::Neutral => (
            egui::Color32::from_rgb(240, 240, 245),
            egui::Color32::from_rgb(80, 80, 95),
            egui::Color32::from_rgb(130, 130, 145),
        ),
    };

    egui::Frame::default()
        .fill(bg_color)
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new("●").color(dot_color).size(8.0));
                ui.label(egui::RichText::new(text).color(fg_color).small().strong());
            });
        });
}

/// True when a deeplink is an evaluation launch (playtest/launch-forge/simulate) that arrived
/// with no deck reference at all — neither a MaMo backend `deck_id` nor a power-user
/// `deck_path` (the escape hatch `handle_launch_forge_with_logger` offers for launching an
/// already-local deck without hitting the backend). In that case the frontend/caller genuinely
/// didn't pin a deck, so Forge should not be started deck-less; the Home tab picker should be
/// used instead.
fn is_deckless_evaluation_action(action: &str, has_deck_reference: bool) -> bool {
    !has_deck_reference && matches!(action, "playtest" | "launch-forge" | "launchforge" | "simulate")
}

/// True for deeplink actions that actually start a play session (download-and-launch or replay),
/// as opposed to account/deck-management actions like `auth` or `import-user-decks` — used to
/// gate the Play tab's status strip so it only ever says "Launching" for something that's
/// actually about to launch Forge.
fn deeplink_starts_play_session(action: &str) -> bool {
    matches!(
        action,
        "playtest" | "launch-forge" | "launchforge" | "playtest-scenario" | "replay-game" | "replaygame"
    )
}

/// Whether a deeplink references a deck by *any* means `handle_launch_forge_with_logger`
/// understands — a MaMo backend `deck_id`/`id`/`deckId`, or a local `deck_path`.
fn deeplink_has_deck_reference(deeplink: &Deeplink) -> bool {
    deeplink.deck_id.is_some()
        || crate::commands::get_parameter(&deeplink.params, "deck_path").is_some()
}

/// If `deck` has already been downloaded into the Forge deck directory (matched by the same
/// sanitized-name convention used at download time), return its file stem. Pure and testable
/// without touching egui/tokio state.
fn find_local_deck_path(deck: &crate::gamelog::UserDeck, local_decks: &[String]) -> Option<String> {
    let target = crate::deck::sanitize_filename(&deck.deck_name).to_lowercase();
    local_decks.iter().find(|stem| stem.to_lowercase() == target).cloned()
}

/// Resolves an opponent deck to pass as Forge's `--deck2` for a standalone (non-deeplink) Play
/// tab launch, so "just press Play" never lands the user in Forge's own lobby needing to
/// configure an opponent by hand. Picks a random deck from the curated opponent pool and
/// downloads it the same way any deck downloads — returns `None` (silently, logging only an
/// info line) if the pool is empty or unreachable, which callers must treat exactly like
/// "no deck2" (never block or fail the launch over this). Must be called from within an async
/// context (this itself awaits — no blocking executor tricks).
async fn resolve_curated_opponent_deck_path(activity_log: &Arc<Mutex<ActivityLogState>>) -> Option<String> {
    let curated_id = crate::deck::pick_random_curated_opponent_deck_id()?;
    let result = crate::deck::create_deck_from_mamo(&curated_id).await.ok()?;
    if !result.success {
        if let Ok(mut log) = activity_log.lock() {
            log.log_info(format!("Couldn't prepare a curated opponent deck: {}", result.message));
        }
        return None;
    }
    result.deck_path.map(|p| p.to_string_lossy().to_string())
}

/// The directory MaMo Forge gets downloaded into (a `forge` subfolder of the Connector's own
/// settings dir) — separate from wherever the user's own Forge install lives.
fn forge_download_dir() -> std::path::PathBuf {
    crate::settings::get_settings_dir()
        .map(|d| d.join("forge"))
        .unwrap_or_else(|_| std::path::PathBuf::from("forge"))
}

/// True when `forge_path` is the Connector's own managed Forge download directory, rather than
/// a Forge install the user pointed at manually. Only Connector-managed installs are safe to
/// offer an "update" for — there's no "latest" baseline to compare a user-provided install
/// against, and it isn't ours to replace.
fn is_connector_managed_forge(forge_path: &str, forge_download_dir: &std::path::Path) -> bool {
    !forge_path.is_empty() && std::path::Path::new(forge_path) == forge_download_dir
}

/// Compare the locally-downloaded MaMo Forge jar against whatever's currently published under
/// the `replay-features-latest` tag. Returns `Ok(Some(asset))` when a different build is available
/// (with everything needed to immediately start downloading it), `Ok(None)` when they already match,
/// or `Err(msg)` if the check failed.
///
/// Compares GitHub's per-asset `updated_at` timestamp, not the filename: `replay-features-latest`
/// is a rolling tag that gets re-published from a new commit under the exact same (fixed
/// `-SNAPSHOT-`) asset name, so a filename comparison can never detect a same-named republish —
/// confirmed as the reason a build containing a real fix (FORGE_REPLAY_BUG.md) sat published for
/// a full day while a stale local jar was never flagged as out of date. An older download with no
/// recorded `updated_at` (predates this fix, or a user-provided Forge path outside our management)
/// is treated as "unknown, assume an update may be available" rather than silently assumed current.
async fn check_forge_update_available() -> Result<Option<crate::download::ForgeAsset>, String> {
    let local_jar = match crate::forge::resolve_latest_forge_jar(&forge_download_dir()) {
        Some(j) => j,
        None => return Ok(None),
    };
    let local_updated_at = crate::download::read_asset_meta_updated_at(&local_jar);

    // The update path only ever re-fetches the standalone JAR (see start_forge_auto_update /
    // download_forge_jar_staged) — compare against that asset, not the portable zip.
    let remote = crate::download::resolve_forge_jar_url()
        .await
        .map_err(|e| e.to_string())?;

    match local_updated_at {
        Some(updated_at) if updated_at == remote.updated_at => Ok(None),
        _ => Ok(Some(remote)),
    }
}

/// Checks for a MaMo Forge update and, if one's available, immediately downloads it to a
/// staging file — no click required. Shared between the 5s-after-startup background check
/// (`LauncherApp::new`) and the Settings tab's "Check now" button. Reports progress through
/// `forge_update_progress` (the same field the download itself reports through) and leaves a
/// finished download in `forge_update_check.staged` for the periodic tick in `update()` to
/// swap into place once Forge is confirmed not running (`finalize_staged_forge_update_if_ready`).
async fn run_forge_update_check_and_download(
    forge_update_check: Arc<Mutex<ForgeUpdateCheckState>>,
    forge_update_progress: Arc<Mutex<Option<DownloadProgress>>>,
    cancelled: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    if let Ok(mut s) = forge_update_check.lock() {
        s.busy = true;
    }
    ctx.request_repaint();

    // Loop to support jumping over versions and re-downloading if a newer build is published
    // while the download is in flight (rare case).
    const MAX_DOWNLOAD_ATTEMPTS: usize = 5;
    let mut attempts = 0;

    while attempts < MAX_DOWNLOAD_ATTEMPTS {
        attempts += 1;

        let check_res = check_forge_update_available().await;
        let asset = match check_res {
            Ok(Some(asset)) => asset,
            Ok(None) => {
                if let Ok(mut s) = forge_update_check.lock() {
                    s.busy = false;
                }
                *forge_update_progress.lock().unwrap() = None;
                log::info!("MaMo Forge is up to date");
                ctx.request_repaint();
                return;
            }
            Err(e) => {
                log::warn!("MaMo Forge update check failed: {e}");
                if let Ok(mut p) = forge_update_progress.lock() {
                    let mut prog = DownloadProgress::default();
                    prog.finished = true;
                    prog.error = Some(format!("Check failed: {e}"));
                    *p = Some(prog);
                }
                if let Ok(mut s) = forge_update_check.lock() {
                    s.busy = false;
                }
                ctx.request_repaint();
                return;
            }
        };

        log::info!("MaMo Forge update available: {} — downloading in background", asset.name);
        cancelled.store(false, Ordering::Relaxed);
        *forge_update_progress.lock().unwrap() = Some(DownloadProgress::default());
        ctx.request_repaint();

        let dest_dir = forge_download_dir();
        let progress_bg = Arc::clone(&forge_update_progress);
        let ctx_progress = ctx.clone();
        let cancelled_clone = Arc::clone(&cancelled);
        let outcome = crate::download::download_forge_jar_staged(
            &dest_dir,
            move |update| {
                if let Ok(mut guard) = progress_bg.lock() {
                    let entry = guard.get_or_insert_with(DownloadProgress::default);
                    entry.bytes_done = update.bytes_done;
                    entry.total_bytes = update.total_bytes;
                    entry.status_text = format_download_status(update.bytes_done, update.total_bytes);
                }
                ctx_progress.request_repaint();
            },
            cancelled_clone,
        )
        .await;

        match outcome {
            Ok((staged_path, downloaded_asset)) => {
                log::info!("MaMo Forge update downloaded: {}", downloaded_asset.name);

                // Check if an even newer version was published while downloading (rare case)
                if let Ok(latest_remote) = crate::download::resolve_forge_jar_url().await {
                    if latest_remote.updated_at != downloaded_asset.updated_at {
                        log::warn!(
                            "A newer MaMo Forge build ({}) was published during download of {}; re-downloading latest build...",
                            latest_remote.name,
                            downloaded_asset.name
                        );
                        let _ = std::fs::remove_file(&staged_path);
                        continue;
                    }
                }

                log::info!("MaMo Forge update downloaded — will install once Forge is closed");
                if let Ok(mut s) = forge_update_check.lock() {
                    s.staged = Some(StagedForgeUpdate { staged_path, asset: downloaded_asset });
                    s.busy = false;
                }
                *forge_update_progress.lock().unwrap() = None;
                break;
            }
            Err(e) if e.to_string().contains("cancelled") => {
                if let Ok(mut s) = forge_update_check.lock() {
                    s.busy = false;
                }
                *forge_update_progress.lock().unwrap() = None;
                break;
            }
            Err(e) => {
                log::error!("MaMo Forge update download failed: {e}");
                if let Ok(mut p) = forge_update_progress.lock() {
                    if let Some(ref mut prog) = *p {
                        prog.finished = true;
                        prog.error = Some(e.to_string());
                    }
                }
                if let Ok(mut s) = forge_update_check.lock() {
                    s.busy = false;
                }
                break;
            }
        }
    }
    ctx.request_repaint();
}

/// Whether a MaMo Forge jar is already sitting in the download directory from a previous run.
fn forge_jar_already_downloaded() -> bool {
    let dir = forge_download_dir();
    dir.exists() && std::fs::read_dir(&dir)
        .map(|mut d| d.any(|e| e.ok().map(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.starts_with("forge-gui-desktop-") && s.ends_with("-jar-with-dependencies.jar")
        }).unwrap_or(false)))
        .unwrap_or(false)
}

/// Represents a requested Forge launch that may need a pre-launch version check
#[derive(Clone, Debug)]
enum PendingForgeLaunch {
    /// Plain launch without deck
    Plain,
    /// Account deck from MaMo (needs to be downloaded & launched)
    AccountDeck(crate::gamelog::UserDeck),
    /// Scenario deck + scenario json
    Scenario {
        deck_id: String,
        scenario_id: String,
        scenario_name: String,
    },
    /// Local deck stem with curated opponent
    LocalDeckWithCuratedOpponent {
        local_stem: String,
    },
    /// Deeplink action
    Deeplink(Deeplink),
}

/// State of the pre-launch Forge update check and prompt
#[derive(Clone)]
enum PreLaunchUpdateState {
    /// Forge is already running: prompt user to confirm whether to start an additional instance
    AlreadyRunningPrompt,
    /// Actively checking remote for an update
    Checking {
        started_at: Instant,
        result_rx: Arc<Mutex<Option<Result<Option<crate::download::ForgeAsset>, String>>>>,
    },
    /// Update is available (either staged locally already or remote asset to download)
    Prompt {
        asset: crate::download::ForgeAsset,
        is_staged: bool,
    },
    /// Update is currently downloading inside the modal
    Downloading {
        asset: crate::download::ForgeAsset,
        progress: Arc<Mutex<Option<DownloadProgress>>>,
        cancelled: Arc<AtomicBool>,
        result: Arc<Mutex<Option<Result<std::path::PathBuf, String>>>>,
    },
    /// Download or install failed
    Failed {
        error: String,
        asset: crate::download::ForgeAsset,
    },
}

/// Dialog state for pre-launch Forge update confirmation
#[derive(Clone)]
struct PreLaunchUpdateDialog {
    pub launch: PendingForgeLaunch,
    pub state: PreLaunchUpdateState,
}

/// State for the Play tab's scenario picker — the saved Starting Hand/Perfect Game scenarios
/// for whichever deck is currently selected in `selected_account_deck`.
#[derive(Clone, Default)]
struct ScenarioPickerState {
    /// Which deck `scenarios` belongs to — lets an in-flight fetch detect that the user has
    /// since selected a different deck and drop its (now stale) result instead of applying it.
    deck_id: Option<String>,
    /// Already filtered to Forge-playable scenarios (see `ScenarioSummary::playable_in_forge`)
    scenarios: Vec<crate::gamelog::ScenarioSummary>,
    is_loading: bool,
    error_message: Option<String>,
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
    /// Is a reparse-failed request currently in flight
    is_retrying_failed: bool,
    /// Status message from the last reparse-failed call
    reparse_status: Option<String>,
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
    /// Path to the Forge scripts directory (run_commander_simulation.ps1 etc.)
    forge_scripts_path_input: String,
    /// MaMo API authentication token
    auth_token_input: String,
    /// Moxfield Bearer token for user deck sync
    moxfield_token_input: String,
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
            
            Ok(Box::new(LauncherApp::new(state.clone(), cc.egui_ctx.clone())))
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
    /// When the launcher PID first exited during this monitoring session.
    /// Used to debounce forge_window_seen: a window that appears for <10 s right after the
    /// launcher exits is treated as the launcher's own UI, not the real Java Forge window.
    forge_launcher_exited_at: Option<Instant>,
    /// Timestamp of last automatic gamelog scan
    last_auto_gamelog_scan: Option<Instant>,
    /// Whether the bottom activity panel is collapsed
    activity_panel_collapsed: bool,
    /// Track entry count to auto-expand on new errors
    last_seen_entry_count: usize,
    /// Account deck (from MaMo, not necessarily downloaded yet) to pre-select when launching Forge
    selected_account_deck: Option<crate::gamelog::UserDeck>,
    /// Saved scenarios for `selected_account_deck`, shown in the Play tab's scenario picker
    scenario_picker: Arc<Mutex<ScenarioPickerState>>,
    /// Local `.dck` file names available in the Forge deck directory
    forge_local_decks: Vec<String>,
    /// Background version check result
    update_check: Arc<Mutex<UpdateCheckState>>,
    /// Live progress while an in-place Connector update download is running
    connector_update_progress: Arc<Mutex<Option<DownloadProgress>>>,
    /// Set by the Connector update banner's Cancel button
    connector_update_cancelled: Arc<AtomicBool>,
    /// Background MaMo Forge update check result
    forge_update_check: Arc<Mutex<ForgeUpdateCheckState>>,
    /// Live progress while an in-place Forge update download is running (Home tab banner)
    forge_update_progress: Arc<Mutex<Option<DownloadProgress>>>,
    /// Set by the banner's Cancel button; read by the update download task
    forge_update_cancelled: Arc<AtomicBool>,
    /// Whether the setup wizard is currently visible
    show_setup_wizard: bool,
    /// State for the setup wizard
    wizard: SetupWizardState,
    /// Set by background threads when a Forge launch fails — checked in update()
    wizard_requested: Arc<AtomicBool>,
    /// Set right after a successful `auth` deeplink so the user's full MaMo deck list loads
    /// automatically — checked in update()
    decks_fetch_requested: Arc<AtomicBool>,
    /// Set after an account-deck download+launch completes, so the locally-known `.dck` list
    /// picks up the newly downloaded file — checked in update()
    forge_local_decks_refresh_requested: Arc<AtomicBool>,
    /// True while an account deck selected in the Home tab picker is being downloaded and launched
    is_launching_selected_deck: Arc<Mutex<bool>>,
    /// Which destructive action is pending confirmation (None = no dialog showing)
    confirm_action: Option<ConfirmAction>,
    /// Current phase of the play session — see `PlaySession` doc comment
    play_session: Arc<Mutex<PlaySession>>,
    /// Coordinates the single gamelog-scan slot shared by the auto-scan and manual-scan paths —
    /// see `ScanSlot` doc comment
    scan_slot: ScanSlot,
    /// Pending pre-launch Forge update check or prompt dialog (None = no dialog active)
    prelaunch_update_dialog: Option<PreLaunchUpdateDialog>,
}

impl LauncherApp {
    fn new(state: AppState, ctx: egui::Context) -> Self {
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
            forge_scripts_path_input: settings.forge_scripts_path.clone().unwrap_or_default(),
            auth_token_input: settings.auth_token.clone().unwrap_or_default(),
            moxfield_token_input: settings.moxfield_auth_token.clone().unwrap_or_default(),
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
                        activity_log.log_success(&forge_result.message);
                    } else if forge_result.success {
                        activity_log.log_success(&forge_result.message);
                    } else {
                        activity_log.log_error(&forge_result.message);
                    }
                }
                CommandResult::ForgeLaunched(forge_result) => {
                    if forge_result.already_running {
                        activity_log.log_success(&forge_result.message);
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
                CommandResult::ReplayGameLaunched(forge_result) => {
                    if forge_result.already_running {
                        activity_log.log_success(&forge_result.message);
                    } else if forge_result.success {
                        activity_log.log_success(&forge_result.message);
                    } else {
                        activity_log.log_error(&forge_result.message);
                    }
                }
                CommandResult::SimulationCompleted(sim_result) => {
                    if sim_result.success {
                        activity_log.log_success(&sim_result.message);
                    } else {
                        activity_log.log_error(&sim_result.message);
                    }
                }
                CommandResult::ScenarioSynced(results) => {
                    activity_log.log_success(format!("Synchronized {} scenario(s) to MaMo", results.len()));
                }
            }
        }

        // Switch to Play tab (activity panel will auto-expand for deeplink progress)
        let initial_tab = Tab::Play;

        // Wizard: show on first run or when Forge is not configured
        let forge_not_configured = settings.forge_path.is_none();
        let wizard_requested = Arc::new(AtomicBool::new(false));
        let decks_fetch_requested = Arc::new(AtomicBool::new(false));
        let forge_local_decks_refresh_requested = Arc::new(AtomicBool::new(false));
        let wizard = SetupWizardState {
            step: WizardStep::Welcome,
            forge_path_input: settings.forge_path.clone().unwrap_or_default(),
            forge_path_valid: settings.forge_path.as_ref().map(|p| validate_forge_path(p)).unwrap_or(false),
            test_status: None,
            pending_test_result: None,
            download_progress: None,
            download_result: None,
            download_cancelled: None,
            java_status: None,
        };

        // Clean up any stale .old backup binaries or unfinished .staged files
        crate::download::cleanup_old_connector_backups();

        // Kick off background update check — doesn't block startup
        let update_check = Arc::new(Mutex::new(UpdateCheckState::default()));
        let connector_update_progress: Arc<Mutex<Option<DownloadProgress>>> = Arc::new(Mutex::new(None));
        let connector_update_cancelled = Arc::new(AtomicBool::new(false));
        {
            let update_check_bg = Arc::clone(&update_check);
            let ctx_bg = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(async {
                    if let Ok(asset) = crate::download::resolve_connector_release_asset().await {
                        if is_newer_version(&asset.version, env!("CARGO_PKG_VERSION")) {
                            if let Ok(mut s) = update_check_bg.lock() {
                                s.available_version = Some(asset.version.clone());
                                s.asset = Some(asset);
                            }
                            ctx_bg.request_repaint();
                        }
                    }
                });
            });
        }

        // Kick off background MaMo Forge update check — only meaningful when the configured
        // Forge install is the Connector's own managed download, not a user-provided one.
        // Finding an update immediately starts downloading it too (see
        // run_forge_update_check_and_download) — no click required; a click is only ever
        // needed to dismiss an error or trigger an out-of-schedule "Check now".
        let forge_update_check = Arc::new(Mutex::new(ForgeUpdateCheckState::default()));
        let forge_update_progress: Arc<Mutex<Option<DownloadProgress>>> = Arc::new(Mutex::new(None));
        let forge_update_cancelled = Arc::new(AtomicBool::new(false));
        if is_connector_managed_forge(settings.forge_path.as_deref().unwrap_or(""), &forge_download_dir()) {
            let forge_update_check_bg = Arc::clone(&forge_update_check);
            let forge_update_progress_bg = Arc::clone(&forge_update_progress);
            let forge_update_cancelled_bg = Arc::clone(&forge_update_cancelled);
            let ctx_bg = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let runtime = tokio::runtime::Runtime::new().unwrap();
                runtime.block_on(run_forge_update_check_and_download(
                    forge_update_check_bg,
                    forge_update_progress_bg,
                    forge_update_cancelled_bg,
                    ctx_bg,
                ));
            });
        }

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
            forge_launcher_exited_at: None,
            last_auto_gamelog_scan: None,
            activity_panel_collapsed: !started_with_deeplink,
            last_seen_entry_count: 0,
            selected_account_deck: None,
            scenario_picker: Arc::new(Mutex::new(ScenarioPickerState::default())),
            forge_local_decks: Vec::new(),
            update_check,
            connector_update_progress,
            connector_update_cancelled,
            forge_update_check,
            forge_update_progress,
            forge_update_cancelled,
            show_setup_wizard: forge_not_configured,
            wizard,
            wizard_requested,
            decks_fetch_requested,
            forge_local_decks_refresh_requested,
            is_launching_selected_deck: Arc::new(Mutex::new(false)),
            confirm_action: None,
            play_session: Arc::new(Mutex::new(PlaySession::default())),
            scan_slot: ScanSlot::default(),
            prelaunch_update_dialog: None,
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process initial deeplink on first frame (with progress logging) - only after setup wizard is completed/closed
        if !self.show_setup_wizard {
            if let Some(deeplink) = self.pending_initial_deeplink.take() {
                self.process_deeplink_with_progress(deeplink, ctx);
            }
        }

        // Pick up wizard requests from background threads (e.g. forge launch failure)
        if self.wizard_requested.load(Ordering::Relaxed) {
            self.wizard_requested.store(false, Ordering::Relaxed);
            self.show_setup_wizard = true;
            self.wizard.step = WizardStep::ConfigureForge;
            // Clear any stale download state so DownloadForge doesn't show leftover errors
            self.wizard.download_progress = None;
            self.wizard.download_result = None;
            self.wizard.download_cancelled = None;
        }

        // Auto-load the user's full MaMo deck list right after a successful auth deeplink,
        // so the Home tab picker is ready without a manual "Load my decks" trip to the Decks tab
        if self.decks_fetch_requested.load(Ordering::Relaxed) {
            self.decks_fetch_requested.store(false, Ordering::Relaxed);
            self.fetch_my_mamo_decks(ctx);
        }

        // Pick up the local `.dck` list refresh after an account-deck download+launch
        if self.forge_local_decks_refresh_requested.load(Ordering::Relaxed) {
            self.forge_local_decks_refresh_requested.store(false, Ordering::Relaxed);
            self.forge_local_decks = list_forge_decks();
        }

        // Poll wizard test-launch result from background thread
        let wizard_test_done = if let Some(ref chan) = self.wizard.pending_test_result {
            chan.try_lock().ok().and_then(|mut g| g.take())
        } else {
            None
        };
        if let Some(result) = wizard_test_done {
            self.wizard.test_status = Some(result);
            self.wizard.pending_test_result = None;
        }

        // Poll Forge download result from background thread
        let download_done = if let Some(ref arc) = self.wizard.download_result {
            arc.try_lock().ok().and_then(|mut g| g.take())
        } else {
            None
        };
        if let Some(result) = download_done {
            self.wizard.download_result = None;
            match result {
                DownloadResult::Success { ref jar_dir } => {
                    let dir = jar_dir.clone();
                    self.wizard.forge_path_input = dir.clone();
                    self.wizard.forge_path_valid = validate_forge_path(&dir);
                    // Mark progress as finished cleanly
                    if let Some(ref p) = self.wizard.download_progress {
                        if let Ok(mut g) = p.lock() { g.finished = true; }
                    }
                    self.wizard.step = WizardStep::ConfigureForge;
                }
                DownloadResult::Failed(msg) => {
                    if let Some(ref p) = self.wizard.download_progress {
                        if let Ok(mut g) = p.lock() {
                            g.finished = true;
                            g.error = Some(msg);
                        }
                    }
                }
                DownloadResult::Cancelled => {
                    self.wizard.download_progress = None;
                    self.wizard.download_cancelled = None;
                }
            }
        }

        // Poll pre-launch Forge update check / download state
        let mut prelaunch_action: Option<PendingForgeLaunch> = None;
        if let Some(ref mut dialog) = self.prelaunch_update_dialog {
            match &mut dialog.state {
                PreLaunchUpdateState::Checking { started_at, result_rx } => {
                    let res = result_rx.lock().unwrap().take();
                    if let Some(res) = res {
                        match res {
                            Ok(Some(asset)) => {
                                dialog.state = PreLaunchUpdateState::Prompt {
                                    asset,
                                    is_staged: false,
                                };
                            }
                            Ok(None) => {
                                // Already up to date! Proceed to launch directly
                                prelaunch_action = Some(dialog.launch.clone());
                            }
                            Err(e) => {
                                log::info!("Pre-launch Forge update check bypassed ({e}) — launching Forge");
                                prelaunch_action = Some(dialog.launch.clone());
                            }
                        }
                    } else if started_at.elapsed().as_secs() > 4 {
                        log::info!("Pre-launch Forge update check timed out — launching Forge");
                        prelaunch_action = Some(dialog.launch.clone());
                    }
                }
                PreLaunchUpdateState::Downloading { result, asset, .. } => {
                    let res = result.lock().unwrap().take();
                    if let Some(res) = res {
                        match res {
                            Ok(staged_path) => {
                                let forge_dir = forge_download_dir();
                                match crate::download::finalize_staged_forge_jar(&forge_dir, &staged_path, asset) {
                                    Ok(_) => {
                                        if let Ok(mut log) = self.activity_log.lock() {
                                            log.log_success("MaMo Forge updated to latest version.");
                                        }
                                    }
                                    Err(e) => {
                                        log::error!("Failed to finalize updated Forge jar: {e}");
                                    }
                                }
                                prelaunch_action = Some(dialog.launch.clone());
                            }
                            Err(e) if e.contains("cancelled") => {
                                // Cancelled
                                self.prelaunch_update_dialog = None;
                                *self.play_session.lock().unwrap() = PlaySession::Watching;
                            }
                            Err(e) => {
                                dialog.state = PreLaunchUpdateState::Failed {
                                    error: e,
                                    asset: asset.clone(),
                                };
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(action) = prelaunch_action {
            self.prelaunch_update_dialog = None;
            self.execute_pending_forge_launch(action, ctx);
        }
        
        // Check for pending commands from secondary instances every 500ms - only after setup wizard is completed/closed
        let now = Instant::now();
        if !self.show_setup_wizard && now.duration_since(self.last_pending_check).as_millis() > 500 {
            self.last_pending_check = now;
            self.check_pending_commands(ctx);
            self.finalize_staged_forge_update_if_ready();
        }

        // Auto gamelog scanning after deeplink Forge launch
        // Two-phase detection: first track launcher PID, then switch to window-based
        // detection since forge.exe is a launcher that spawns java.exe and exits.
        let monitoring_since = *self.forge_monitoring_since.lock().unwrap();
        if let Some(start_time) = monitoring_since {
            // Guard against stale fields left by a previous session (e.g. when monitoring
            // was started by a spawned thread that cannot reset self fields directly).
            // forge_launcher_exited_at always predates start_time if it belongs to the
            // previous session, so we reset both fields when that is detected.
            if let Some(exited_at) = self.forge_launcher_exited_at {
                if exited_at < start_time {
                    self.forge_launcher_exited_at = None;
                    self.forge_window_seen = false;
                }
            }

            let forge_pid_value = *self.forge_pid.lock().unwrap();
            let pid_alive = forge_pid_value.map(|p| crate::forge::is_process_running(p)).unwrap_or(false);
            let window_open = crate::forge::is_forge_window_open();
            let forge_alive = pid_alive || window_open;
            let is_scanning = self.gamelog_state.lock().unwrap().is_scanning;
            let elapsed = now.duration_since(start_time);
            
            // Clear launcher PID once it exits (launcher is just a wrapper)
            if !pid_alive && forge_pid_value.is_some() {
                *self.forge_pid.lock().unwrap() = None;
                // Record the moment the launcher PID died so we can debounce the
                // forge_window_seen flag below.
                if self.forge_launcher_exited_at.is_none() {
                    self.forge_launcher_exited_at = Some(now);
                }
            }

            // Track if we've ever seen the REAL Forge game window.
            // The launcher (forge.exe) and any separate "Game Launcher" app may also
            // have a window titled "Forge …" that closes shortly after the launcher PID
            // exits. Only count a window as the real Forge game window if it is still
            // visible at least 10 s after the launcher PID exited — transient launcher
            // UIs disappear within 1-3 s, while the real Java Forge session stays open
            // for the entire game.
            if !self.forge_window_seen && window_open && !pid_alive {
                let secs_since_pid_exit = self.forge_launcher_exited_at
                    .map(|t| now.duration_since(t).as_secs())
                    .unwrap_or(0);
                if secs_since_pid_exit >= 10 {
                    self.forge_window_seen = true;
                }
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
                        *self.play_session.lock().unwrap() = PlaySession::Scanning;
                        self.start_auto_gamelog_scan(ctx, true);
                    }
                    *self.forge_monitoring_since.lock().unwrap() = None;
                    self.forge_window_seen = false;
                    self.forge_launcher_exited_at = None;
                    self.last_auto_gamelog_scan = None;
                }
            } else if !is_scanning {
                // Forge is running - handle periodic scans.
                // Only scan if the user has connected their MaMo account; otherwise
                // there's nothing to upload to, so don't spam the log with failures.
                let has_token = {
                    let s = self.settings.lock().unwrap();
                    s.auth_token.is_some() || s.gamelog_config.auth_token.is_some()
                };
                let should_scan = match self.last_auto_gamelog_scan {
                    None => {
                        self.last_auto_gamelog_scan = Some(now);
                        if let Ok(mut log) = self.activity_log.lock() {
                            if has_token {
                                log.log_info("\u{1F3AE} Forge running - auto gamelog scanning active (every 5 min)");
                            } else {
                                log.log_info("\u{1F3AE} Forge running - connect your MaMo account in Settings to auto-upload game logs");
                            }
                        }
                        false
                    }
                    Some(last) => now.duration_since(last).as_secs() >= 300,
                };

                if should_scan && has_token {
                    if let Ok(mut log) = self.activity_log.lock() {
                        log.log_info("\u{1F504} Auto gamelog scan (periodic 5 min)");
                    }
                    self.start_auto_gamelog_scan(ctx, false);
                    self.last_auto_gamelog_scan = Some(now);
                } else if should_scan {
                    // No token — defer silently so we don't re-check every frame.
                    self.last_auto_gamelog_scan = Some(now);
                }
            }
        }
        
        // Request a repaint in 500ms to keep checking for pending commands
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        
        // Auto-expand activity panel when new errors arrive
        {
            if let Ok(log) = self.activity_log.lock() {
                let current_count = log.entries.len();
                if current_count > self.last_seen_entry_count {
                    // Check if any new entry is an error
                    let new_entries = current_count - self.last_seen_entry_count;
                    let has_new_error = log.entries.iter().take(new_entries).any(|e| e.is_error);
                    if has_new_error {
                        self.activity_panel_collapsed = false;
                    }
                    self.last_seen_entry_count = current_count;
                }
            }
        }
        
        // Confirm dialog for destructive actions (rendered as a floating window)
        if self.confirm_action.is_some() {
            self.render_confirm_dialog(ctx);
        }

        // Pre-launch Forge update check / prompt dialog
        if self.prelaunch_update_dialog.is_some() {
            self.render_prelaunch_update_dialog(ctx);
        }

        // Bottom panel: Activity Log (rendered BEFORE CentralPanel per egui rules)
        self.render_activity_panel(ctx);
        
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::WHITE))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::BLACK);
                ui.visuals_mut().panel_fill = egui::Color32::WHITE;

                // Connector update banner (render even during setup wizard)
                let (update_ver, staged_path, is_downloading, update_err, already_dismissed) = {
                    let s = self.update_check.lock().unwrap();
                    (
                        s.available_version.clone(),
                        s.staged_path.clone(),
                        s.is_downloading,
                        s.error.clone(),
                        s.dismissed,
                    )
                };
                if !already_dismissed {
                    if let Some(ref staged) = staged_path {
                        let ver = update_ver.as_deref().unwrap_or("new");
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(212, 237, 218))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("✨ Update v{ver} ready"))
                                            .color(egui::Color32::from_rgb(21, 87, 36))
                                            .small()
                                            .strong(),
                                    );
                                    if ui.small_button("Restart Now").clicked() {
                                        if let Err(e) = crate::download::apply_connector_update_and_restart(staged) {
                                            log::error!("Failed to restart and apply update: {e}");
                                            if let Ok(mut s) = self.update_check.lock() {
                                                s.error = Some(format!("Restart failed: {e}"));
                                            }
                                        }
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("✕").clicked() {
                                            self.update_check.lock().unwrap().dismissed = true;
                                        }
                                    });
                                });
                            });
                        ui.add_space(2.0);
                    } else if is_downloading {
                        let ver = update_ver.as_deref().unwrap_or("");
                        let status_text = self
                            .connector_update_progress
                            .lock()
                            .unwrap()
                            .as_ref()
                            .map(|p| p.status_text.clone())
                            .unwrap_or_else(|| "Downloading update…".to_string());
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(226, 227, 229))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(
                                        egui::RichText::new(format!("⏳ v{ver}: {status_text}"))
                                            .color(egui::Color32::BLACK)
                                            .small(),
                                    );
                                    if ui.small_button("Cancel").clicked() {
                                        self.connector_update_cancelled.store(true, Ordering::SeqCst);
                                    }
                                });
                            });
                        ui.add_space(2.0);
                    } else if let Some(ref err) = update_err {
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(248, 215, 218))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("⚠ Update error: {err}"))
                                            .color(egui::Color32::from_rgb(114, 28, 36))
                                            .small(),
                                    );
                                    if ui.small_button("Retry").clicked() {
                                        self.trigger_connector_update_download(ctx);
                                    }
                                    if ui.small_button("Browser Download").clicked() {
                                        let _ = std::process::Command::new("cmd")
                                            .args(["/c", "start", "https://github.com/killriam/mamo-Connector/releases/latest"])
                                            .spawn();
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("✕").clicked() {
                                            self.update_check.lock().unwrap().dismissed = true;
                                        }
                                    });
                                });
                            });
                        ui.add_space(2.0);
                    } else if let Some(ref ver) = update_ver {
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(255, 243, 205))
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("⬆ Update available: v{ver}"))
                                            .color(egui::Color32::from_rgb(133, 100, 4))
                                            .small(),
                                    );
                                    if ui.small_button("Download & Install").clicked() {
                                        self.trigger_connector_update_download(ctx);
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.small_button("✕").clicked() {
                                            self.update_check.lock().unwrap().dismissed = true;
                                        }
                                    });
                                });
                            });
                        ui.add_space(2.0);
                    }
                }

                if self.show_setup_wizard {
                    self.render_setup_wizard(ui, ctx);
                    return;
                }

                // Title with version info
                ui.horizontal(|ui| {
                    ui.heading("Mamo Connector");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.small(egui::RichText::new(format!("v{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")))
                            .color(egui::Color32::GRAY));
                    });
                });
                ui.separator();

                // MaMo Forge update banner — only ever populated for Connector-managed installs.
                // Fully automatic: an update starts downloading the moment it's detected, and
                // installs itself the moment Forge is confirmed closed (finalize_staged_forge_
                // update_if_ready). This banner is purely informational — its only interactive
                // controls are Cancel (mid-download) and dismiss (error only).
                let (forge_busy, forge_staged, forge_update_dismissed) = {
                    let s = self.forge_update_check.lock().unwrap();
                    (s.busy, s.staged.is_some(), s.dismissed)
                };
                let forge_update_progress = self.forge_update_progress.lock().unwrap().clone();
                if !forge_update_dismissed
                    && (forge_busy || forge_staged || forge_update_progress.is_some())
                {
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgb(205, 232, 255))
                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if let Some(ref prog) = forge_update_progress {
                                    if let Some(ref err) = prog.error {
                                        ui.label(
                                            egui::RichText::new(format!("✗ MaMo Forge update failed: {err}"))
                                                .color(egui::Color32::from_rgb(176, 0, 32))
                                                .small(),
                                        );
                                    } else {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "⬆ Downloading MaMo Forge update… {}",
                                                format_download_status(prog.bytes_done, prog.total_bytes)
                                            ))
                                            .color(egui::Color32::from_rgb(0, 90, 158))
                                            .small(),
                                        );
                                        if !prog.finished && ui.small_button("Cancel").clicked() {
                                            self.forge_update_cancelled.store(true, Ordering::Relaxed);
                                        }
                                    }
                                } else if forge_staged {
                                    ui.label(
                                        egui::RichText::new("⬆ MaMo Forge update ready — installs automatically once Forge is closed")
                                            .color(egui::Color32::from_rgb(0, 90, 158))
                                            .small(),
                                    );
                                } else if forge_busy {
                                    ui.label(
                                        egui::RichText::new("🔄 Checking for a MaMo Forge update…")
                                            .color(egui::Color32::GRAY)
                                            .small(),
                                    );
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if forge_update_progress.as_ref().map(|p| p.error.is_some()).unwrap_or(false)
                                        && ui.small_button("✕").clicked()
                                    {
                                        self.forge_update_check.lock().unwrap().dismissed = true;
                                    }
                                });
                            });
                        });
                    ui.add_space(2.0);
                }

                // Tab bar — 4 tabs, one per core journey: Play (start decks, launch Forge, watch
                // an active session), Get Decks (pull someone else's list in), Setup (MaMo
                // account + Forge + Connector updates), Settings (the rarer technical knobs).
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.current_tab == Tab::Play, "▶ Play").clicked() {
                        self.current_tab = Tab::Play;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Decks, "📥 Get Decks").clicked() {
                        self.current_tab = Tab::Decks;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Setup, "🔧 Setup").clicked() {
                        self.current_tab = Tab::Setup;
                    }
                    if ui.selectable_label(self.current_tab == Tab::Settings, "⚙ Settings").clicked() {
                        self.current_tab = Tab::Settings;
                    }
                });
                ui.separator();

                // Persistent status strip — visible on every tab, not just Play, and never
                // blank, so it's always clear Connector needs to stay open to keep syncing.
                {
                    let ps = self.play_session.lock().unwrap().clone();
                    let (text, is_active) = play_session_strip(&ps);
                    let dot_color = if is_active {
                        egui::Color32::from_rgb(76, 92, 196)
                    } else {
                        egui::Color32::from_rgb(23, 145, 90)
                    };
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgb(238, 236, 247))
                        .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                ui.painter().circle_filled(rect.center(), 4.0, dot_color);
                                ui.label(egui::RichText::new(text).small().color(egui::Color32::from_rgb(70, 65, 105)));
                            });
                        });
                }
                ui.add_space(4.0);
                ui.separator();

                // Tab content
                match self.current_tab {
                    Tab::Play => self.render_play_tab(ui, ctx),
                    Tab::Decks => self.render_decks_tab(ui, ctx),
                    Tab::Setup => self.render_setup_tab(ui, ctx),
                    Tab::Settings => self.render_settings_tab(ui, ctx),
                }
            });
    }
}

impl LauncherApp {
    /// Process a deeplink with real-time progress logging to the Activity tab
    fn process_deeplink_with_progress(&mut self, deeplink: Deeplink, ctx: &egui::Context) {
        use log::info;
        
        info!("Processing deeplink with progress: {}", deeplink.raw);
        
        // Expand activity panel to show progress (visible on all tabs)
        self.activity_panel_collapsed = false;
        
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

        // An evaluation launch (playtest/launch-forge/simulate) with no deck id means the
        // frontend didn't pin a deck — rather than silently starting Forge deck-less, send the
        // user to the Home tab picker (backed by their full MaMo account deck list) instead.
        if is_deckless_evaluation_action(&deeplink.action, deeplink_has_deck_reference(&deeplink)) {
            if let Ok(mut log) = self.activity_log.lock() {
                log.log_info("No deck specified — pick one below to launch Forge.");
            }
            self.current_tab = Tab::Play;
            self.decks_fetch_requested.store(true, Ordering::Relaxed);
            ctx.request_repaint();
            return;
        }

        if deeplink_starts_play_session(&deeplink.action) {
            self.request_forge_launch(PendingForgeLaunch::Deeplink(deeplink), ctx);
        } else {
            self.process_deeplink_with_progress_direct(deeplink, ctx);
        }
    }

    /// Directly execute a deeplink without checking for Forge updates (e.g. after update prompt is resolved)
    fn process_deeplink_with_progress_direct(&mut self, deeplink: Deeplink, ctx: &egui::Context) {
        use crate::commands::{self, SharedLogCollector};

        if deeplink_starts_play_session(&deeplink.action) {
            *self.play_session.lock().unwrap() = PlaySession::Launching;
        }

        // Handle the command in a background thread
        let settings = self.settings.clone();
        let settings_state = self.settings_state.clone();
        let activity_log = self.activity_log.clone();
        let activity_log_for_polling = self.activity_log.clone();
        let forge_pid = self.forge_pid.clone();
        let forge_monitoring_since = self.forge_monitoring_since.clone();
        let play_session = Arc::clone(&self.play_session);
        let ctx_clone = ctx.clone();
        let ctx_for_polling = ctx.clone();
        let wizard_requested = Arc::clone(&self.wizard_requested);
        let decks_fetch_requested = Arc::clone(&self.decks_fetch_requested);

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
                            log.log_success(&forge_result.message);
                        } else if forge_result.success {
                            log.log_success(&forge_result.message);
                        } else {
                            log.log_error(&forge_result.message);
                            wizard_requested.store(true, Ordering::Relaxed);
                        }
                    }
                    commands::CommandResult::ForgeLaunched(forge_result) => {
                        if forge_result.already_running {
                            log.log_success(&forge_result.message);
                        } else if forge_result.success {
                            log.log_success(&forge_result.message);
                        } else {
                            log.log_error(&forge_result.message);
                            wizard_requested.store(true, Ordering::Relaxed);
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
                    commands::CommandResult::ReplayGameLaunched(forge_result) => {
                        if forge_result.already_running {
                            log.log_success(&forge_result.message);
                        } else if forge_result.success {
                            log.log_success(&forge_result.message);
                        } else {
                            log.log_error(&forge_result.message);
                            wizard_requested.store(true, Ordering::Relaxed);
                        }
                    }
                    commands::CommandResult::SimulationCompleted(sim_result) => {
                        if sim_result.success {
                            log.log_success(&sim_result.message);
                        } else {
                            log.log_error(&sim_result.message);
                        }
                    }
                    commands::CommandResult::ScenarioSynced(results) => {
                        log.log_success(format!("Synchronized {} scenario(s) to MaMo", results.len()));
                    }
                }
            }

            // Track Forge PID for auto gamelog scanning, and move the play session forward
            match &result {
                commands::CommandResult::DeckCreatedAndLaunched(_, forge_result) if forge_result.success => {
                    if let Some(pid) = forge_result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                    *play_session.lock().unwrap() = if forge_result.already_running { PlaySession::Watching } else { PlaySession::Playing };
                }
                commands::CommandResult::ForgeLaunched(forge_result) if forge_result.success => {
                    if let Some(pid) = forge_result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                    *play_session.lock().unwrap() = if forge_result.already_running { PlaySession::Watching } else { PlaySession::Playing };
                }
                commands::CommandResult::ReplayGameLaunched(forge_result) if forge_result.success => {
                    if let Some(pid) = forge_result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                    *play_session.lock().unwrap() = if forge_result.already_running { PlaySession::Watching } else { PlaySession::Playing };
                }
                _ => {
                    *play_session.lock().unwrap() = PlaySession::Watching;
                }
            }

            // Handle auth token saved result
            if let commands::CommandResult::AuthTokenSaved(ref token) = result {
                log::info!("Auth token saved via deeplink: {}",
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

                // Load the user's full MaMo deck list automatically now that we're connected
                decks_fetch_requested.store(true, Ordering::Relaxed);
            }

            ctx_clone.request_repaint();
        });
    }
    
    /// Check if secondary instances sent a command via pending_command.txt
    fn check_pending_commands(&mut self, ctx: &egui::Context) {
        let pending_path = crate::get_pending_command_path();
        if pending_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&pending_path) {
                let content = content.trim();
                if !content.is_empty() {
                    if let Some(deeplink) = crate::deeplink::parse_deeplink(&[content.to_string()], "mamoConnector://") {
                        self.process_deeplink_with_progress(deeplink, ctx);
                    }
                    let _ = std::fs::remove_file(&pending_path);
                }
            }
        }
    }

    // ==================== Activity Bottom Panel ====================

    fn render_activity_panel(&mut self, ctx: &egui::Context) {
        let panel_id = egui::Id::new("activity_panel");

        if self.activity_panel_collapsed {
            // Collapsed: single-line status bar
            egui::TopBottomPanel::bottom(panel_id)
                .resizable(false)
                .min_height(28.0)
                .max_height(28.0)
                .frame(egui::Frame::default()
                    .fill(egui::Color32::from_rgb(245, 245, 250))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 220, 230))))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("▴ Show Activity").clicked() {
                            self.activity_panel_collapsed = false;
                        }
                        // Show latest entry inline
                        if let Ok(log) = self.activity_log.lock() {
                            if let Some(entry) = log.entries.first() {
                                let (text_color, prefix) = if entry.is_error {
                                    (egui::Color32::from_rgb(176, 0, 32), "[ERR]")
                                } else if entry.is_success {
                                    (egui::Color32::from_rgb(0, 128, 0), "[OK]")
                                } else {
                                    (egui::Color32::GRAY, "[INFO]")
                                };
                                
                                ui.label(egui::RichText::new(format!("{} {} {}", entry.timestamp, prefix, entry.message))
                                    .small().color(text_color));
                            }
                        }
                    });
                });
        } else {
            // Expanded: scrollable log area
            egui::TopBottomPanel::bottom(panel_id)
                .resizable(true)
                .min_height(60.0)
                .max_height(250.0)
                .default_height(150.0)
                .frame(egui::Frame::default()
                    .fill(egui::Color32::from_rgb(245, 245, 250))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 220, 230))))
                .show(ctx, |ui| {
                    // Header row
                    ui.horizontal(|ui| {
                        if ui.small_button("▾ Hide Activity").clicked() {
                            self.activity_panel_collapsed = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Clear").clicked() {
                                if let Ok(mut log) = self.activity_log.lock() {
                                    log.clear();
                                    self.last_seen_entry_count = 0;
                                }
                            }
                        });
                    });

                    // Scrollable log entries
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(false)
                        .show(ui, |ui| {
                            if let Ok(log) = self.activity_log.lock() {
                                if log.entries.is_empty() {
                                    ui.label(egui::RichText::new("No activity yet").italics().color(egui::Color32::GRAY).small());
                                } else {
                                    for entry in &log.entries {
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(&entry.timestamp)
                                                .monospace().small()
                                                .color(egui::Color32::GRAY));

                                            let (color, prefix) = if entry.is_error {
                                                (egui::Color32::from_rgb(176, 0, 32), "[ERR] ")
                                            } else if entry.is_success {
                                                (egui::Color32::from_rgb(0, 128, 0), "[OK] ")
                                            } else {
                                                (egui::Color32::BLACK, "[INFO] ")
                                            };

                                            ui.label(egui::RichText::new(format!("{}{}", prefix, &entry.message))
                                                .small().color(color));
                                        });
                                    }
                                }
                            }
                        });
                });
        }
    }

    // ==================== Pre-Launch Forge Update Check & Prompt ====================

    /// Request a Forge launch, checking if Forge is already open or for newer versions if Connector-managed.
    fn request_forge_launch(&mut self, launch: PendingForgeLaunch, ctx: &egui::Context) {
        // If Forge is already open — or is currently starting up (monitoring active but window
        // not yet visible because the JVM hasn't finished booting) — prompt before spawning
        // a second instance. is_forge_window_open() alone isn't enough: the Java window can
        // take several seconds to appear, so during that gap the check returns false even
        // though Forge is already on its way up.
        let forge_starting = self.forge_monitoring_since.lock().unwrap().is_some();
        if forge_starting || crate::forge::is_forge_window_open() {
            self.prelaunch_update_dialog = Some(PreLaunchUpdateDialog {
                launch,
                state: PreLaunchUpdateState::AlreadyRunningPrompt,
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
            return;
        }

        // Only Connector-managed installs can be auto-checked & updated
        let forge_path = {
            let s = self.settings.lock().unwrap();
            s.forge_path.clone().unwrap_or_default()
        };
        if !is_connector_managed_forge(&forge_path, &forge_download_dir()) {
            self.execute_pending_forge_launch(launch, ctx);
            return;
        }

        // Check if an update is already staged and ready to install
        let staged = self.forge_update_check.lock().unwrap().staged.clone();
        if let Some(staged) = staged {
            self.prelaunch_update_dialog = Some(PreLaunchUpdateDialog {
                launch,
                state: PreLaunchUpdateState::Prompt {
                    asset: staged.asset,
                    is_staged: true,
                },
            });
            ctx.request_repaint();
            return;
        }

        // Start background check with result_rx
        let result_rx = Arc::new(Mutex::new(None));
        let result_rx_bg = Arc::clone(&result_rx);
        let ctx_bg = ctx.clone();

        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let res = runtime.block_on(async {
                // Cap update check to 4 seconds so a slow network/GitHub doesn't hang launch
                tokio::select! {
                    res = check_forge_update_available() => res,
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(4)) => {
                        Err("Update check timed out".to_string())
                    }
                }
            });
            *result_rx_bg.lock().unwrap() = Some(res);
            ctx_bg.request_repaint();
        });

        self.prelaunch_update_dialog = Some(PreLaunchUpdateDialog {
            launch,
            state: PreLaunchUpdateState::Checking {
                started_at: Instant::now(),
                result_rx,
            },
        });
        ctx.request_repaint();
    }

    /// Execute the requested Forge launch directly
    fn execute_pending_forge_launch(&mut self, launch: PendingForgeLaunch, ctx: &egui::Context) {
        match launch {
            PendingForgeLaunch::Plain => {
                let result = launch_forge_from_settings(None, None);
                self.apply_forge_launch_result(result);
            }
            PendingForgeLaunch::AccountDeck(deck) => {
                self.launch_account_deck_async(deck, ctx);
            }
            PendingForgeLaunch::Scenario { deck_id, scenario_id, scenario_name } => {
                self.launch_scenario_async(deck_id, scenario_id, scenario_name, ctx);
            }
            PendingForgeLaunch::LocalDeckWithCuratedOpponent { local_stem } => {
                self.launch_local_deck_with_curated_opponent_async(local_stem, ctx);
            }
            PendingForgeLaunch::Deeplink(deeplink) => {
                self.process_deeplink_with_progress_direct(deeplink, ctx);
            }
        }
    }

    /// Render pre-launch Forge update confirmation modal
    fn render_prelaunch_update_dialog(&mut self, ctx: &egui::Context) {
        let Some(ref mut dialog) = self.prelaunch_update_dialog else { return; };
        
        let title = match &dialog.state {
            PreLaunchUpdateState::AlreadyRunningPrompt => "🎮 Forge is Already Open",
            PreLaunchUpdateState::Checking { .. } => "Checking for Updates…",
            PreLaunchUpdateState::Prompt { is_staged: true, .. } => "✨ MaMo Forge Update Ready",
            PreLaunchUpdateState::Prompt { is_staged: false, .. } => "⬆ MaMo Forge Update Available",
            PreLaunchUpdateState::Downloading { .. } => "⏳ Updating MaMo Forge…",
            PreLaunchUpdateState::Failed { .. } => "⚠ Update Failed",
        };

        let mut action_launch_anyway = false;
        let mut action_cancel = false;
        let mut action_start_download = false;
        let mut action_apply_staged = false;

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                match &dialog.state {
                    PreLaunchUpdateState::AlreadyRunningPrompt => {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("🎮").size(28.0));
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new("Forge is already running").strong());
                                match &dialog.launch {
                                    PendingForgeLaunch::AccountDeck(deck) => {
                                        ui.label(
                                            egui::RichText::new(format!("Deck: {}", deck.deck_name))
                                                .color(egui::Color32::from_rgb(0, 90, 158))
                                                .small(),
                                        );
                                    }
                                    PendingForgeLaunch::Scenario { scenario_name, .. } => {
                                        ui.label(
                                            egui::RichText::new(format!("Scenario: {}", scenario_name))
                                                .color(egui::Color32::from_rgb(0, 90, 158))
                                                .small(),
                                        );
                                    }
                                    PendingForgeLaunch::LocalDeckWithCuratedOpponent { local_stem } => {
                                        ui.label(
                                            egui::RichText::new(format!("Deck: {}", local_stem))
                                                .color(egui::Color32::from_rgb(0, 90, 158))
                                                .small(),
                                        );
                                    }
                                    _ => {}
                                }
                            });
                        });
                        ui.add_space(12.0);
                        ui.label("A Forge window is already open. Would you like to start a new Forge instance?");
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                action_cancel = true;
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(egui::RichText::new("Start New Forge").color(egui::Color32::WHITE).strong())
                                        .fill(egui::Color32::from_rgb(0, 120, 215)),
                                ).clicked() {
                                    action_launch_anyway = true;
                                }
                            });
                        });
                    }
                    PreLaunchUpdateState::Checking { .. } => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Checking if a newer Forge version is available…").small());
                        });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                action_cancel = true;
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Skip Check & Launch").clicked() {
                                    action_launch_anyway = true;
                                }
                            });
                        });
                    }
                    PreLaunchUpdateState::Prompt { asset, is_staged } => {
                        let is_staged = *is_staged;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(if is_staged { "✨" } else { "⬆" }).size(28.0));
                            ui.vertical(|ui| {
                                if is_staged {
                                    ui.label(egui::RichText::new("A new MaMo Forge build is downloaded and ready to install.").strong());
                                } else {
                                    ui.label(egui::RichText::new("A new MaMo Forge build is available.").strong());
                                }
                                ui.label(
                                    egui::RichText::new(format!("Version: {}", asset.name))
                                        .color(egui::Color32::from_rgb(0, 90, 158))
                                        .small(),
                                );
                                if !asset.updated_at.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!("Released: {}", asset.updated_at))
                                            .color(egui::Color32::GRAY)
                                            .small(),
                                    );
                                }
                            });
                        });
                        ui.add_space(12.0);
                        ui.label("Would you like to update before starting Forge?");
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                action_cancel = true;
                            }
                            if ui.button("Launch Without Updating").clicked() {
                                action_launch_anyway = true;
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let btn_text = if is_staged { "Install & Launch" } else { "Update & Launch" };
                                if ui.add(
                                    egui::Button::new(egui::RichText::new(btn_text).color(egui::Color32::WHITE).strong())
                                        .fill(egui::Color32::from_rgb(0, 120, 215)),
                                ).clicked() {
                                    if is_staged {
                                        action_apply_staged = true;
                                    } else {
                                        action_start_download = true;
                                    }
                                }
                            });
                        });
                    }
                    PreLaunchUpdateState::Downloading { progress, cancelled, .. } => {
                        let prog_guard = progress.lock().unwrap();
                        let (status_text, pct) = match prog_guard.as_ref() {
                            Some(p) => {
                                let pct = if let Some(total) = p.total_bytes {
                                    (p.bytes_done as f32 / total as f32).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                };
                                (p.status_text.clone(), pct)
                            }
                            None => ("Starting download…".to_string(), 0.0),
                        };
                        drop(prog_guard);

                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Downloading Forge update…").strong());
                        });
                        ui.add_space(8.0);
                        ui.add(egui::ProgressBar::new(pct).show_percentage());
                        ui.label(egui::RichText::new(status_text).small().color(egui::Color32::GRAY));
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                cancelled.store(true, Ordering::Relaxed);
                                action_cancel = true;
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Launch Current Version").clicked() {
                                    cancelled.store(true, Ordering::Relaxed);
                                    action_launch_anyway = true;
                                }
                            });
                        });
                    }
                    PreLaunchUpdateState::Failed { error, .. } => {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("✗").color(egui::Color32::from_rgb(176, 0, 32)).size(24.0));
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new("Failed to update MaMo Forge").strong());
                                ui.label(egui::RichText::new(error).small().color(egui::Color32::from_rgb(176, 0, 32)));
                            });
                        });
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                action_cancel = true;
                            }
                            if ui.button("Retry").clicked() {
                                action_start_download = true;
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Launch Without Updating").clicked() {
                                    action_launch_anyway = true;
                                }
                            });
                        });
                    }
                }
            });

        if action_cancel {
            if let Some(ref d) = self.prelaunch_update_dialog {
                if matches!(d.state, PreLaunchUpdateState::AlreadyRunningPrompt) {
                    if let Ok(mut log) = self.activity_log.lock() {
                        log.log_info("Forge launch cancelled — Forge is already open.");
                    }
                }
            }
            self.prelaunch_update_dialog = None;
            *self.play_session.lock().unwrap() = PlaySession::Watching;
            *self.is_launching_selected_deck.lock().unwrap() = false;
        } else if action_launch_anyway {
            let launch = self.prelaunch_update_dialog.take().unwrap().launch;
            self.execute_pending_forge_launch(launch, ctx);
        } else if action_apply_staged {
            let dialog_data = self.prelaunch_update_dialog.take().unwrap();
            if let PreLaunchUpdateState::Prompt { asset, .. } = dialog_data.state {
                let staged_path = self.forge_update_check.lock().unwrap().staged.as_ref().map(|s| s.staged_path.clone());
                if let Some(staged_path) = staged_path {
                    let forge_dir = forge_download_dir();
                    match crate::download::finalize_staged_forge_jar(&forge_dir, &staged_path, &asset) {
                        Ok(_) => {
                            if let Ok(mut log) = self.activity_log.lock() {
                                log.log_success("MaMo Forge updated to latest version.");
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to finalize staged Forge jar: {e}");
                        }
                    }
                }
                self.forge_update_check.lock().unwrap().staged = None;
            }
            self.execute_pending_forge_launch(dialog_data.launch, ctx);
        } else if action_start_download {
            let (asset, launch) = match &self.prelaunch_update_dialog {
                Some(d) => match &d.state {
                    PreLaunchUpdateState::Prompt { asset, .. } | PreLaunchUpdateState::Failed { asset, .. } => (asset.clone(), d.launch.clone()),
                    _ => return,
                },
                None => return,
            };

            let progress: Arc<Mutex<Option<DownloadProgress>>> = Arc::new(Mutex::new(Some(DownloadProgress::default())));
            let cancelled = Arc::new(AtomicBool::new(false));
            let result: Arc<Mutex<Option<Result<std::path::PathBuf, String>>>> = Arc::new(Mutex::new(None));

            let progress_bg = Arc::clone(&progress);
            let cancelled_bg = Arc::clone(&cancelled);
            let result_bg = Arc::clone(&result);
            let ctx_bg = ctx.clone();

            std::thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new().unwrap();
                let dest_dir = forge_download_dir();
                let ctx_callback = ctx_bg.clone();
                let outcome = runtime.block_on(async {
                    crate::download::download_forge_jar_staged(
                        &dest_dir,
                        move |update| {
                            if let Ok(mut guard) = progress_bg.lock() {
                                let entry = guard.get_or_insert_with(DownloadProgress::default);
                                entry.bytes_done = update.bytes_done;
                                entry.total_bytes = update.total_bytes;
                                entry.status_text = format_download_status(update.bytes_done, update.total_bytes);
                            }
                            ctx_callback.request_repaint();
                        },
                        cancelled_bg,
                    )
                    .await
                });

                *result_bg.lock().unwrap() = Some(outcome.map(|(p, _)| p).map_err(|e| e.to_string()));
                ctx_bg.request_repaint();
            });

            self.prelaunch_update_dialog = Some(PreLaunchUpdateDialog {
                launch,
                state: PreLaunchUpdateState::Downloading {
                    asset,
                    progress,
                    cancelled,
                    result,
                },
            });
            ctx.request_repaint();
        }
    }

    // ==================== Home Tab ====================

    fn render_confirm_dialog(&mut self, ctx: &egui::Context) {
        let action = match &self.confirm_action {
            Some(a) => a.clone(),
            None => return,
        };
        let (title, body, confirm_label, confirm_color) = match action {
            ConfirmAction::ResetFirstRun => (
                "Reset to First Run?",
                "This will:\n• Delete all settings (Forge path, auth token, saved links)\n• Show the setup wizard immediately\n\nThe mamoConnector:// URL scheme stays registered so deeplinks keep working.",
                "Reset",
                egui::Color32::from_rgb(230, 130, 0),
            ),
            ConfirmAction::Uninstall => (
                "Uninstall MaMo Connector?",
                "This will:\n• De-register the mamoConnector:// URL scheme\n• Delete all settings and data\n• Delete the application executable\n\nThis cannot be undone.",
                "Uninstall",
                egui::Color32::from_rgb(200, 0, 0),
            ),
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.label(egui::RichText::new(body).color(egui::Color32::from_rgb(60, 60, 60)));
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_action = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(
                            egui::Button::new(egui::RichText::new(confirm_label).color(egui::Color32::WHITE))
                                .fill(confirm_color),
                        ).clicked() {
                            let action = self.confirm_action.take().unwrap();
                            match action {
                                ConfirmAction::ResetFirstRun => self.do_reset(),
                                ConfirmAction::Uninstall => self.do_uninstall(ctx),
                            }
                        }
                    });
                });
            });
    }

    fn do_reset(&mut self) {
        use crate::settings::get_settings_dir;

        // Do NOT unregister the URL scheme — the connector is still running and deeplinks
        // must keep working so new playtest requests trigger the setup wizard.
        // Only do_uninstall() removes the OS registration.

        if let Ok(dir) = get_settings_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Reset in-memory state so the wizard shows immediately
        *self.settings.lock().unwrap() = crate::settings::Settings::default();
        {
            let mut ss = self.settings_state.lock().unwrap();
            ss.forge_path_input = String::new();
            ss.forge_path_valid = false;
            ss.auth_token_input = String::new();
            ss.status_message = Some("Reset complete. Setup wizard will appear on next launch.".to_string());
        }
        self.wizard = SetupWizardState::default();
        self.show_setup_wizard = true;
    }

    fn do_uninstall(&mut self, ctx: &egui::Context) {
        use crate::registration::unregister;
        use crate::settings::get_settings_dir;

        let _ = unregister(crate::SCHEME);

        if let Ok(dir) = get_settings_dir() {
            let _ = std::fs::remove_dir_all(&dir);
        }

        // Self-delete: spawn a script that waits for this process to exit, then deletes the exe
        #[cfg(windows)]
        {
            if let Ok(exe) = std::env::current_exe() {
                let exe_str = exe.to_string_lossy();
                let bat = format!(
                    "@echo off\r\nping -n 3 127.0.0.1 >nul\r\ndel /f /q \"{exe_str}\"\r\ndel /f /q \"%~f0\"\r\n"
                );
                let bat_path = std::env::temp_dir().join("mamo_uninstall.bat");
                if std::fs::write(&bat_path, bat).is_ok() {
                    let _ = std::process::Command::new("cmd")
                        .args(["/c", "start", "/min", &bat_path.to_string_lossy()])
                        .spawn();
                }
            }
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Start (or restart) the MaMo Forge portable-bundle download (JAR + `res/`) in the
    /// background, wiring up the wizard's progress state. Shared by the Welcome step's
    /// auto-start and the DownloadForge screen's manual "Download"/"Re-download" buttons.
    fn start_forge_download(&mut self, ctx: &egui::Context) {
        let forge_dir = forge_download_dir();

        let progress_arc: Arc<Mutex<DownloadProgress>> = Arc::new(Mutex::new(DownloadProgress::default()));
        let result_arc: Arc<Mutex<Option<DownloadResult>>> = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));

        self.wizard.download_progress = Some(Arc::clone(&progress_arc));
        self.wizard.download_result = Some(Arc::clone(&result_arc));
        self.wizard.download_cancelled = Some(Arc::clone(&cancelled));

        let ctx_progress = ctx.clone();
        let ctx_end = ctx.clone();
        let progress_bg = Arc::clone(&progress_arc);
        let result_bg = Arc::clone(&result_arc);
        let cancelled_bg = Arc::clone(&cancelled);

        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let outcome = runtime.block_on(async {
                crate::download::download_forge_portable(
                    &forge_dir,
                    move |update| {
                        if let Ok(mut p) = progress_bg.lock() {
                            p.bytes_done = update.bytes_done;
                            p.total_bytes = update.total_bytes;
                            p.status_text = format_download_status(update.bytes_done, update.total_bytes);
                        }
                        ctx_progress.request_repaint();
                    },
                    cancelled_bg,
                )
                .await
            });

            let terminal = match outcome {
                Ok(jar_path) => {
                    let dir = jar_path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    DownloadResult::Success { jar_dir: dir }
                }
                Err(e) if e.to_string().contains("cancelled") => DownloadResult::Cancelled,
                Err(e) => DownloadResult::Failed(e.to_string()),
            };
            *result_bg.lock().unwrap() = Some(terminal);
            ctx_end.request_repaint();
        });
    }

    /// Triggers an on-demand download of the latest MaMo Connector release in the background.
    fn trigger_connector_update_download(&mut self, ctx: &egui::Context) {
        let asset = {
            let mut s = self.update_check.lock().unwrap();
            if s.is_downloading {
                return;
            }
            s.is_downloading = true;
            s.error = None;
            s.asset.clone()
        };

        let Some(asset) = asset else {
            if let Ok(mut s) = self.update_check.lock() {
                s.is_downloading = false;
                s.error = Some("No update asset metadata found".to_string());
            }
            return;
        };

        self.connector_update_cancelled.store(false, Ordering::Relaxed);
        *self.connector_update_progress.lock().unwrap() = Some(DownloadProgress::default());

        let update_check_bg = Arc::clone(&self.update_check);
        let progress_bg = Arc::clone(&self.connector_update_progress);
        let cancelled_bg = Arc::clone(&self.connector_update_cancelled);
        let ctx_progress = ctx.clone();

        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let progress_cb = Arc::clone(&progress_bg);
                let ctx_cb = ctx_progress.clone();
                let outcome = crate::download::download_connector_update_staged(
                    &asset,
                    move |update| {
                        if let Ok(mut guard) = progress_cb.lock() {
                            let entry = guard.get_or_insert_with(DownloadProgress::default);
                            entry.bytes_done = update.bytes_done;
                            entry.total_bytes = update.total_bytes;
                            entry.status_text = format_download_status(update.bytes_done, update.total_bytes);
                        }
                        ctx_cb.request_repaint();
                    },
                    cancelled_bg,
                )
                .await;

                match outcome {
                    Ok(staged_path) => {
                        log::info!("MaMo Connector update downloaded to {:?}", staged_path);
                        if let Ok(mut s) = update_check_bg.lock() {
                            s.staged_path = Some(staged_path);
                            s.is_downloading = false;
                            s.error = None;
                        }
                        *progress_bg.lock().unwrap() = None;
                    }
                    Err(e) if e.to_string().contains("cancelled") => {
                        log::info!("MaMo Connector update download cancelled");
                        if let Ok(mut s) = update_check_bg.lock() {
                            s.is_downloading = false;
                        }
                        *progress_bg.lock().unwrap() = None;
                    }
                    Err(e) => {
                        log::error!("Failed to download MaMo Connector update: {e}");
                        if let Ok(mut s) = update_check_bg.lock() {
                            s.is_downloading = false;
                            s.error = Some(e.to_string());
                        }
                        *progress_bg.lock().unwrap() = None;
                    }
                }
                ctx_progress.request_repaint();
            });
        });
    }

    /// "Check now" — runs the same check-and-auto-download `LauncherApp::new`'s 5s-after-startup
    /// timer already runs, on demand. Guarded by `busy` so a click while one's already in
    /// flight (background or otherwise) doesn't start a second, redundant fetch.
    fn trigger_forge_update_check(&mut self, ctx: &egui::Context) {
        {
            let mut s = self.forge_update_check.lock().unwrap();
            if s.busy {
                return;
            }
            s.busy = true;
            s.dismissed = false;
        }
        *self.forge_update_progress.lock().unwrap() = None;
        let forge_update_check = Arc::clone(&self.forge_update_check);
        let forge_update_progress = Arc::clone(&self.forge_update_progress);
        let cancelled = Arc::clone(&self.forge_update_cancelled);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(run_forge_update_check_and_download(
                forge_update_check,
                forge_update_progress,
                cancelled,
                ctx,
            ));
        });
    }

    /// "Check now" for MaMo Connector updates — runs on-demand GitHub release check.
    fn trigger_connector_update_check(&mut self, ctx: &egui::Context) {
        {
            let mut s = self.update_check.lock().unwrap();
            if s.busy || s.is_downloading {
                return;
            }
            s.busy = true;
            s.error = None;
        }
        let update_check = Arc::clone(&self.update_check);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                match crate::download::resolve_connector_release_asset().await {
                    Ok(asset) => {
                        let is_newer = is_newer_version(&asset.version, env!("CARGO_PKG_VERSION"));
                        if let Ok(mut s) = update_check.lock() {
                            s.busy = false;
                            if is_newer {
                                s.available_version = Some(asset.version.clone());
                                s.asset = Some(asset);
                                s.dismissed = false;
                            } else {
                                s.available_version = None;
                                s.asset = None;
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Connector update check failed: {e}");
                        if let Ok(mut s) = update_check.lock() {
                            s.busy = false;
                            s.error = Some(e.to_string());
                        }
                    }
                }
                ctx.request_repaint();
            });
        });
    }

    /// Swaps a fully-downloaded, staged MaMo Forge update into place the moment Forge is
    /// confirmed not running — called from the same 500ms tick `check_pending_commands` already
    /// runs on. Unlike the old click-triggered `start_forge_update` this replaced, downloading
    /// happens automatically the moment an update is detected (`run_forge_update_check_and_download`);
    /// this step only ever does a fast local rename, deferred as many ticks as it takes for
    /// Forge to close, so it never risks overwriting a jar Forge might still have open.
    fn finalize_staged_forge_update_if_ready(&mut self) {
        let staged = self.forge_update_check.lock().unwrap().staged.clone();
        let Some(staged) = staged else { return };
        if crate::forge::is_forge_window_open() {
            return; // still running — try again on the next tick
        }

        let forge_dir = forge_download_dir();
        match crate::download::finalize_staged_forge_jar(&forge_dir, &staged.staged_path, &staged.asset) {
            Ok(_final_path) => {
                if let Ok(mut log) = self.activity_log.lock() {
                    log.log_success("MaMo Forge updated to the latest build.");
                }
            }
            Err(e) => {
                if let Ok(mut log) = self.activity_log.lock() {
                    log.log_error(format!("Failed to install downloaded MaMo Forge update: {e}"));
                }
            }
        }
        // Clear either way — a failed rename (e.g. Forge opened again in the instant between
        // the check above and now) will simply be re-detected and re-downloaded next check.
        self.forge_update_check.lock().unwrap().staged = None;
    }

    fn render_setup_wizard(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);

            match self.wizard.step.clone() {
                // ── Step 1: Welcome ───────────────────────────────────────────
                WizardStep::Welcome => {
                    ui.label(egui::RichText::new("🔌").size(48.0));
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Welcome to MaMo Connector").size(22.0).strong());
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(
                            "Before you can playtest in Forge, we need to find your\n\
                             Forge MTG installation. This only takes a moment."
                        )
                        .color(egui::Color32::from_rgb(80, 80, 80)),
                    );
                    ui.add_space(24.0);
                    if ui.add(egui::Button::new(
                        egui::RichText::new("Get Started →").size(16.0)
                    ).min_size(egui::vec2(160.0, 36.0))).clicked() {
                        self.wizard.step = WizardStep::DownloadForge;
                        // Combine setup into one flow: start fetching MaMo Forge immediately
                        // instead of waiting for a second, separate button click — unless it's
                        // already cached from a previous run (e.g. after Reset to First Run).
                        if !forge_jar_already_downloaded() {
                            self.start_forge_download(ctx);
                        }
                    }
                }

                // ── Step 1b: Download MaMo Forge ─────────────────────────────
                WizardStep::DownloadForge => {
                    ui.set_max_width(500.0);
                    ui.label(egui::RichText::new("⬇ Download MaMo Forge").size(20.0).strong());
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new(
                            "MaMo uses a custom Forge build with replay recording, \
                             commander simulation, and MaMo integration. \
                             Download it automatically (~400 MB)."
                        )
                        .color(egui::Color32::from_rgb(80, 80, 80)),
                    ).wrap());
                    ui.add_space(16.0);

                    // Read current progress state
                    let (prog_done, prog_total, prog_text, prog_finished, prog_error) = self
                        .wizard
                        .download_progress
                        .as_ref()
                        .and_then(|a| a.try_lock().ok())
                        .map(|p| (p.bytes_done, p.total_bytes, p.status_text.clone(), p.finished, p.error.clone()))
                        .unwrap_or_default();

                    let is_downloading = self.wizard.download_progress.is_some() && !prog_finished;

                    if is_downloading {
                        // ── Downloading ──────────────────────────────────────
                        ui.label(egui::RichText::new(&prog_text).color(egui::Color32::from_rgb(0, 100, 180)));
                        ui.add_space(6.0);
                        match prog_total {
                            Some(total) if total > 0 => {
                                ui.add(
                                    egui::ProgressBar::new(prog_done as f32 / total as f32)
                                        .show_percentage()
                                        .desired_width(380.0),
                                );
                            }
                            _ => {
                                ui.add(egui::ProgressBar::new(0.0).animate(true).desired_width(380.0));
                            }
                        }
                    } else {
                        // ── Idle / error / already downloaded ────────────────
                        let forge_dir = forge_download_dir();

                        if forge_jar_already_downloaded() && prog_error.is_none() {
                            // Already downloaded — offer to reuse or re-download
                            ui.label(
                                egui::RichText::new("✓ MaMo Forge already downloaded")
                                    .color(egui::Color32::from_rgb(0, 140, 0))
                                    .strong(),
                            );
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if ui.add(
                                    egui::Button::new(egui::RichText::new("Use Existing →").strong())
                                        .min_size(egui::vec2(130.0, 30.0)),
                                ).clicked() {
                                    let s = forge_dir.to_string_lossy().to_string();
                                    self.wizard.forge_path_valid = validate_forge_path(&s);
                                    self.wizard.forge_path_input = s;
                                    self.wizard.step = WizardStep::ConfigureForge;
                                }
                                if ui.button("Re-download").clicked() {
                                    self.start_forge_download(ctx);
                                }
                            });
                            ui.add_space(6.0);
                        } else {
                            // Show any previous error
                            if let Some(ref err) = prog_error {
                                ui.label(
                                    egui::RichText::new(format!("✗ {err}"))
                                        .color(egui::Color32::from_rgb(200, 0, 0))
                                        .small(),
                                );
                                ui.add_space(6.0);
                            }

                            if ui.add(
                                egui::Button::new(egui::RichText::new("⬇ Download MaMo Forge").size(15.0))
                                    .min_size(egui::vec2(210.0, 36.0)),
                            ).clicked() {
                                self.start_forge_download(ctx);
                            }
                        }
                    }
                }

                // ── Step 2: Configure Forge path ─────────────────────────────
                WizardStep::ConfigureForge => {
                    ui.label(egui::RichText::new("🎮 Configure Forge").size(20.0).strong());
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Point MaMo Connector to your Forge installation.")
                            .color(egui::Color32::from_rgb(80, 80, 80)),
                    );
                    ui.add_space(16.0);

                    // Path input row
                    ui.horizontal(|ui| {
                        let mut path = self.wizard.forge_path_input.clone();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut path)
                                .desired_width(380.0)
                                .hint_text("Path to forge.exe, .jar, or Forge directory"),
                        );
                        if resp.changed() {
                            self.wizard.forge_path_valid = validate_forge_path(&path);
                            self.wizard.forge_path_input = path;
                            self.wizard.test_status = None;
                        }
                        if self.wizard.forge_path_input.is_empty() {
                            ui.label(egui::RichText::new("  ").weak());
                        } else if self.wizard.forge_path_valid {
                            ui.label(egui::RichText::new("✓").color(egui::Color32::from_rgb(0, 150, 0)).strong());
                        } else {
                            ui.label(egui::RichText::new("✗").color(egui::Color32::from_rgb(200, 0, 0)).strong());
                        }
                    });
                    ui.add_space(8.0);

                    // Helper buttons
                    ui.horizontal(|ui| {
                        if ui.button("🔍 Auto-detect").clicked() {
                            if let Some(p) = get_default_forge_path() {
                                let s = p.to_string_lossy().to_string();
                                self.wizard.forge_path_valid = validate_forge_path(&s);
                                self.wizard.forge_path_input = s;
                                self.wizard.test_status = None;
                            }
                        }
                        if ui.button("📁 Browse…").clicked() {
                            let dialog = rfd::FileDialog::new()
                                .add_filter("Forge Executable", &["exe", "jar", "bat"])
                                .add_filter("All Files", &["*"])
                                .set_title("Select Forge Executable or Directory");
                            if let Some(path) = dialog.pick_file() {
                                let s = path.to_string_lossy().to_string();
                                self.wizard.forge_path_valid = validate_forge_path(&s);
                                self.wizard.forge_path_input = s;
                                self.wizard.test_status = None;
                            }
                        }
                        if ui.button("📂 Folder…").clicked() {
                            if let Some(folder) = rfd::FileDialog::new()
                                .set_title("Select Forge Directory")
                                .pick_folder()
                            {
                                let s = folder.to_string_lossy().to_string();
                                self.wizard.forge_path_valid = validate_forge_path(&s);
                                self.wizard.forge_path_input = s;
                                self.wizard.test_status = None;
                            }
                        }
                    });
                    ui.add_space(12.0);

                    // Test launch row
                    ui.horizontal(|ui| {
                        let can_test = self.wizard.forge_path_valid
                            && !matches!(self.wizard.test_status, Some(WizardTestStatus::Testing));
                        if ui.add_enabled(can_test, egui::Button::new("▶ Test Launch")).clicked() {
                            self.wizard.test_status = Some(WizardTestStatus::Testing);
                            let path = self.wizard.forge_path_input.clone();
                            let settings_arc = self.settings.clone();
                            let ctx2 = ctx.clone();
                            let chan: Arc<Mutex<Option<WizardTestStatus>>> = Arc::new(Mutex::new(None));
                            self.wizard.pending_test_result = Some(Arc::clone(&chan));
                            std::thread::spawn(move || {
                                // Persist path temporarily so launch_forge_from_settings can read it
                                {
                                    let mut s = settings_arc.lock().unwrap();
                                    s.forge_path = Some(path);
                                    let _ = s.save();
                                }
                                let status = match launch_forge_from_settings(None, None) {
                                    Ok(r) if r.success || r.already_running => WizardTestStatus::Ok,
                                    Ok(r) => WizardTestStatus::Err(r.message),
                                    Err(e) => WizardTestStatus::Err(e.to_string()),
                                };
                                *chan.lock().unwrap() = Some(status);
                                ctx2.request_repaint();
                            });
                        }
                        match &self.wizard.test_status {
                            Some(WizardTestStatus::Testing) => {
                                ui.spinner();
                                ui.label(egui::RichText::new("Launching Forge…").weak());
                            }
                            Some(WizardTestStatus::Ok) => {
                                ui.label(egui::RichText::new("✓ Forge launched successfully").color(egui::Color32::from_rgb(0, 150, 0)));
                            }
                            Some(WizardTestStatus::Err(msg)) => {
                                ui.label(egui::RichText::new(format!("✗ {msg}")).color(egui::Color32::from_rgb(200, 0, 0)));
                            }
                            None => {}
                        }
                    });
                    ui.add_space(16.0);

                    // Java runtime status — Forge needs Java 17+
                    if self.wizard.java_status.is_none() {
                        self.wizard.java_status = Some(crate::forge::detect_java());
                    }
                    match self.wizard.java_status.clone() {
                        Some(crate::forge::JavaStatus::Ok(major)) => {
                            ui.label(
                                egui::RichText::new(format!("✓ Java {major} detected"))
                                    .color(egui::Color32::from_rgb(0, 150, 0))
                                    .small(),
                            );
                        }
                        Some(status) => {
                            let msg = match status {
                                crate::forge::JavaStatus::TooOld(m) => format!(
                                    "⚠ Java {m} found, but Forge needs Java 17 or newer."
                                ),
                                _ => "⚠ Java 17 is required to run Forge, but none was found.".to_string(),
                            };
                            egui::Frame::default()
                                .fill(egui::Color32::from_rgb(255, 244, 224))
                                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(230, 160, 60)))
                                .inner_margin(egui::Margin::same(10.0))
                                .rounding(6.0)
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(msg)
                                            .color(egui::Color32::from_rgb(150, 80, 0)),
                                    );
                                    ui.add_space(6.0);
                                    ui.horizontal(|ui| {
                                        if ui.button("📥 Download Java 17").clicked() {
                                            let _ = std::process::Command::new("cmd")
                                                .args(["/c", "start", crate::forge::JAVA_DOWNLOAD_URL])
                                                .spawn();
                                        }
                                        if ui.button("🔄 Re-check").clicked() {
                                            self.wizard.java_status = Some(crate::forge::detect_java());
                                        }
                                    });
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(
                                            "That page offers two options: the .msi installer is \
                                             easiest, but needs administrator rights. No admin \
                                             rights? Pick the .zip instead — extract it anywhere, \
                                             then set a JAVA_HOME environment variable (your own \
                                             account, no admin needed) pointing at that folder, \
                                             and click Re-check."
                                        )
                                        .small()
                                        .color(egui::Color32::from_rgb(150, 80, 0)),
                                    );
                                });
                        }
                        None => {}
                    }
                    ui.add_space(16.0);

                    ui.horizontal(|ui| {
                        let can_finish = self.wizard.forge_path_valid;
                        if ui.add_enabled(
                            can_finish,
                            egui::Button::new(egui::RichText::new("Save & Finish ✓").strong())
                                .min_size(egui::vec2(130.0, 28.0)),
                        ).clicked() {
                            // Persist the path
                            let path = self.wizard.forge_path_input.clone();
                            {
                                let mut settings = self.settings.lock().unwrap();
                                settings.forge_path = Some(path.clone());
                                let _ = settings.save();
                            }
                            // Sync into settings_state so the Settings tab shows it too
                            {
                                let mut ss = self.settings_state.lock().unwrap();
                                ss.forge_path_input = path;
                                ss.forge_path_valid = true;
                            }
                            self.wizard.step = WizardStep::Done;
                        }
                    });
                }

                // ── Step 3: Done ─────────────────────────────────────────────
                WizardStep::Done => {
                    ui.label(egui::RichText::new("✅").size(48.0));
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("You're all set!").size(22.0).strong());
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(
                            "Forge is configured. Click any playtest button in MaMo\n\
                             to launch Forge with your deck loaded."
                        )
                        .color(egui::Color32::from_rgb(80, 80, 80)),
                    );
                    ui.add_space(24.0);
                    if ui.add(egui::Button::new(
                        egui::RichText::new("Close").size(15.0)
                    ).min_size(egui::vec2(100.0, 32.0))).clicked() {
                        self.show_setup_wizard = false;
                    }
                }
            }
        });
    }

    /// The user's full MaMo account deck list, sorted by name — every deck the user owns is
    /// pickable here, whether or not it has been downloaded locally yet (see
    /// `launch_account_deck_async`, which downloads on demand).
    fn account_decks(&self) -> Vec<crate::gamelog::UserDeck> {
        let mut decks = self.gamelog_state.lock().unwrap().user_decks.clone();
        decks.sort_by(|a, b| a.deck_name.to_lowercase().cmp(&b.deck_name.to_lowercase()));
        decks
    }

    /// Apply the outcome of a synchronous Forge launch (activity log + PID tracking), shared by
    /// every "launch with an already-local deck" code path in the Home tab.
    fn apply_forge_launch_result(&mut self, result: anyhow::Result<ForgeLaunchResult>) {
        match result {
            Ok(result) => {
                if let Ok(mut log) = self.activity_log.lock() {
                    if result.success {
                        log.log_success(&result.message);
                    } else {
                        log.log_info(&result.message);
                    }
                }
                // Track Forge PID for auto gamelog scanning
                if let Some(pid) = result.pid {
                    *self.forge_pid.lock().unwrap() = Some(pid);
                    *self.forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    self.forge_window_seen = false;
                    self.forge_launcher_exited_at = None;
                }
            }
            Err(e) => {
                if let Ok(mut log) = self.activity_log.lock() {
                    log.log_error(format!("Failed to launch Forge: {}", e));
                }
            }
        }
    }

    /// Download an account deck the user picked in the Home tab (but hasn't downloaded locally
    /// yet) and launch Forge with it once ready. Mirrors the deeplink-driven
    /// download-then-launch flow in `commands.rs::handle_launch_forge_with_logger`.
    fn launch_account_deck_async(&mut self, deck: crate::gamelog::UserDeck, ctx: &egui::Context) {
        *self.is_launching_selected_deck.lock().unwrap() = true;
        *self.play_session.lock().unwrap() = PlaySession::Launching;

        let activity_log = self.activity_log.clone();
        let forge_pid = self.forge_pid.clone();
        let forge_monitoring_since = self.forge_monitoring_since.clone();
        let is_launching = self.is_launching_selected_deck.clone();
        let refresh_requested = self.forge_local_decks_refresh_requested.clone();
        let play_session = Arc::clone(&self.play_session);
        let ctx_clone = ctx.clone();

        if let Ok(mut log) = self.activity_log.lock() {
            log.log_info(format!("Downloading '{}' before launching Forge…", deck.deck_name));
        }

        tokio::spawn(async move {
            let forge_result = match crate::deck::create_deck_from_mamo(&deck.deck_id).await {
                Ok(deck_result) if deck_result.success => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_success(&deck_result.message);
                    }
                    let deck_path_str = deck_result.deck_path.as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                    let deck2_path = resolve_curated_opponent_deck_path(&activity_log).await;
                    launch_forge_from_settings(deck_path_str.as_deref(), deck2_path.as_deref())
                }
                Ok(deck_result) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Deck download failed: {}", deck_result.message));
                    }
                    *is_launching.lock().unwrap() = false;
                    *play_session.lock().unwrap() = PlaySession::Watching;
                    ctx_clone.request_repaint();
                    return;
                }
                Err(e) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Failed to download deck: {}", e));
                    }
                    *is_launching.lock().unwrap() = false;
                    *play_session.lock().unwrap() = PlaySession::Watching;
                    ctx_clone.request_repaint();
                    return;
                }
            };

            match forge_result {
                Ok(result) => {
                    if let Ok(mut log) = activity_log.lock() {
                        if result.success {
                            log.log_success(&result.message);
                        } else {
                            log.log_info(&result.message);
                        }
                    }
                    if let Some(pid) = result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                        // ponytail: forge_window_seen/forge_launcher_exited_at aren't reset here
                        // (they're plain, non-Arc fields owned by the UI thread) — worst case the
                        // window-open debounce is slightly off for this launch. Upgrade path: move
                        // those two fields behind Arc<Mutex<_>> like forge_pid if this becomes a
                        // real problem.
                    }
                    *play_session.lock().unwrap() = if result.success { PlaySession::Playing } else { PlaySession::Watching };
                }
                Err(e) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Failed to launch Forge: {}", e));
                    }
                    *play_session.lock().unwrap() = PlaySession::Watching;
                }
            }

            refresh_requested.store(true, Ordering::Relaxed);
            *is_launching.lock().unwrap() = false;
            ctx_clone.request_repaint();
        });
    }

    /// Download+launch a scenario picked from the Play tab's scenario list — the scenario
    /// counterpart to `launch_account_deck_async` above: same activity-log/PID/play-session
    /// bookkeeping and curated-opponent resolution, but prepares the scenario-ordered deck
    /// (`create_deck_and_scenario_for_forge`) instead of a plain deck download.
    fn launch_scenario_async(
        &mut self,
        deck_id: String,
        scenario_id: String,
        scenario_name: String,
        ctx: &egui::Context,
    ) {
        *self.is_launching_selected_deck.lock().unwrap() = true;
        *self.play_session.lock().unwrap() = PlaySession::Launching;

        let activity_log = self.activity_log.clone();
        let forge_pid = self.forge_pid.clone();
        let forge_monitoring_since = self.forge_monitoring_since.clone();
        let is_launching = self.is_launching_selected_deck.clone();
        let refresh_requested = self.forge_local_decks_refresh_requested.clone();
        let play_session = Arc::clone(&self.play_session);
        let ctx_clone = ctx.clone();

        if let Ok(mut log) = self.activity_log.lock() {
            log.log_info(format!("Preparing scenario '{}' before launching Forge…", scenario_name));
        }

        tokio::spawn(async move {
            let forge_result = match crate::deck::create_deck_and_scenario_for_forge(&deck_id, &scenario_id).await {
                Ok(deck_result) if deck_result.success => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_success(&deck_result.message);
                    }
                    let deck_path_str = deck_result.deck_path.as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                    let deck2_path = resolve_curated_opponent_deck_path(&activity_log).await;
                    launch_forge_from_settings(deck_path_str.as_deref(), deck2_path.as_deref())
                }
                Ok(deck_result) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Scenario preparation failed: {}", deck_result.message));
                    }
                    *is_launching.lock().unwrap() = false;
                    *play_session.lock().unwrap() = PlaySession::Watching;
                    ctx_clone.request_repaint();
                    return;
                }
                Err(e) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Failed to prepare scenario: {}", e));
                    }
                    *is_launching.lock().unwrap() = false;
                    *play_session.lock().unwrap() = PlaySession::Watching;
                    ctx_clone.request_repaint();
                    return;
                }
            };

            match forge_result {
                Ok(result) => {
                    if let Ok(mut log) = activity_log.lock() {
                        if result.success {
                            log.log_success(&result.message);
                        } else {
                            log.log_info(&result.message);
                        }
                    }
                    if let Some(pid) = result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                    *play_session.lock().unwrap() = if result.success { PlaySession::Playing } else { PlaySession::Watching };
                }
                Err(e) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Failed to launch Forge: {}", e));
                    }
                    *play_session.lock().unwrap() = PlaySession::Watching;
                }
            }

            refresh_requested.store(true, Ordering::Relaxed);
            *is_launching.lock().unwrap() = false;
            ctx_clone.request_repaint();
        });
    }

    /// Launches Forge with a deck that's already downloaded locally, still resolving a curated
    /// opponent deck for `--deck2` first — the counterpart to `launch_account_deck_async` for
    /// when deck1 doesn't need downloading. Kept async (rather than the previous one-line
    /// synchronous `launch_forge_from_settings` call) purely because picking an opponent now
    /// means a network round-trip, which must never run on the UI thread.
    fn launch_local_deck_with_curated_opponent_async(&mut self, local_stem: String, ctx: &egui::Context) {
        *self.is_launching_selected_deck.lock().unwrap() = true;
        *self.play_session.lock().unwrap() = PlaySession::Launching;

        let activity_log = self.activity_log.clone();
        let forge_pid = self.forge_pid.clone();
        let forge_monitoring_since = self.forge_monitoring_since.clone();
        let is_launching = self.is_launching_selected_deck.clone();
        let refresh_requested = self.forge_local_decks_refresh_requested.clone();
        let play_session = Arc::clone(&self.play_session);
        let ctx_clone = ctx.clone();

        tokio::spawn(async move {
            let deck2_path = resolve_curated_opponent_deck_path(&activity_log).await;
            let forge_result = launch_forge_from_settings(Some(&local_stem), deck2_path.as_deref());

            match forge_result {
                Ok(result) => {
                    if let Ok(mut log) = activity_log.lock() {
                        if result.success {
                            log.log_success(&result.message);
                        } else {
                            log.log_info(&result.message);
                        }
                    }
                    if let Some(pid) = result.pid {
                        *forge_pid.lock().unwrap() = Some(pid);
                        *forge_monitoring_since.lock().unwrap() = Some(Instant::now());
                    }
                    *play_session.lock().unwrap() = if result.success { PlaySession::Playing } else { PlaySession::Watching };
                }
                Err(e) => {
                    if let Ok(mut log) = activity_log.lock() {
                        log.log_error(format!("Failed to launch Forge: {}", e));
                    }
                    *play_session.lock().unwrap() = PlaySession::Watching;
                }
            }

            refresh_requested.store(true, Ordering::Relaxed);
            *is_launching.lock().unwrap() = false;
            ctx_clone.request_repaint();
        });
    }

    fn render_play_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Compact connection status — full account/Forge management lives in Setup now;
            // this is just enough to tell at a glance whether Play is ready to use.
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let has_token = {
                        let state = self.settings_state.lock().unwrap();
                        !state.auth_token_input.is_empty()
                    };
                    if has_token {
                        ui.label(egui::RichText::new("Connected to MaMo").color(egui::Color32::from_rgb(0, 128, 0)));
                    } else {
                        ui.label(egui::RichText::new("Not connected to MaMo account").color(egui::Color32::from_rgb(176, 0, 32)));
                        if ui.small_button("Connect").clicked() {
                            let _ = std::process::Command::new("cmd")
                                .args(["/c", "start", MAMO_WEBSITE_URL])
                                .spawn();
                            self.current_tab = Tab::Setup;
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Registration status (compact)
                        let reg_text = match self.state.registration.status {
                            RegistrationStatus::Registered => {
                                egui::RichText::new("Deeplink OK").small().color(egui::Color32::from_rgb(0, 128, 0)).strong()
                            }
                            RegistrationStatus::Failed => {
                                egui::RichText::new("Deeplink FAIL").small().color(egui::Color32::from_rgb(176, 0, 32)).strong()
                            }
                            RegistrationStatus::Skipped => {
                                egui::RichText::new("Deeplink N/A").small().color(egui::Color32::from_rgb(196, 112, 0))
                            }
                        };
                        ui.label(reg_text);
                    });
                });
            });

            ui.add_space(10.0);

            // Your decks — standalone start (pick a deck, launch Forge) plus manual log
            // upload/retry, independent of anything triggered from the website.
            ui.group(|ui| {
                ui.label(egui::RichText::new("Your decks").strong());
                ui.add_space(5.0);

                // Lazy-load local Forge decks on first render
                if self.forge_local_decks.is_empty() {
                    self.forge_local_decks = list_forge_decks();
                }

                ui.horizontal(|ui| {
                    // Launch Forge button
                    let forge_configured = {
                        let state = self.settings_state.lock().unwrap();
                        state.forge_path_valid
                    };
                    if forge_configured {
                        let is_launching = *self.is_launching_selected_deck.lock().unwrap();
                        let label = if is_launching { "Launching…" } else { "Launch Forge" };
                        if ui.add_enabled(!is_launching, egui::Button::new(label)).clicked() {
                            match self.selected_account_deck.clone() {
                                None => {
                                    self.request_forge_launch(PendingForgeLaunch::Plain, ctx);
                                }
                                Some(deck) => match find_local_deck_path(&deck, &self.forge_local_decks) {
                                    Some(local_stem) => {
                                        self.request_forge_launch(PendingForgeLaunch::LocalDeckWithCuratedOpponent { local_stem }, ctx);
                                    }
                                    None => self.request_forge_launch(PendingForgeLaunch::AccountDeck(deck), ctx),
                                },
                            }
                        }
                    } else {
                        ui.add_enabled(false, egui::Button::new("Launch Forge"));
                        if ui.small_button("Configure Forge →").clicked() {
                            self.current_tab = Tab::Setup;
                        }
                    }

                    // Upload Logs button
                    let (is_scanning, directory_valid, is_retrying_failed) = {
                        let state = self.gamelog_state.lock().unwrap();
                        (state.is_scanning, state.directory_valid, state.is_retrying_failed)
                    };
                    if ui.add_enabled(!is_scanning && directory_valid, egui::Button::new("Upload Logs")).clicked() {
                        self.start_gamelog_scan(ctx);
                    }
                    if is_scanning {
                        ui.spinner();
                        ui.label("Uploading...");
                    }

                    // Retry Failed button — triggers backend re-parse of parse_failed logs
                    if ui.add_enabled(!is_retrying_failed, egui::Button::new("Retry Failed")).clicked() {
                        self.start_reparse_failed(ctx);
                    }
                    if is_retrying_failed {
                        ui.spinner();
                        ui.label("Re-parsing...");
                    }
                    // Show result of last reparse
                    let reparse_status = self.gamelog_state.lock().unwrap().reparse_status.clone();
                    if let Some(ref msg) = reparse_status {
                        let color = if msg.starts_with("Error") {
                            egui::Color32::from_rgb(176, 0, 32)
                        } else {
                            egui::Color32::from_rgb(0, 128, 0)
                        };
                        ui.label(egui::RichText::new(msg).small().color(color));
                    }
                });

                // Deck pre-selection for Forge launch
                let forge_configured = {
                    let state = self.settings_state.lock().unwrap();
                    state.forge_path_valid
                };
                if forge_configured {
                    let deck_id_before = self.selected_account_deck.as_ref().map(|d| d.deck_id.clone());
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Deck:").small());
                        let decks_snapshot = self.account_decks();
                        let local_decks = self.forge_local_decks.clone();
                        let label_for = |deck: &crate::gamelog::UserDeck| {
                            if find_local_deck_path(deck, &local_decks).is_some() {
                                deck.deck_name.clone()
                            } else {
                                format!("{} (download)", deck.deck_name)
                            }
                        };
                        let selected_label = self.selected_account_deck
                            .as_ref()
                            .map(|d| label_for(d))
                            .unwrap_or_else(|| "— none —".to_string());
                        egui::ComboBox::from_id_source("forge_launch_deck")
                            .width(220.0)
                            .selected_text(selected_label)
                            .show_ui(ui, |ui: &mut egui::Ui| {
                                ui.selectable_value(
                                    &mut self.selected_account_deck,
                                    None,
                                    "— none —",
                                );
                                for deck in &decks_snapshot {
                                    let label = label_for(deck);
                                    ui.selectable_value(
                                        &mut self.selected_account_deck,
                                        Some(deck.clone()),
                                        label,
                                    );
                                }
                            });
                        if decks_snapshot.is_empty() {
                            ui.label(egui::RichText::new("(no decks loaded yet)").small().color(egui::Color32::GRAY));
                        }
                        if ui.small_button("↺").on_hover_text("Refresh deck list (local files + MaMo account)").clicked() {
                            self.forge_local_decks = list_forge_decks();
                            self.fetch_my_mamo_decks(ctx);
                        }
                    });

                    // Selecting a different deck refreshes its scenario picker below.
                    let deck_id_after = self.selected_account_deck.as_ref().map(|d| d.deck_id.clone());
                    if deck_id_after != deck_id_before {
                        match self.selected_account_deck.clone() {
                            Some(deck) => self.fetch_scenarios_for_deck(deck.deck_id, ctx),
                            None => *self.scenario_picker.lock().unwrap() = ScenarioPickerState::default(),
                        }
                    }

                    self.render_scenario_picker(ui, ctx);
                }
            });

            ui.add_space(10.0);

            let directory_valid = self.gamelog_state.lock().unwrap().directory_valid;
            if !directory_valid {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("⚠ Game log folder not configured").color(egui::Color32::from_rgb(196, 112, 0)));
                        if ui.small_button("Configure →").clicked() {
                            self.current_tab = Tab::Settings;
                        }
                    });
                    ui.label(egui::RichText::new("Uploads (and the Activity below) can't work until this is set.").small().color(egui::Color32::GRAY));
                });
                ui.add_space(10.0);
            }

            // Activity — the play-session timeline (journey 2's centerpiece). Always shows the
            // same six steps so the whole lifecycle is visible at a glance; the one matching
            // what's happening right now is highlighted, earlier ones are marked done, later
            // ones stay pending. This is the same story whether the session was started here or
            // from the MaMo website.
            ui.group(|ui| {
                ui.label(egui::RichText::new("Activity").strong());
                ui.label(
                    egui::RichText::new(
                        "Whatever you launch — from here or from the MaMo website — shows up \
                         here, through to the finished game analysis. This keeps going for as \
                         many games as you play in one sitting.",
                    )
                    .small()
                    .color(egui::Color32::GRAY),
                );
                ui.add_space(6.0);

                let ps = self.play_session.lock().unwrap().clone();
                let current = play_session_step_index(&ps);
                let is_issue = matches!(ps, PlaySession::UploadIssue { .. });

                // No leading glyph on these titles — egui's bundled font doesn't cover the
                // dotted-circle/circled-digit/dingbat characters that would naturally go here
                // (confirmed by a real screenshot: they rendered as tofu boxes), and the
                // colored frame below already carries the done/active/pending signal on its own.
                let steps: [(&str, &str); 6] = [
                    ("Watching for a game to start", "Nothing to do — launch a deck above, or start one from the MaMo website"),
                    ("Launching Forge", "Preparing your deck"),
                    ("Game in progress", "Forge is running — we'll pick this up when you're done"),
                    ("Scanning for your game log", "Forge closed — checking what you played"),
                    ("Uploading", "Sending your game log to MaMo"),
                    ("Uploaded — analysis ready", "Play a deck to see this update"),
                ];

                for (i, (title, default_sub)) in steps.into_iter().enumerate() {
                    let (fill, stroke, text_color) = if is_issue && i == 4 {
                        (egui::Color32::from_rgb(251, 228, 226), egui::Color32::from_rgb(211, 55, 47), egui::Color32::from_rgb(140, 30, 25))
                    } else if i < current {
                        (egui::Color32::from_rgb(227, 247, 236), egui::Color32::from_rgb(23, 145, 90), egui::Color32::from_rgb(15, 90, 60))
                    } else if i == current {
                        (egui::Color32::from_rgb(226, 222, 250), egui::Color32::from_rgb(76, 92, 196), egui::Color32::BLACK)
                    } else {
                        (egui::Color32::from_rgb(246, 245, 250), egui::Color32::from_rgb(216, 211, 238), egui::Color32::GRAY)
                    };

                    egui::Frame::default()
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0, stroke))
                        .inner_margin(egui::Margin::same(10.0))
                        .rounding(6.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(title).strong().color(text_color));
                            if is_issue && i == 4 {
                                if let PlaySession::UploadIssue { ref message } = ps {
                                    ui.label(egui::RichText::new(message).small().color(text_color));
                                }
                            } else if i == 5 {
                                if let PlaySession::Uploaded { ref deck_id, ref filename } = ps {
                                    ui.label(egui::RichText::new(filename).small().color(text_color));
                                    if let Some(id) = deck_id {
                                        if ui.small_button("View analysis on MaMo").clicked() {
                                            let url = format!(
                                                "{}/DeckBuilding/playbook?deckId={}&tab=evaluation",
                                                MAMO_WEBSITE_URL, id
                                            );
                                            let _ = std::process::Command::new("cmd").args(["/c", "start", &url]).spawn();
                                        }
                                    } else {
                                        ui.label(
                                            egui::RichText::new("Couldn't match this to a deck — map it under Settings → Deck Mapping")
                                                .small()
                                                .color(egui::Color32::GRAY),
                                        );
                                    }
                                } else {
                                    ui.label(egui::RichText::new(default_sub).small().color(text_color));
                                }
                            } else {
                                ui.label(egui::RichText::new(default_sub).small().color(text_color));
                            }
                        });
                    ui.add_space(6.0);
                }
            });

            ui.add_space(10.0);

            // Per-file detail for the last scan — secondary to the Activity timeline above
            // (which only ever reflects the single most relevant file), so a batch of several
            // logs found at once is still fully visible here, just tucked behind a disclosure.
            let scan_results: Vec<GameLogProcessResult> = {
                let state = self.gamelog_state.lock().unwrap();
                state.scan_results.clone()
            };

            if !scan_results.is_empty() {
                let successful = scan_results.iter().filter(|r| r.success).count();
                let failed = scan_results.len() - successful;
                ui.collapsing(format!("Recent uploads ({successful} uploaded, {failed} failed)"), |ui| {
                    egui::ScrollArea::vertical()
                        .id_source("home_scan_results")
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for result in &scan_results {
                                ui.horizontal(|ui| {
                                    let (icon, color) = if result.success {
                                        ("✓", egui::Color32::from_rgb(0, 128, 0))
                                    } else {
                                        ("✗", egui::Color32::from_rgb(176, 0, 32))
                                    };
                                    ui.label(egui::RichText::new(icon).color(color));
                                    ui.label(egui::RichText::new(&result.filename).small());
                                    if !result.success {
                                        ui.label(egui::RichText::new(&result.message).small().color(egui::Color32::from_rgb(176, 0, 32)));
                                    } else if let Some(ref deck) = result.deck_identifier {
                                        ui.label(egui::RichText::new(format!("→ {}", deck)).small().color(egui::Color32::from_rgb(100, 149, 237)));
                                    }
                                });
                            }
                        });
                });
                ui.add_space(6.0);
            }

            ui.add_space(4.0);

            // Build info (compact)
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!(
                    "v{}  ({})  {}",
                    env!("CARGO_PKG_VERSION"),
                    env!("GIT_HASH"),
                    env!("GIT_BRANCH")
                )).small().color(egui::Color32::GRAY));
            });
        });
    }

    // ==================== Decks Tab (merged Import + Sync) ====================

    fn render_decks_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(egui::RichText::new(format!("Deck folder: {}", get_deck_directory_display())).weak().small());
        ui.add_space(5.0);

        // URL input section (from Import tab)
        self.render_import_tab(ui, ctx);

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(5.0);

        // Sync section (from Sync tab)
        self.render_sync_tab(ui, ctx);
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
        // Claim the shared scan slot — if the auto-scan path already holds it, don't spawn a
        // second concurrent process_new_logs_with_filter call (that used to cause duplicate
        // overlapping scans and nondeterministic play_session flicker). Our intent to resolve
        // play_session is still recorded, so whichever scan is currently running will honor it.
        if !self.scan_slot.try_begin(true) {
            let mut state = self.gamelog_state.lock().unwrap();
            state.status_message = Some("A scan is already running — it'll finish shortly.".to_string());
            return;
        }

        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let play_session = Arc::clone(&self.play_session);
        let scan_slot = self.scan_slot.clone();
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
        *play_session.lock().unwrap() = PlaySession::Uploading;

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
            let should_resolve = scan_slot.finish();

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

                        if should_resolve {
                            *play_session.lock().unwrap() = if let Some(uploaded) =
                                summary.results.iter().find(|r| r.success)
                            {
                                PlaySession::Uploaded {
                                    deck_id: uploaded.resolved_deck_id.clone(),
                                    filename: uploaded.filename.clone(),
                                }
                            } else if let Some(failed) = summary.results.iter().find(|r| !r.success) {
                                PlaySession::UploadIssue { message: failed.message.clone() }
                            } else {
                                PlaySession::Watching
                            };
                        }

                        state.last_scan_summary = Some(summary);
                    }
                    Err(e) => {
                        state.status_message = Some(format!("Error: {}", e));
                        if should_resolve {
                            *play_session.lock().unwrap() = PlaySession::UploadIssue { message: e.to_string() };
                        }
                    }
                }
            }

            ctx_clone.request_repaint();
        });
    }

    /// Trigger backend re-parse of all parse_failed game logs for the current user.
    fn start_reparse_failed(&mut self, ctx: &egui::Context) {
        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let ctx_clone = ctx.clone();

        {
            let mut state = gamelog_state.lock().unwrap();
            state.is_retrying_failed = true;
            state.reparse_status = None;
        }

        let rt = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            rt.block_on(async move {
                let (api_url, auth_token) = {
                    let settings = settings.lock().unwrap();
                    (
                        settings.gamelog_config.api_url.clone(),
                        settings.gamelog_config.auth_token.clone(),
                    )
                };

                let result = match auth_token {
                    Some(token) => crate::gamelog::reparse_failed_logs(&api_url, &token).await,
                    None => Err(anyhow::anyhow!("No auth token configured")),
                };

                {
                    let mut state = gamelog_state.lock().unwrap();
                    state.is_retrying_failed = false;
                    state.reparse_status = Some(match result {
                        Ok((reparsed, still_failed, total)) => {
                            if total == 0 {
                                "No failed logs found".to_string()
                            } else {
                                format!("Re-parsed {}/{} logs ({} still failed)", reparsed, total, still_failed)
                            }
                        }
                        Err(e) => format!("Error: {}", e),
                    });
                }

                ctx_clone.request_repaint();
            });
        });
    }

    /// Start an automatic gamelog scan (no filters, triggered by Forge process tracking)
    /// `is_final_scan` distinguishes the "Forge just closed" scan (which should drive the Play
    /// tab's timeline through Scanning/Uploading/Uploaded) from the periodic every-5-minutes
    /// scan that runs *while Forge is still open* — that one is incidental housekeeping for a
    /// multi-game sitting, not "the game just ended", so it must not yank the strip away from
    /// Playing while the game is still genuinely in progress.
    fn start_auto_gamelog_scan(&mut self, ctx: &egui::Context, is_final_scan: bool) {
        // Claim the shared scan slot. If a periodic scan already holds it when Forge closes
        // (is_final_scan=true), we don't spawn a second overlapping scan — but `is_final_scan`
        // is still recorded as a pending resolution, so the periodic scan already in flight
        // will resolve play_session with its own results when it finishes, instead of leaving
        // the Play tab's timeline stuck on "Scanning" forever.
        if !self.scan_slot.try_begin(is_final_scan) {
            return;
        }

        let gamelog_state = Arc::clone(&self.gamelog_state);
        let settings = Arc::clone(&self.settings);
        let activity_log = Arc::clone(&self.activity_log);
        let play_session = Arc::clone(&self.play_session);
        let scan_slot = self.scan_slot.clone();
        let ctx_clone = ctx.clone();

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
            let scenario_syncs = if config.auth_token.is_some() {
                crate::gamelog::sync_all_scenario_files(&config).await.ok()
            } else {
                None
            };
            let should_resolve = scan_slot.finish();

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
                        if summary.auth_missing {
                            // Not connected — gentle hint, not a red error
                            if summary.new_files > 0 {
                                if let Ok(mut log) = activity_log.lock() {
                                    log.log_info(format!(
                                        "\u{1F4CB} {} game log(s) waiting — connect your MaMo account in Settings to upload",
                                        summary.new_files
                                    ));
                                }
                            }
                        } else if summary.new_files > 0 || summary.failed_uploads > 0 {
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

                        // Also log synchronized Forge scenarios if any were processed
                        if let Some(syncs) = scenario_syncs {
                            if !syncs.is_empty() {
                                if let Ok(mut log) = activity_log.lock() {
                                    log.log_success(format!(
                                        "\u{1F3AE} Scenario Sync: {} scenario(s) synchronized back to MaMo",
                                        syncs.len()
                                    ));
                                }
                            }
                        }

                        if should_resolve {
                            *play_session.lock().unwrap() = if let Some(uploaded) =
                                summary.results.iter().find(|r| r.success)
                            {
                                PlaySession::Uploaded {
                                    deck_id: uploaded.resolved_deck_id.clone(),
                                    filename: uploaded.filename.clone(),
                                }
                            } else if let Some(failed) = summary.results.iter().find(|r| !r.success) {
                                PlaySession::UploadIssue { message: failed.message.clone() }
                            } else {
                                // Nothing new found this scan — back to idle.
                                PlaySession::Watching
                            };
                        }

                        state.last_scan_summary = Some(summary);
                    }
                    Err(e) => {
                        if let Ok(mut log) = activity_log.lock() {
                            log.log_error(format!("Auto-scan error: {}", e));
                        }
                        if should_resolve {
                            *play_session.lock().unwrap() = PlaySession::UploadIssue { message: e.to_string() };
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

    /// Fetch the saved scenarios for `deck_id` and show them in the Play tab's scenario
    /// picker — mirrors `fetch_my_mamo_decks` above, scoped to one deck.
    fn fetch_scenarios_for_deck(&mut self, deck_id: String, ctx: &egui::Context) {
        let scenario_picker = Arc::clone(&self.scenario_picker);
        let settings = Arc::clone(&self.settings);
        let ctx_clone = ctx.clone();

        {
            let mut picker = scenario_picker.lock().unwrap();
            picker.deck_id = Some(deck_id.clone());
            picker.scenarios.clear();
            picker.is_loading = true;
            picker.error_message = None;
        }

        tokio::spawn(async move {
            let config = {
                let settings = settings.lock().unwrap();
                settings.gamelog_config.clone()
            };

            let result = crate::gamelog::fetch_deck_scenarios(&config, &deck_id).await;

            {
                let mut picker = scenario_picker.lock().unwrap();
                // The user may have selected a different deck while this was in flight —
                // drop a stale response instead of showing scenarios for the wrong deck.
                if picker.deck_id.as_deref() != Some(deck_id.as_str()) {
                    return;
                }
                picker.is_loading = false;

                match result {
                    Ok(scenarios) => {
                        picker.scenarios = scenarios
                            .into_iter()
                            .filter(|s| s.playable_in_forge())
                            .collect();
                    }
                    Err(e) => {
                        picker.error_message = Some(format!("Failed to fetch scenarios: {}", e));
                    }
                }
            }

            ctx_clone.request_repaint();
        });
    }

    /// Renders the "Scenarios" list under the deck picker for whichever deck is selected —
    /// one row per Forge-playable saved scenario, each with its own "▶ Play in Forge" button.
    /// No-op if no deck is selected (nothing to show).
    fn render_scenario_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(deck) = self.selected_account_deck.clone() else {
            return;
        };

        let (is_loading, scenarios, error_message) = {
            let picker = self.scenario_picker.lock().unwrap();
            (picker.is_loading, picker.scenarios.clone(), picker.error_message.clone())
        };

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Scenarios").strong().small());
            if is_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Loading scenarios…").small());
                });
            } else if let Some(err) = error_message {
                ui.label(egui::RichText::new(err).small().color(egui::Color32::from_rgb(176, 0, 32)));
            } else if scenarios.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No Starting Hand / Perfect Game scenarios with a filled opening hand for this deck yet."
                    )
                    .small()
                    .color(egui::Color32::GRAY),
                );
            } else {
                let is_launching = *self.is_launching_selected_deck.lock().unwrap();
                for scenario in &scenarios {
                    ui.horizontal(|ui| {
                        ui.label(&scenario.name);
                        if ui.add_enabled(!is_launching, egui::Button::new("▶ Play in Forge")).clicked() {
                            self.request_forge_launch(
                                PendingForgeLaunch::Scenario {
                                    deck_id: deck.deck_id.clone(),
                                    scenario_id: scenario.id.clone(),
                                    scenario_name: scenario.name.clone(),
                                },
                                ctx,
                            );
                        }
                    });
                }
            }
        });
    }

    // ==================== Setup Tab ====================
    // Journey 1: connect your MaMo account, configure Forge, keep both Forge and the
    // Connector itself up to date. Everything here is account/install management — the rarer,
    // "set it up once and mostly forget it" side of things, as opposed to Play's day-to-day use.

    fn render_setup_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let (forge_path_input, forge_path_valid, has_token, status_message) = {
                let s = self.settings_state.lock().unwrap();
                (s.forge_path_input.clone(), s.forge_path_valid, !s.auth_token_input.is_empty(), s.status_message.clone())
            };

            // ── MaMo account ──────────────────────────────────────────────
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("MaMo account").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if has_token {
                            render_status_pill(ui, "Connected", PillStatus::Success);
                        } else {
                            render_status_pill(ui, "Not connected", PillStatus::Error);
                        }
                    });
                });
                ui.add_space(6.0);

                if has_token {
                    ui.label("Game logs upload automatically, and your MaMo decks show up in Play.");
                    ui.add_space(5.0);
                    if ui.button("Disconnect").clicked() {
                        {
                            let mut state = self.settings_state.lock().unwrap();
                            state.auth_token_input.clear();
                        }
                        self.save_auth_token();
                    }
                } else {
                    ui.label("On the MaMo website, click the profile icon (top-right), then \"Connect Connector\".");
                    if ui.button("🌐 Open MaMo Website")
                        .on_hover_text("Opens MaMo in your browser to retrieve an API token")
                        .clicked()
                    {
                        ctx.output_mut(|o| o.open_url = Some(egui::OpenUrl::new_tab(MAMO_WEBSITE_URL)));
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("Or paste a token directly:").small().weak());
                    ui.horizontal(|ui| {
                        let mut token_input = {
                            let state = self.settings_state.lock().unwrap();
                            state.auth_token_input.clone()
                        };
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut token_input)
                                .desired_width(320.0)
                                .password(true)
                                .hint_text("Paste token here"),
                        );
                        if response.changed() {
                            let mut state = self.settings_state.lock().unwrap();
                            state.auth_token_input = token_input;
                        }
                        if ui.button("Save").clicked() {
                            self.save_auth_token();
                        }
                    });
                }
            });

            ui.add_space(12.0);

            // ── Forge ──────────────────────────────────────────────────────
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Forge").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if forge_path_valid {
                            let (forge_busy, forge_active) = {
                                let s = self.forge_update_check.lock().unwrap();
                                (s.busy, s.staged.is_some())
                            };
                            let downloading = self.forge_update_progress.lock().unwrap().is_some();
                            if downloading || forge_active {
                                render_status_pill(ui, "Updating", PillStatus::Neutral);
                            } else if forge_busy {
                                render_status_pill(ui, "Checking…", PillStatus::Neutral);
                            } else {
                                render_status_pill(ui, "Configured", PillStatus::Success);
                            }
                        } else {
                            render_status_pill(ui, "Not configured", PillStatus::Error);
                        }
                    });
                });
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Path:");
                    let mut path_input = forge_path_input.clone();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut path_input)
                            .desired_width(360.0)
                            .hint_text("Path to forge.exe, .jar, Forge.app, or a Forge directory"),
                    );
                    if response.changed() {
                        let mut state = self.settings_state.lock().unwrap();
                        state.forge_path_input = path_input.clone();
                        state.forge_path_valid = validate_forge_path(&path_input);
                    }
                    if !forge_path_input.is_empty() {
                        if forge_path_valid {
                            ui.label(egui::RichText::new("OK").color(egui::Color32::from_rgb(0, 128, 0)).small().strong());
                        } else {
                            ui.label(egui::RichText::new("Invalid").color(egui::Color32::from_rgb(176, 0, 32)).small().strong());
                        }
                    }
                });
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button("Auto-detect").clicked() {
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
                    if ui.button("Browse…").clicked() {
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
                            state.status_message = Some(if is_valid {
                                format!("Selected: {}", path_str)
                            } else {
                                format!("Warning: {} may not be a valid Forge executable", path_str)
                            });
                        }
                    }
                    if ui.button("Folder…").clicked() {
                        if let Some(folder) = rfd::FileDialog::new()
                            .set_title("Select Forge Directory (e.g. forge-gui-desktop/target/)")
                            .pick_folder()
                        {
                            let path_str = folder.to_string_lossy().to_string();
                            let is_valid = validate_forge_path(&path_str);
                            let mut state = self.settings_state.lock().unwrap();
                            state.forge_path_input = path_str.clone();
                            state.forge_path_valid = is_valid;
                            state.status_message = Some(if is_valid {
                                resolve_latest_forge_jar(&folder)
                                    .map(|jar| format!("Folder OK — will launch: {}", jar.file_name().unwrap_or_default().to_string_lossy()))
                                    .unwrap_or_else(|| "Folder OK".to_string())
                            } else {
                                format!("No forge-gui-desktop JAR found in: {}", path_str)
                            });
                        }
                    }
                    if ui.button("Save").clicked() {
                        self.save_forge_settings();
                    }
                });

                if forge_path_valid {
                    let p = std::path::Path::new(&forge_path_input);
                    if p.is_dir() {
                        if let Some(jar) = resolve_latest_forge_jar(p) {
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new(format!("└─  {}", jar.file_name().unwrap_or_default().to_string_lossy()))
                                    .color(egui::Color32::from_rgb(80, 130, 200))
                                    .small(),
                            );
                        }
                    }
                }

                ui.add_space(5.0);
                let forge_auto_launch = self.settings_state.lock().unwrap().forge_auto_launch;
                let mut auto_launch = forge_auto_launch;
                if ui.checkbox(&mut auto_launch, "Auto-launch Forge after downloading a deck").changed() {
                    self.settings_state.lock().unwrap().forge_auto_launch = auto_launch;
                }

                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(forge_path_valid, egui::Button::new("Test Launch Forge")).clicked() {
                        match launch_forge_from_settings(None, None) {
                            Ok(result) => self.settings_state.lock().unwrap().status_message = Some(result.message),
                            Err(e) => self.settings_state.lock().unwrap().status_message = Some(format!("Launch failed: {}", e)),
                        }
                    }
                    if ui.button("Re-run setup").clicked() {
                        self.wizard.step = WizardStep::Welcome;
                        self.show_setup_wizard = true;
                    }
                });

                // Update status for a Connector-managed Forge install — fully automatic (see the
                // top banner's doc comment), so there's nothing to click here except "Check now"
                // and, on error, dismiss. Stays visible even when idle so "Check now" is always
                // reachable, unlike the top banner which only shows while something's happening.
                if is_connector_managed_forge(&forge_path_input, &forge_download_dir()) {
                    let (forge_busy, forge_staged, forge_update_dismissed) = {
                        let s = self.forge_update_check.lock().unwrap();
                        (s.busy, s.staged.is_some(), s.dismissed)
                    };
                    let forge_update_progress = self.forge_update_progress.lock().unwrap().clone();
                    ui.add_space(8.0);
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgb(205, 232, 255))
                        .inner_margin(egui::Margin::same(8.0))
                        .rounding(6.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if let Some(ref prog) = forge_update_progress {
                                    if let Some(ref err) = prog.error {
                                        if !forge_update_dismissed {
                                            ui.label(egui::RichText::new(format!("MaMo Forge update failed: {err}")).color(egui::Color32::from_rgb(176, 0, 32)));
                                            if ui.small_button("✕").clicked() {
                                                self.forge_update_check.lock().unwrap().dismissed = true;
                                            }
                                        }
                                    } else {
                                        ui.label(egui::RichText::new(format!("Downloading MaMo Forge update… {}", format_download_status(prog.bytes_done, prog.total_bytes))).color(egui::Color32::from_rgb(0, 90, 158)));
                                        if !prog.finished && ui.small_button("Cancel").clicked() {
                                            self.forge_update_cancelled.store(true, Ordering::Relaxed);
                                        }
                                    }
                                } else if forge_staged {
                                    ui.label(egui::RichText::new("Update ready — installs automatically once Forge is closed").color(egui::Color32::from_rgb(0, 90, 158)));
                                } else if forge_busy {
                                    ui.label(egui::RichText::new("Checking for a MaMo Forge update…").color(egui::Color32::GRAY));
                                } else {
                                    ui.label(egui::RichText::new("MaMo Forge is up to date.").color(egui::Color32::from_rgb(0, 128, 0)));
                                    if ui.small_button("Check now").clicked() {
                                        self.trigger_forge_update_check(ctx);
                                    }
                                }
                            });
                        });
                }
            });

            ui.add_space(12.0);

            // ── MaMo Connector itself ────────────────────────────────────
            ui.group(|ui| {
                let (update_ver, staged_path, is_downloading, is_busy, dismissed, update_err) = {
                    let s = self.update_check.lock().unwrap();
                    (
                        s.available_version.clone(),
                        s.staged_path.clone(),
                        s.is_downloading,
                        s.busy,
                        s.dismissed,
                        s.error.clone(),
                    )
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("MaMo Connector").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if staged_path.is_some() {
                            render_status_pill(ui, "Ready to restart", PillStatus::Success);
                        } else if is_downloading {
                            render_status_pill(ui, "Downloading…", PillStatus::Warning);
                        } else if is_busy {
                            render_status_pill(ui, "Checking…", PillStatus::Neutral);
                        } else if update_ver.is_some() && !dismissed {
                            render_status_pill(ui, "Update available", PillStatus::Warning);
                        } else {
                            render_status_pill(ui, "Up to date", PillStatus::Success);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("You're on v{}", env!("CARGO_PKG_VERSION"))).small().weak());
                    if !is_downloading && staged_path.is_none() {
                        if is_busy {
                            ui.spinner();
                        } else if ui.small_button("Check now").clicked() {
                            self.trigger_connector_update_check(ctx);
                        }
                    }
                });

                if let Some(ref err) = update_err {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("Update check failed: {err}")).small().color(egui::Color32::from_rgb(176, 0, 32)));
                }

                if let Some(ref staged) = staged_path {
                    let ver = update_ver.as_deref().unwrap_or("new");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("v{ver} has been downloaded and is ready")).color(egui::Color32::from_rgb(0, 128, 0)));
                        if ui.button("Restart & Apply").clicked() {
                            if let Err(e) = crate::download::apply_connector_update_and_restart(staged) {
                                log::error!("Failed to restart and apply update: {e}");
                            }
                        }
                    });
                } else if is_downloading {
                    let status_text = self
                        .connector_update_progress
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|p| p.status_text.clone())
                        .unwrap_or_else(|| "Downloading…".to_string());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(status_text);
                        if ui.small_button("Cancel").clicked() {
                            self.connector_update_cancelled.store(true, Ordering::SeqCst);
                        }
                    });
                } else if let Some(ref ver) = update_ver {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("v{ver} is available")).color(egui::Color32::from_rgb(133, 100, 4)));
                        if ui.button("Download & Install").clicked() {
                            self.trigger_connector_update_download(ctx);
                        }
                        if ui.small_button("View on GitHub").clicked() {
                            let _ = std::process::Command::new("cmd")
                                .args(["/c", "start", "https://github.com/killriam/mamo-Connector/releases/latest"])
                                .spawn();
                        }
                    });
                }
            });

            ui.add_space(16.0);

            // ── Advanced ──────────────────────────────────────────────────
            ui.collapsing("Advanced", |ui| {
                ui.horizontal(|ui| {
                    if ui.add(
                        egui::Button::new("🔄 Reset to First Run")
                            .fill(egui::Color32::from_rgb(255, 243, 220)),
                    )
                    .on_hover_text("Clears Forge path, MaMo account connection, and saved links, then reopens setup. The mamoConnector:// URL scheme stays registered, so deeplinks from the website keep working.")
                    .clicked()
                    {
                        self.confirm_action = Some(ConfirmAction::ResetFirstRun);
                    }
                    ui.label(egui::RichText::new("Re-run the setup wizard and clear all settings").weak().small());
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add(
                        egui::Button::new("🗑 Uninstall")
                            .fill(egui::Color32::from_rgb(255, 220, 220)),
                    )
                    .on_hover_text("De-registers the URL scheme, deletes all data, then deletes the application itself.")
                    .clicked()
                    {
                        self.confirm_action = Some(ConfirmAction::Uninstall);
                    }
                    ui.label(egui::RichText::new("Remove MaMo Connector from this machine").weak().small());
                });
            });

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
        });
    }

    // ==================== Settings Tab ====================

    fn render_settings_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(egui::RichText::new("⚙ Settings").strong());
        ui.label(egui::RichText::new("MaMo account and Forge configuration moved to the Setup tab — this is everything else.").small().weak());
        ui.add_space(10.0);

        // Get current state
        let (forge_scripts_path_input, status_message) = {
            let state = self.settings_state.lock().unwrap();
            (state.forge_scripts_path_input.clone(), state.status_message.clone())
        };

        // Game Logs directory section (moved from GameLogs tab)
        {
            let (directory_input, directory_valid, file_count) = {
                let state = self.gamelog_state.lock().unwrap();
                (state.directory_input.clone(), state.directory_valid, state.file_count)
            };

            ui.group(|ui| {
                ui.label(egui::RichText::new("📁 Game log folder").strong());
                ui.label(egui::RichText::new("Where Forge writes game logs — Connector watches this while Forge is running.").small().weak());
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    ui.label("Watch Directory:");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.gamelog_state.lock().unwrap().directory_input)
                            .desired_width(400.0)
                            .hint_text("Path to Forge game logs directory")
                    );

                    if response.changed() {
                        let new_path = self.gamelog_state.lock().unwrap().directory_input.clone();
                        let valid = validate_directory(&new_path).unwrap_or(false);
                        let mut state = self.gamelog_state.lock().unwrap();
                        state.directory_valid = valid;
                        state.file_count = None;
                    }

                    if ui.button("Browse...").clicked() {
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

                    if directory_valid {
                        ui.label(egui::RichText::new("✓ Valid").color(egui::Color32::from_rgb(0, 128, 0)));
                        if let Some(count) = file_count {
                            ui.label(format!("({} log files)", count));
                        }
                    } else if !directory_input.is_empty() {
                        ui.label(egui::RichText::new("✗ Invalid or inaccessible").color(egui::Color32::from_rgb(176, 0, 32)));
                    }
                });

                ui.add_space(5.0);

                // Processed files info
                let processed_count = {
                    let state = self.gamelog_state.lock().unwrap();
                    state.processed_files.len()
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Total files processed: {}", processed_count)).small().weak());
                    if ui.small_button("Clear History").clicked() {
                        self.clear_processed_history();
                    }
                });
            });
        }

        ui.add_space(15.0);

        // Deck Mapping section (moved from GameLogs tab)
        self.render_deck_mapping_section(ui, ctx);

        ui.add_space(15.0);
        ui.label(
            egui::RichText::new("Full list of mamoConnector:// links the website can open: see the project docs.")
                .small()
                .weak(),
        );

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

        ui.add_space(20.0);

        // ── Advanced (rare/technical knobs) ────────────────────────────────
        ui.collapsing("Advanced", |ui| {
            ui.group(|ui| {
                ui.label(egui::RichText::new("Simulation scripts").strong());
                ui.label(egui::RichText::new("Optional — only needed for local AI simulation. Path to the folder containing run_commander_simulation.ps1 and analyze_commander_stats.py.").small().weak());
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    ui.label("Scripts folder:");
                    let mut scripts_input = forge_scripts_path_input.clone();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut scripts_input)
                            .desired_width(360.0)
                            .hint_text("Path to folder with .ps1 and .py scripts"),
                    );
                    if response.changed() {
                        let mut state = self.settings_state.lock().unwrap();
                        state.forge_scripts_path_input = scripts_input.clone();
                    }

                    let scripts_valid = !forge_scripts_path_input.is_empty()
                        && std::path::Path::new(&forge_scripts_path_input)
                            .join("run_commander_simulation.ps1")
                            .exists();
                    if !forge_scripts_path_input.is_empty() {
                        if scripts_valid {
                            ui.label(egui::RichText::new("✓").color(egui::Color32::from_rgb(0, 128, 0)));
                        } else {
                            ui.label(egui::RichText::new("✗ ps1 not found").color(egui::Color32::from_rgb(176, 0, 32)));
                        }
                    }
                });

                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button("📂 Browse…").clicked() {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            let path_str = folder.to_string_lossy().to_string();
                            let mut state = self.settings_state.lock().unwrap();
                            state.forge_scripts_path_input = path_str;
                        }
                    }
                    if ui.button("💾 Save").clicked() {
                        self.save_forge_scripts_path();
                    }
                });
            });
        });

        }); // end ScrollArea
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

    fn save_forge_scripts_path(&mut self) {
        let path = {
            let state = self.settings_state.lock().unwrap();
            state.forge_scripts_path_input.clone()
        };
        {
            let mut settings = self.settings.lock().unwrap();
            settings.forge_scripts_path = if path.is_empty() { None } else { Some(path) };
            if let Err(e) = settings.save() {
                let mut state = self.settings_state.lock().unwrap();
                state.status_message = Some(format!("Failed to save scripts path: {}", e));
                return;
            }
        }
        let mut state = self.settings_state.lock().unwrap();
        state.status_message = Some("Scripts path saved!".to_string());
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

#[cfg(test)]
mod deck_picker_tests {
    use super::*;
    use crate::gamelog::UserDeck;

    fn make_deck(name: &str) -> UserDeck {
        UserDeck {
            deck_id: "deck-uuid-1".to_string(),
            deck_name: name.to_string(),
            user_id: "user-1".to_string(),
            color_identity: None,
            commander_id: None,
            commander_partner_id: None,
            updated_at: None,
            created_at: None,
        }
    }

    #[test]
    fn find_local_deck_path_matches_case_insensitive() {
        let deck = make_deck("My Commander Deck");
        let local = vec!["Some Other Deck".to_string(), "my commander deck".to_string()];
        assert_eq!(
            find_local_deck_path(&deck, &local),
            Some("my commander deck".to_string())
        );
    }

    #[test]
    fn find_local_deck_path_none_when_not_downloaded() {
        let deck = make_deck("Never Downloaded Deck");
        let local = vec!["Some Other Deck".to_string()];
        assert_eq!(find_local_deck_path(&deck, &local), None);
    }

    #[test]
    fn deckless_evaluation_detected_for_evaluation_actions_without_deck_id() {
        for action in ["playtest", "launch-forge", "launchforge", "simulate"] {
            assert!(
                is_deckless_evaluation_action(action, false),
                "expected {action} with no deck id to be treated as deck-less"
            );
            assert!(
                !is_deckless_evaluation_action(action, true),
                "expected {action} with a deck id to NOT be treated as deck-less"
            );
        }
    }

    fn make_deeplink(action: &str, deck_id: Option<&str>, params: Vec<(&str, &str)>) -> Deeplink {
        Deeplink {
            raw: format!("mamoConnector://{action}?test"),
            action: action.to_string(),
            params: params.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            token: None,
            doc: None,
            deck_id: deck_id.map(|s| s.to_string()),
            username: None,
        }
    }

    #[test]
    fn deeplink_has_deck_reference_true_for_deck_id() {
        let dl = make_deeplink("launch-forge", Some("some-uuid"), vec![]);
        assert!(deeplink_has_deck_reference(&dl));
    }

    #[test]
    fn deeplink_has_deck_reference_true_for_deck_path_power_user_escape_hatch() {
        // Regression test: launch-forge?deck_path=X&skip_download=true has no deck_id at all,
        // but does specify a deck — it must NOT be routed to the Home tab picker.
        let dl = make_deeplink("launch-forge", None, vec![("deck_path", "Aggro"), ("skip_download", "true")]);
        assert!(deeplink_has_deck_reference(&dl));
        assert!(!is_deckless_evaluation_action(&dl.action, deeplink_has_deck_reference(&dl)));
    }

    #[test]
    fn deeplink_has_deck_reference_false_with_neither() {
        let dl = make_deeplink("launch-forge", None, vec![]);
        assert!(!deeplink_has_deck_reference(&dl));
    }

    #[test]
    fn deckless_evaluation_ignores_unrelated_actions() {
        assert!(!is_deckless_evaluation_action("auth", false));
        assert!(!is_deckless_evaluation_action("replay-game", false));
        assert!(!is_deckless_evaluation_action("download-deck", false));
    }

    #[test]
    fn connector_managed_forge_true_when_path_matches_download_dir() {
        let dir = std::path::PathBuf::from(r"C:\Users\test\AppData\Roaming\MamoConnector\forge");
        let path = dir.to_string_lossy().to_string();
        assert!(is_connector_managed_forge(&path, &dir));
    }

    #[test]
    fn connector_managed_forge_false_for_external_install() {
        let dir = std::path::PathBuf::from(r"C:\Users\test\AppData\Roaming\MamoConnector\forge");
        assert!(!is_connector_managed_forge(r"C:\SWProjects\Forge\forge-gui-desktop\target\forge.exe", &dir));
    }

    #[test]
    fn connector_managed_forge_false_when_empty() {
        let dir = std::path::PathBuf::from(r"C:\Users\test\AppData\Roaming\MamoConnector\forge");
        assert!(!is_connector_managed_forge("", &dir));
    }

    #[test]
    fn play_session_step_index_is_linear_for_the_normal_lifecycle() {
        assert_eq!(play_session_step_index(&PlaySession::Watching), 0);
        assert_eq!(play_session_step_index(&PlaySession::Launching), 1);
        assert_eq!(play_session_step_index(&PlaySession::Playing), 2);
        assert_eq!(play_session_step_index(&PlaySession::Scanning), 3);
        assert_eq!(play_session_step_index(&PlaySession::Uploading), 4);
        assert_eq!(
            play_session_step_index(&PlaySession::Uploaded { deck_id: None, filename: "a.json".to_string() }),
            5
        );
    }

    #[test]
    fn play_session_step_index_upload_issue_lands_on_the_uploading_step() {
        // The failure happened while uploading, so earlier steps (Launching/Playing/Scanning)
        // should still read as done, and Uploaded (index 5) should still read as pending.
        let issue = PlaySession::UploadIssue { message: "network error".to_string() };
        assert_eq!(play_session_step_index(&issue), 4);
    }

    #[test]
    fn play_session_strip_never_returns_an_empty_string() {
        let sessions = [
            PlaySession::Watching,
            PlaySession::Launching,
            PlaySession::Playing,
            PlaySession::Scanning,
            PlaySession::Uploading,
            PlaySession::Uploaded { deck_id: Some("deck-1".to_string()), filename: "a.json".to_string() },
            PlaySession::UploadIssue { message: "oops".to_string() },
        ];
        for ps in &sessions {
            let (text, _) = play_session_strip(ps);
            assert!(!text.is_empty(), "strip text should never be blank — that's the whole point");
        }
    }

    #[test]
    fn play_session_strip_is_active_only_while_something_is_actually_happening() {
        assert!(!play_session_strip(&PlaySession::Watching).1);
        assert!(play_session_strip(&PlaySession::Launching).1);
        assert!(play_session_strip(&PlaySession::Playing).1);
        assert!(play_session_strip(&PlaySession::Scanning).1);
        assert!(play_session_strip(&PlaySession::Uploading).1);
        assert!(!play_session_strip(&PlaySession::Uploaded { deck_id: None, filename: "a.json".to_string() }).1);
        assert!(!play_session_strip(&PlaySession::UploadIssue { message: "oops".to_string() }).1);
    }

    #[test]
    fn deeplink_starts_play_session_only_for_launch_shaped_actions() {
        for action in ["playtest", "launch-forge", "launchforge", "playtest-scenario", "replay-game", "replaygame"] {
            assert!(deeplink_starts_play_session(action), "{action} should start a play session");
        }
        for action in ["auth", "import-user-decks", "list-user-decks", "simulate", "simulate-ai", "unknown"] {
            assert!(!deeplink_starts_play_session(action), "{action} should not start a play session");
        }
    }

    // ── ScanSlot: regression tests for the two race conditions found while assessing
    // confidence in the redesign — a periodic auto-scan racing the Forge-closed final scan,
    // and a manual "Upload Logs" click racing either kind of auto-scan. Both are simulated
    // deterministically here (no real threads/timing needed) since the whole point of
    // ScanSlot is that the outcome doesn't depend on which call happens to finish first.

    #[test]
    fn scan_slot_first_caller_claims_it_second_caller_does_not() {
        let slot = ScanSlot::default();
        assert!(slot.try_begin(true), "first caller should claim the slot");
        assert!(!slot.try_begin(true), "second caller must not claim an already-busy slot");
    }

    #[test]
    fn scan_slot_final_scan_racing_an_in_flight_periodic_scan_is_not_lost() {
        // Reproduces the original bug: a periodic scan (wants_resolution=false) is already
        // running when Forge closes and a final scan (wants_resolution=true) is requested.
        let slot = ScanSlot::default();

        // Periodic scan starts first and claims the slot.
        assert!(slot.try_begin(false), "periodic scan should claim the empty slot");

        // Forge closes; the final-scan request arrives while the periodic scan still holds it.
        assert!(!slot.try_begin(true), "final scan must not start a second overlapping run");

        // The periodic scan (the one actually running) finishes. Even though *it* never asked
        // for a resolution, the final scan's request must still be honored here — this is the
        // fix: play_session gets resolved instead of staying stuck on "Scanning" forever.
        assert!(slot.finish(), "the in-flight scan must report the pending resolution as owed");
    }

    #[test]
    fn scan_slot_manual_click_racing_auto_scan_does_not_spawn_a_second_scan() {
        // Reproduces the second original bug: clicking "Upload Logs" while an auto-scan is
        // already running used to spawn a second concurrent process_new_logs_with_filter call.
        let slot = ScanSlot::default();
        assert!(slot.try_begin(false), "auto-scan claims the slot first");
        assert!(
            !slot.try_begin(true),
            "a manual click while auto-scan is running must not claim its own slot"
        );
        // Only one scan's completion should ever fire — and it still resolves play_session for
        // the manual click that asked for it.
        assert!(slot.finish());
    }

    #[test]
    fn scan_slot_no_resolution_owed_when_nobody_asked_for_one() {
        // A plain periodic scan, uncontested, with no final/manual request racing it: finishing
        // must NOT resolve play_session — that would fight the "still Playing" state a real
        // mid-game housekeeping scan should leave alone.
        let slot = ScanSlot::default();
        assert!(slot.try_begin(false));
        assert!(!slot.finish(), "an uncontested periodic scan should not claim a resolution");
    }

    #[test]
    fn scan_slot_reusable_after_finish() {
        let slot = ScanSlot::default();
        assert!(slot.try_begin(true));
        assert!(slot.finish());
        // The slot must be free again for the next scan, with no resolution wrongly carried over.
        assert!(slot.try_begin(false), "slot should be claimable again after finish()");
        assert!(!slot.finish());
    }

    #[test]
    fn pending_forge_launch_variants_constructible() {
        let plain = PendingForgeLaunch::Plain;
        assert!(matches!(plain, PendingForgeLaunch::Plain));

        let local = PendingForgeLaunch::LocalDeckWithCuratedOpponent {
            local_stem: "deck_abc".to_string(),
        };
        if let PendingForgeLaunch::LocalDeckWithCuratedOpponent { local_stem } = local {
            assert_eq!(local_stem, "deck_abc");
        } else {
            panic!("expected LocalDeckWithCuratedOpponent");
        }

        let scenario = PendingForgeLaunch::Scenario {
            deck_id: "d123".to_string(),
            scenario_id: "s456".to_string(),
            scenario_name: "T1 Fast".to_string(),
        };
        if let PendingForgeLaunch::Scenario { deck_id, scenario_id, scenario_name } = scenario {
            assert_eq!(deck_id, "d123");
            assert_eq!(scenario_id, "s456");
            assert_eq!(scenario_name, "T1 Fast");
        } else {
            panic!("expected Scenario");
        }
    }

    #[test]
    fn prelaunch_update_state_modal_flow() {
        let asset = crate::download::ForgeAsset {
            name: "forge-gui-desktop-2.0.0.jar".to_string(),
            download_url: "https://example.com/forge.jar".to_string(),
            updated_at: "2026-08-19T12:00:00Z".to_string(),
        };

        let prompt_staged = PreLaunchUpdateState::Prompt {
            asset: asset.clone(),
            is_staged: true,
        };
        if let PreLaunchUpdateState::Prompt { is_staged, .. } = prompt_staged {
            assert!(is_staged, "should be marked as staged");
        }

        let prompt_remote = PreLaunchUpdateState::Prompt {
            asset,
            is_staged: false,
        };
        if let PreLaunchUpdateState::Prompt { is_staged, .. } = prompt_remote {
            assert!(!is_staged, "should not be marked as staged");
        }

        let prompt_already_running = PreLaunchUpdateState::AlreadyRunningPrompt;
        assert!(matches!(prompt_already_running, PreLaunchUpdateState::AlreadyRunningPrompt));
    }
}

