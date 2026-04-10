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
use tokio::process::Command as TokioCommand;

use crate::settings::Settings;

const MAMO_API_BASE: &str = "https://new-backend-two-eosin.vercel.app";

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
        deck1_name: sanitize_deck_name(deck_name),
        deck2_name: None, // mirror match by default
        games: settings.simulation_games,
        timeout_secs: 180,
    };

    log(&format!(
        "Starting simulation: {} games of '{}' (mirror match)",
        config.games, config.deck1_name
    ));

    // Step 1: Run simulation scripts
    if let Err(e) = run_simulation_script(&config, &scripts_dir, log).await {
        return SimulationResult::failure(format!("Simulation script failed: {}", e));
    }

    // Step 2: Analyse stats
    let report_path = scripts_dir.join("commander_simulation_report.json");
    if let Err(e) = run_analysis_script(&scripts_dir, &report_path, log).await {
        return SimulationResult::failure(format!("Analysis script failed: {}", e));
    }

    // Step 3: Read report
    let report = match read_report(&report_path) {
        Ok(r) => r,
        Err(e) => return SimulationResult::failure(format!("Failed to read report: {}", e)),
    };

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

    let output = TokioCommand::new("powershell")
        .args(&args)
        .current_dir(scripts_dir)
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
        .arg(&stats_dir)
        .arg(output_path)
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
