//! Commander AI Simulation runner and analyser
//!
//! Orchestrates the full simulation workflow:
//!   1. Run `run_commander_simulation.ps1` (Forge headless batch)
//!   2. Run `analyze_commander_stats.py` (aggregates per-game JSON stats)
//!   3. Read the resulting `commander_simulation_report.json`
//!   4. POST the report to the MaMo backend
//!
//! Scripts must be present in `Settings.forge_scripts_path`.

use anyhow::{Context, Result, anyhow};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use crate::settings::Settings;

pub const MAMO_API_BASE: &str = "https://new-backend-two-eosin.vercel.app";

// ==================== Public types ====================

/// Configuration for a single simulation run
#[derive(Debug, Clone)]
pub struct SimulationConfig {
    /// Deck stem name (without `.dck`) used as -Deck1
    pub deck1_name: String,
    /// Optional opponent deck stem — defaults to mirror match
    pub deck2_name: Option<String>,
    /// Number of games (default 100)
    pub games: u32,
    /// Per-game timeout in seconds (default 180)
    pub timeout_secs: u32,
}

/// Result returned after a complete simulation + analysis run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub success: bool,
    pub message: String,
    /// Parsed report JSON (matches `commander-simulation-report` format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<serde_json::Value>,
}

impl SimulationResult {
    pub fn success(report: serde_json::Value) -> Self {
        Self {
            success: true,
            message: "Simulation completed and report uploaded.".into(),
            report: Some(report),
        }
    }

    pub fn failure(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            message: msg.into(),
            report: None,
        }
    }
}

// ==================== Main entry point ====================

/// Run the full simulation pipeline for a deck identified by its MaMo deck ID.
///
/// Steps:
///   1. Resolve scripts directory from settings
///   2. Run PowerShell simulation script
///   3. Run Python analysis script
///   4. Read the JSON report
///   5. POST report to backend
pub async fn run_simulation_for_deck(
    deck_id: &str,
    deck_name: &str,
    log: &dyn Fn(&str),
) -> SimulationResult {
    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => return SimulationResult::failure(format!("Failed to load settings: {}", e)),
    };

    let scripts_dir = match resolve_scripts_dir(&settings) {
        Some(d) => d,
        None => {
            return SimulationResult::failure(
                "Forge scripts path not configured. Set 'forge_scripts_path' in Settings.",
            )
        }
    };

    let config = SimulationConfig {
        // deck_name already comes from the actual saved .dck file stem — do NOT sanitize it,
        // as that would produce a different name than the file on disk.
        deck1_name: deck_name.to_string(),
        deck2_name: settings.simulation_opponent_deck.clone(),
        games: settings.simulation_games,
        timeout_secs: 180,
    };

    log(&format!(
        "Starting simulation: {} games of '{}' vs '{}'",
        config.games,
        config.deck1_name,
        config.deck2_name.as_deref().unwrap_or("mirror match")
    ));

    // Step 1: Run simulation scripts
    if let Err(e) = run_simulation_script(&config, &scripts_dir, log).await {
        return SimulationResult::failure(format!("Simulation script failed: {}", e));
    }

    // Step 2: Analyse stats
    let report_path = scripts_dir.join("commander_simulation_report.json");
    let limit = if config.games == 0 { 5 } else { config.games };
    if let Err(e) = run_analysis_script(&scripts_dir, &report_path, limit, log).await {
        return SimulationResult::failure(format!("Analysis script failed: {}", e));
    }

    // Step 3: Read report and annotate with deck names so the frontend can
    // label which player is the deck under test vs. the opponent.
    let mut report = match read_report(&report_path) {
        Ok(r) => r,
        Err(e) => return SimulationResult::failure(format!("Failed to read report: {}", e)),
    };

    // Inject deck_names and deck_under_test into meta (P1 = deck under test, P2 = opponent).
    if let Some(meta) = report.get_mut("meta").and_then(|m| m.as_object_mut()) {
        let deck2_label = config
            .deck2_name
            .clone()
            .unwrap_or_else(|| config.deck1_name.clone());
        meta.insert(
            "deck_names".to_string(),
            serde_json::json!({ "P1": config.deck1_name, "P2": deck2_label }),
        );
        meta.insert(
            "deck_under_test".to_string(),
            serde_json::Value::String("P1".to_string()),
        );
    }

    log("Uploading simulation report to MaMo backend…");

    // Step 4: POST to backend
    let auth_token = settings.auth_token.clone();
    if let Err(e) = post_simulation_report(deck_id, &report, auth_token.as_deref()).await {
        warn!("Failed to upload simulation report: {}", e);
        // Don't fail — the report was generated, just not uploaded
        return SimulationResult {
            success: true,
            message: format!(
                "Simulation complete, but upload failed: {}. Report saved locally at {}",
                e,
                report_path.display()
            ),
            report: Some(report),
        };
    }

    log("Simulation report uploaded.");
    SimulationResult::success(report)
}

