//! `mamoConnector://print-proxies/<deckId>?opts=<base64url JSON>[&token=…][&api_url=…]`
//!
//! Asks the MaMo backend to build the proxy / slip-in marker XML for a deck, saves the file(s) into
//! the proxy-printing tool folder and opens the print GUI (`proxy_gui.py`) with them loaded.
//! The `opts` JSON is built by the web app's "Print Proxies & Markers" dialog and is forwarded to
//! the backend untouched (the backend validates it).

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use log::{info, warn};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::commands::{get_parameter, CommandResult};
use crate::deck::MAMO_API_URL;
use crate::deeplink::Deeplink;
use crate::settings::Settings;

/// Outcome of a print-proxies command, shown in the activity log.
#[derive(Debug, Clone)]
pub struct ProxyPrintResult {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct ProxyPrintResponse {
    #[serde(default)]
    files: Vec<ProxyPrintFile>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProxyPrintFile {
    filename: String,
    content: String,
}

/// Decode the `opts` query value (base64url without padding) into the JSON body for the backend.
pub fn decode_opts(raw: &str) -> Result<serde_json::Value> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim_end_matches('='))
        .context("opts is not valid base64url")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("opts is not valid JSON")?;
    if !value.is_object() {
        bail!("opts must be a JSON object");
    }
    Ok(value)
}

/// The deeplink may name a different backend (local dev), but a crafted link must not be able to
/// send the stored auth token to an arbitrary host: only production and localhost are accepted.
pub fn resolve_api_base(requested: Option<&str>) -> String {
    let Some(requested) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return MAMO_API_URL.to_string();
    };
    let allowed = |candidate: &str| -> bool {
        let Ok(url) = url::Url::parse(candidate) else {
            return false;
        };
        let prod_host = url::Url::parse(MAMO_API_URL)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        match (url.scheme(), url.host_str()) {
            ("https", Some(host)) => prod_host.as_deref() == Some(host),
            ("http", Some(host)) => host == "localhost" || host == "127.0.0.1",
            _ => false,
        }
    };
    if allowed(requested) {
        requested.trim_end_matches('/').to_string()
    } else {
        warn!("Ignoring untrusted api_url in print-proxies deeplink: {requested}");
        MAMO_API_URL.to_string()
    }
}

/// Only plain `*.xml` file names are written: no separators, no traversal, no odd characters.
pub fn is_safe_xml_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.to_ascii_lowercase().ends_with(".xml")
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ' | '(' | ')'))
        && !name.contains("..")
}

/// First candidate directory that contains `proxy_gui.py`.
pub fn find_print_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|dir| dir.join("proxy_gui.py").is_file())
        .cloned()
}

fn print_dir_candidates(settings: &Settings) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = settings.proxy_printing_path.as_deref().filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("PROXY_PRINTING_DIR") {
        if !p.is_empty() {
            out.push(PathBuf::from(p));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("proxy-printing"));
            if let Some(parent) = dir.parent() {
                out.push(parent.join("proxy-printing"));
            }
        }
    }
    if let Some(docs) = dirs::document_dir() {
        out.push(docs.join("MaMo").join("proxy-printing"));
    }
    if let Some(local) = dirs::data_local_dir() {
        out.push(local.join("MaMo").join("proxy-printing"));
    }
    out
}

/// Open `proxy_gui.py` with the given XML files, detached from the Connector.
fn launch_gui(print_dir: &Path, xml_files: &[PathBuf]) -> Result<()> {
    // pythonw avoids a console window on Windows; fall back to the other launchers.
    let launchers: &[&str] = if cfg!(windows) {
        &["pythonw", "pyw", "python", "py"]
    } else {
        &["python3", "python"]
    };
    let mut last_err = None;
    for launcher in launchers {
        let mut cmd = Command::new(launcher);
        cmd.arg("proxy_gui.py").args(xml_files).current_dir(print_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(DETACHED_PROCESS);
        }
        match cmd.spawn() {
            Ok(_) => {
                info!("Launched print GUI via {launcher}");
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!(
        "Could not start Python ({}). Install Python 3 and run `pip install -r requirements.txt` in {}",
        last_err.map(|e| e.to_string()).unwrap_or_default(),
        print_dir.display()
    ))
}

pub async fn handle_print_proxies(deeplink: &Deeplink) -> CommandResult {
    let Some(deck_id) = deeplink.deck_id.clone().filter(|s| !s.is_empty()) else {
        return CommandResult::MissingParameters(
            "Deck id is required. Use mamoConnector://print-proxies/DECK_ID?opts=…".to_string(),
        );
    };
    let opts = match get_parameter(&deeplink.params, "opts") {
        Some(raw) => match decode_opts(&raw) {
            Ok(v) => v,
            Err(e) => return CommandResult::Error(format!("Invalid print options: {e:#}")),
        },
        None => serde_json::json!({}),
    };

    let settings = match Settings::load() {
        Ok(s) => s,
        Err(e) => return CommandResult::Error(format!("Failed to load settings: {e:#}")),
    };
    let Some(token) = settings.auth_token.clone().filter(|t| !t.is_empty()) else {
        return CommandResult::Error(
            "Not signed in. Open MaMo in your browser and connect the MaMo Connector first."
                .to_string(),
        );
    };

    // Fail early (before any network call) when there is nowhere to print from.
    let candidates = print_dir_candidates(&settings);
    let Some(print_dir) = find_print_dir(&candidates) else {
        return CommandResult::Error(format!(
            "Print tool not found. Put the proxy-printing folder (with proxy_gui.py) next to the MaMo Connector, \
             or set 'proxy_printing_path' in the Connector settings. Looked in: {}",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    };

    let api_base = resolve_api_base(get_parameter(&deeplink.params, "api_url").as_deref());
    let url = format!("{}/api/deck/{}/proxy-print-xml", api_base, urlencode_segment(&deck_id));
    info!("Requesting proxy print XML from {url}");

    let response = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .json(&opts)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return CommandResult::Error(format!("Could not reach the MaMo backend: {e}")),
    };
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        let hint = match status.as_u16() {
            401 => " (token expired — reconnect the MaMo Connector from the web app)",
            404 => " (deck not found, or it belongs to another account)",
            _ => "",
        };
        return CommandResult::Error(format!(
            "Backend returned {status}{hint}: {}",
            detail.chars().take(200).collect::<String>()
        ));
    }
    let body: ProxyPrintResponse = match response.json().await {
        Ok(b) => b,
        Err(e) => return CommandResult::Error(format!("Unexpected backend response: {e}")),
    };

    if body.files.is_empty() {
        return CommandResult::ProxyPrint(ProxyPrintResult {
            success: false,
            message: body
                .message
                .unwrap_or_else(|| "Nothing to print for this selection.".to_string()),
        });
    }

    let mut written = Vec::new();
    for file in &body.files {
        if !is_safe_xml_filename(&file.filename) {
            return CommandResult::Error(format!(
                "Backend returned an unsafe file name: {:?}",
                file.filename
            ));
        }
        let path = print_dir.join(&file.filename);
        if let Err(e) = std::fs::write(&path, &file.content) {
            return CommandResult::Error(format!("Failed to write {}: {e}", path.display()));
        }
        written.push(path);
    }

    if let Err(e) = launch_gui(&print_dir, &written) {
        return CommandResult::Error(format!("{e:#}"));
    }

    CommandResult::ProxyPrint(ProxyPrintResult {
        success: true,
        message: format!(
            "Opened the print tool with {} file(s) from {}",
            written.len(),
            print_dir.display()
        ),
    })
}

/// Minimal path-segment escaping for the deck id (ids are UUIDs; anything else is escaped).
fn urlencode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    }

    #[test]
    fn decodes_opts_json_object() {
        let v = decode_opts(&b64(r#"{"scope":"all","markerSize":"medium"}"#)).unwrap();
        assert_eq!(v["markerSize"], "medium");
    }

    #[test]
    fn rejects_bad_opts() {
        assert!(decode_opts("!!!").is_err());
        assert!(decode_opts(&b64("not json")).is_err());
        assert!(decode_opts(&b64("[1,2]")).is_err());
    }

    #[test]
    fn api_base_is_allowlisted() {
        assert_eq!(resolve_api_base(None), MAMO_API_URL);
        assert_eq!(resolve_api_base(Some("https://evil.example")), MAMO_API_URL);
        assert_eq!(resolve_api_base(Some("http://evil.example")), MAMO_API_URL);
        assert_eq!(resolve_api_base(Some("ftp://localhost")), MAMO_API_URL);
        assert_eq!(resolve_api_base(Some(MAMO_API_URL)), MAMO_API_URL);
        assert_eq!(
            resolve_api_base(Some("http://localhost:3001/")),
            "http://localhost:3001"
        );
        assert_eq!(
            resolve_api_base(Some("http://127.0.0.1:3001")),
            "http://127.0.0.1:3001"
        );
    }

    #[test]
    fn filename_validation() {
        assert!(is_safe_xml_filename("My_Deck_2026-10-25_missing_proxy.xml"));
        assert!(is_safe_xml_filename("My Deck (2)_missing_markers.xml"));
        assert!(!is_safe_xml_filename("../evil.xml"));
        assert!(!is_safe_xml_filename("a/b.xml"));
        assert!(!is_safe_xml_filename("a\\b.xml"));
        assert!(!is_safe_xml_filename("deck.exe"));
        assert!(!is_safe_xml_filename(".hidden.xml"));
        assert!(!is_safe_xml_filename("a..b.xml"));
        assert!(!is_safe_xml_filename(""));
    }

    #[test]
    fn finds_first_dir_with_gui() {
        let base = std::env::temp_dir().join(format!("mamo_print_test_{}", std::process::id()));
        let empty = base.join("empty");
        let good = base.join("good");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("proxy_gui.py"), "").unwrap();
        assert_eq!(find_print_dir(&[empty.clone(), good.clone()]), Some(good));
        assert_eq!(find_print_dir(&[empty]), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn escapes_deck_id_segment() {
        assert_eq!(urlencode_segment("abc-123"), "abc-123");
        assert_eq!(urlencode_segment("a/b c"), "a%2Fb%20c");
    }
}