// ==================== Script runners ====================

/// Run `run_commander_simulation.ps1` and wait for completion.
async fn run_simulation_script(
    config: &SimulationConfig,
    scripts_dir: &Path,
    log: &dyn Fn(&str),
) -> Result<()> {
    let script = scripts_dir.join("run_commander_simulation.ps1");
    if !script.exists() {
        return Err(anyhow!(
            "Simulation script not found: {}",
            script.display()
        ));
    }

    let mut args = vec![
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-File".to_string(),
        script.to_string_lossy().to_string(),
        "-Deck1".to_string(),
        config.deck1_name.clone(),
        "-Games".to_string(),
        config.games.to_string(),
        "-Timeout".to_string(),
        config.timeout_secs.to_string(),
        "-Quiet".to_string(),
    ];

    if let Some(deck2) = &config.deck2_name {
        args.push("-Deck2".to_string());
        args.push(deck2.clone());
    }

    info!("Running simulation script: powershell {}", args.join(" "));
    log(&format!(
        "Running {} games… (this may take several minutes)",
        config.games
    ));

    let mut cmd = TokioCommand::new("powershell");
    cmd.args(&args)
        .stdin(Stdio::null()) // prevent any interactive prompt from blocking
        .current_dir(scripts_dir);

    // If Java 17+ is detected, ensure PATH and JAVA_HOME point to it so PowerShell runs the modern runtime
    if let crate::forge::JavaStatus::Ok { path, .. } = crate::forge::detect_java() {
        if let Some(bin_dir) = path.parent() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            let new_path = format!("{};{}", bin_dir.display(), current_path);
            cmd.env("PATH", new_path);

            if let Some(java_home) = bin_dir.parent() {
                cmd.env("JAVA_HOME", java_home);
            }
        }
    }

    let output = cmd
        .output()
        .await
        .context("Failed to spawn PowerShell simulation script")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    for line in stdout.lines() {
        info!("[sim] {}", line);
    }
    if !stderr.is_empty() {
        warn!("[sim stderr] {}", stderr.trim());
    }

    if !output.status.success() {
        return Err(anyhow!(
            "Simulation script exited with status {:?}. stderr: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    log("Simulation complete. Running analysis…");
    Ok(())
}

/// Run `analyze_commander_stats.py` and wait for completion.
async fn run_analysis_script(
    scripts_dir: &Path,
    output_path: &Path,
    limit: u32,
    log: &dyn Fn(&str),
) -> Result<()> {
    let script = scripts_dir.join("analyze_commander_stats.py");
    if !script.exists() {
        return Err(anyhow!(
            "Analysis script not found: {}",
            script.display()
        ));
    }

    // Stats are written by Forge to %APPDATA%\Forge\games\simulation_stats\
    let stats_dir = get_forge_simulation_stats_dir();

    info!(
        "Running analysis: python {} {} {}",
        script.display(),
        stats_dir.display(),
        output_path.display()
    );
    log("Analysing simulation stats…");

    let output = TokioCommand::new("python")
        .arg(&script)
        .arg("--limit")
        .arg(limit.to_string())
        .arg(&stats_dir)
        .arg(output_path)
        .env("PYTHONUTF8", "1") // force UTF-8 I/O on Windows (avoids cp1252 UnicodeEncodeError)
        .stdin(Stdio::null()) // ensure isatty() returns False → no interactive prompt
        .current_dir(scripts_dir)
        .output()
        .await
        .context("Failed to spawn Python analysis script")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    for line in stdout.lines() {
        info!("[analysis] {}", line);
    }
    if !stderr.is_empty() {
        warn!("[analysis stderr] {}", stderr.trim());
    }

    if !output.status.success() {
        return Err(anyhow!(
            "Analysis script exited with status {:?}. stderr: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    if !output_path.exists() {
        return Err(anyhow!(
            "Analysis script completed but report file was not created: {}",
            output_path.display()
        ));
    }

    Ok(())
}

// ==================== Report I/O ====================

fn read_report(path: &Path) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read report at {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).context("Failed to parse simulation report JSON")?;
    Ok(value)
}

/// POST simulation report to `POST /api/simulation-report/:deckId`
pub async fn post_simulation_report(
    deck_id: &str,
    report: &serde_json::Value,
    auth_token: Option<&str>,
) -> Result<()> {
    let url = format!("{}/api/simulation-report/{}", MAMO_API_BASE, deck_id);

    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(report);

    if let Some(token) = auth_token {
        req = req.bearer_auth(token);
    }

    let response = req
        .send()
        .await
        .context("Failed to POST simulation report")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Backend returned {} when uploading report: {}",
            status,
            body
        ));
    }

    info!("Simulation report uploaded for deck {}", deck_id);
    Ok(())
}

// ==================== Helpers ====================

/// Resolve the Forge scripts directory from settings.
/// Falls back to sibling `forge` directory of the configured `forge_path`.
fn resolve_scripts_dir(settings: &Settings) -> Option<PathBuf> {
    if let Some(ref p) = settings.forge_scripts_path {
        if !p.is_empty() {
            let path = PathBuf::from(p);
            if path.exists() {
                return Some(path);
            }
            warn!(
                "forge_scripts_path '{}' does not exist, trying fallback",
                p
            );
        }
    }

    // Fallback: try <forge_path parent>/forge (common dev layout)
    if let Some(ref fp) = settings.forge_path {
        let forge = PathBuf::from(fp);
        // Walk up until we find run_commander_simulation.ps1
        let candidates = [
            forge.parent().map(|p| p.join("forge")),
            forge.parent().and_then(|p| p.parent()).map(|p| p.join("forge")),
            Some(PathBuf::from("C:\\SWProjects\\Forge")),
        ];
        for candidate in candidates.into_iter().flatten() {
            if candidate.join("run_commander_simulation.ps1").exists() {
                info!("Resolved scripts dir via fallback: {}", candidate.display());
                return Some(candidate);
            }
        }
    }

    error!("Could not resolve Forge scripts directory");
    None
}

/// Return the directory where Forge writes Commander replay gamelogs.
/// On Windows: `%APPDATA%\Forge\games\gamelogs`
/// Pattern: `replay_Commander_*.json`
fn get_forge_simulation_stats_dir() -> PathBuf {
    #[cfg(windows)]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Forge")
            .join("games")
            .join("gamelogs")
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".forge")
            .join("games")
            .join("gamelogs")
    }
}

/// Sanitize a deck name to match the `.dck` file stem Forge uses.
/// Spaces and special characters become underscores.
#[allow(dead_code)]
pub fn sanitize_deck_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ==================== Headless replay verification ====================

/// Result of a single headless `sim -r <replay>` verification run.
///
/// This does NOT verify that the game reproduces the original recorded outcome — `sim` mode
/// pilots BOTH seats with fresh AI decisions from the reordered opening library (there is no
/// human seat in headless mode), so a different winner than the original replay is expected
/// and is not a failure. What it verifies is structural: the replay JSON parses, both decks
/// load, and Forge's engine can play the reconstructed game to completion (or a clean draw)
/// without crashing — the thing the interactive `replay <path>` GUI path can never confirm in
/// an automated run, since it always launches a window and can block on operator-only dialogs
/// ("Already Replayed", AI-deck-compatibility warnings) that `sim` mode never shows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayVerificationResult {
    pub success: bool,
    pub is_draw: bool,
    pub winner: Option<String>,
    pub duration_ms: Option<u64>,
    pub message: String,
}

/// Run `java -jar <forge.jar> sim -d <deck1> <deck2> -n 1 -f commander -c <timeout> -r <replay_json>`
/// and parse its "Game Result" line.
///
/// `deck1_name`/`deck2_name` are deck stems (no `.dck`, no path) as Forge's own deck storage
/// resolves them — same convention `run_simulation_for_deck` already uses for `-d`. Callers are
/// responsible for confirming both decks actually exist locally before calling this; a missing
/// deck surfaces as a normal Forge "Could not load deck" failure in `stdout`/`stderr` here rather
/// than as a distinct error path.
pub async fn run_replay_verification(
    deck1_name: &str,
    deck2_name: &str,
    replay_json_path: &Path,
    timeout_secs: u32,
    log: &dyn Fn(&str),
) -> Result<ReplayVerificationResult> {
    let settings = Settings::load()?;
    let forge_path = settings
        .forge_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow!("Forge path not configured. Set it in the Setup tab."))?;

    let forge_path_buf = PathBuf::from(forge_path);
    let jar_path = if forge_path_buf.is_dir() {
        crate::forge::resolve_latest_forge_jar(&forge_path_buf)
            .ok_or_else(|| anyhow!("No forge-gui-desktop JAR found in: {}", forge_path_buf.display()))?
    } else {
        forge_path_buf
    };

    log(&format!(
        "Verifying replay headlessly: '{}' vs '{}' ({})",
        deck1_name,
        deck2_name,
        jar_path.display()
    ));

    let mut cmd = TokioCommand::new(crate::forge::resolve_java_command());
    cmd.arg("-Xmx4096m")
        .arg("-Dfile.encoding=UTF-8")
        .arg("-jar")
        .arg(&jar_path)
        .arg("sim")
        .arg("-d")
        .arg(deck1_name)
        .arg(deck2_name)
        .arg("-n")
        .arg("1")
        .arg("-f")
        .arg("commander")
        .arg("-c")
        .arg(timeout_secs.to_string())
        .arg("-r")
        .arg(replay_json_path)
        .stdin(Stdio::null());

    if let Some(dir) = jar_path.parent() {
        cmd.current_dir(dir);
    }

    let output = cmd
        .output()
        .await
        .context("Failed to spawn headless Forge verification process")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    for line in stdout.lines() {
        info!("[verify-replay] {}", line);
    }
    if !stderr.trim().is_empty() {
        warn!("[verify-replay stderr] {}", stderr.trim());
    }

    if !output.status.success() {
        let msg = format!(
            "Verification failed: Forge exited with status {:?} before completing the game. stderr: {}",
            output.status.code(),
            stderr.trim()
        );
        log(&msg);
        return Ok(ReplayVerificationResult {
            success: false,
            is_draw: false,
            winner: None,
            duration_ms: None,
            message: msg,
        });
    }

    match parse_game_result_line(&stdout) {
        Some(result) => {
            log(&result.message);
            Ok(result)
        }
        None => {
            let msg = "Verification failed: Forge exited cleanly but no 'Game Result' line was \
                found in its output — the game may not have started (check the deck names resolved)."
                .to_string();
            log(&msg);
            Ok(ReplayVerificationResult {
                success: false,
                is_draw: false,
                winner: None,
                duration_ms: None,
                message: msg,
            })
        }
    }
}

/// Parse Forge's `SimulateMatch` stdout for its "Game Result" line. Pure function, no I/O — see
/// `SimulateMatch.java` (forge-gui-desktop) for the exact formats this mirrors:
///   "Game Result: Game %d ended in a Draw! Took %d ms."
///   "Game Result: Game %d ended in %d ms. %s has won!"
fn parse_game_result_line(stdout: &str) -> Option<ReplayVerificationResult> {
    let line = stdout.lines().find(|l| l.contains("Game Result:"))?;

    if let Some(rest) = line.split("ended in a Draw! Took ").nth(1) {
        let ms: u64 = rest.trim().trim_end_matches("ms.").trim().parse().ok()?;
        return Some(ReplayVerificationResult {
            success: true,
            is_draw: true,
            winner: None,
            duration_ms: Some(ms),
            message: format!("Replay verified: game ended in a draw after {} ms.", ms),
        });
    }

    // "... ended in <ms> ms. <winner> has won!"
    let after_ended = line.split("ended in ").nth(1)?;
    let (ms_part, rest) = after_ended.split_once(" ms. ")?;
    let winner = rest.trim().trim_end_matches(" has won!").to_string();
    if winner.is_empty() {
        return None;
    }
    let ms: u64 = ms_part.trim().parse().ok()?;

    Some(ReplayVerificationResult {
        success: true,
        is_draw: false,
        winner: Some(winner.clone()),
        duration_ms: Some(ms),
        message: format!("Replay verified: game completed in {} ms, {} won.", ms, winner),
    })
}

#[cfg(test)]
mod replay_verification_tests {
    use super::*;

    #[test]
    fn parses_win_line() {
        let stdout = "Simulation mode\nReplay mode enabled: C:\\replay.json\n\
            \nGame Result: Game 1 ended in 4567 ms. Ai(2)-Edgar Markov Aggro 5.0 has won!\n";
        let result = parse_game_result_line(stdout).expect("should parse a win line");
        assert!(result.success);
        assert!(!result.is_draw);
        assert_eq!(result.winner.as_deref(), Some("Ai(2)-Edgar Markov Aggro 5.0"));
        assert_eq!(result.duration_ms, Some(4567));
    }

    #[test]
    fn parses_draw_line() {
        let stdout = "\nGame Result: Game 1 ended in a Draw! Took 1234 ms.\n";
        let result = parse_game_result_line(stdout).expect("should parse a draw line");
        assert!(result.success);
        assert!(result.is_draw);
        assert_eq!(result.winner, None);
        assert_eq!(result.duration_ms, Some(1234));
    }

    #[test]
    fn returns_none_when_no_game_result_line_present() {
        let stdout = "Simulation mode\nCould not load deck - Foo, match cannot start\n";
        assert!(parse_game_result_line(stdout).is_none());
    }

    #[test]
    fn returns_none_on_malformed_result_line() {
        let stdout = "Game Result: Game 1 ended weirdly\n";
        assert!(parse_game_result_line(stdout).is_none());
    }
}
