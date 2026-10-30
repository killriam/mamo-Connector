use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::gamelog::GameLogConfig;
use crate::secrets::{self, SecretStore, ACCOUNT_API_TOKEN, ACCOUNT_GAMELOG_TOKEN};

/// Type of saved link for synchronization
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedLinkType {
    MoxfieldDeck,      // Single Moxfield deck
    MoxfieldUser,      // All decks from a Moxfield user
    ArchidektDeck,     // Single Archidekt deck
    DeckstatsDeck,     // Single Deckstats deck
    MamoDeck,          // Single MaMo deck
}

impl SavedLinkType {
    pub fn display_name(&self) -> &'static str {
        match self {
            SavedLinkType::MoxfieldDeck => "Moxfield Deck",
            SavedLinkType::MoxfieldUser => "Moxfield User",
            SavedLinkType::ArchidektDeck => "Archidekt Deck",
            SavedLinkType::DeckstatsDeck => "Deckstats Deck",
            SavedLinkType::MamoDeck => "MaMo Deck",
        }
    }
}

/// A saved link for deck synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedLink {
    /// Unique ID for this saved link
    pub id: String,
    /// Display name for this link (user-editable)
    pub name: String,
    /// The type of link
    pub link_type: SavedLinkType,
    /// The URL or identifier (deck ID, username, etc.)
    pub url: String,
    /// For Deckstats: owner ID (stored separately)
    #[serde(default)]
    pub owner_id: Option<String>,
    /// When this link was added
    pub added_at: String,
    /// When this was last synced
    #[serde(default)]
    pub last_synced: Option<String>,
    /// Whether this link is enabled for sync
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

fn default_simulation_games() -> u32 {
    100
}

fn default_simulation_opponent_deck() -> Option<String> {
    Some("killriam - dummy defender (2026-04-09)".to_string())
}

impl SavedLink {
    /// Create a new saved link
    pub fn new(name: String, link_type: SavedLinkType, url: String) -> Self {
        Self {
            id: uuid_v4(),
            name,
            link_type,
            url,
            owner_id: None,
            added_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            last_synced: None,
            enabled: true,
        }
    }

    /// Create a new saved link for Deckstats (requires owner_id)
    pub fn new_deckstats(name: String, owner_id: String, deck_id: String) -> Self {
        Self {
            id: uuid_v4(),
            name,
            link_type: SavedLinkType::DeckstatsDeck,
            url: deck_id,
            owner_id: Some(owner_id),
            added_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            last_synced: None,
            enabled: true,
        }
    }

    /// Mark this link as synced
    pub fn mark_synced(&mut self) {
        self.last_synced = Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
    }
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// List of saved links for synchronization
    #[serde(default)]
    pub saved_links: Vec<SavedLink>,
    /// Path to Forge executable
    #[serde(default)]
    pub forge_path: Option<String>,
    /// Whether to auto-launch Forge after downloading deck
    #[serde(default = "default_true")]
    pub forge_auto_launch: bool,
    /// Auto-sync on startup
    #[serde(default)]
    pub auto_sync_on_startup: bool,
    /// Game log reader configuration
    #[serde(default)]
    pub gamelog_config: GameLogConfig,
    /// Authentication token for MaMo API. In memory this is the live value; on disk it lives in
    /// the OS credential store, and the settings file only holds it if the store refused it.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Moxfield API Bearer token (for syncing user decks — copy from browser dev tools)
    #[serde(default)]
    pub moxfield_auth_token: Option<String>,
    /// Path to the Forge scripts directory (contains run_commander_simulation.ps1 etc.)
    #[serde(default)]
    pub forge_scripts_path: Option<String>,
    /// Default number of games to run in AI simulations
    #[serde(default = "default_simulation_games")]
    pub simulation_games: u32,
    /// Standard opponent deck for AI simulations (deck stem without .dck).
    /// Defaults to the bundled dummy defender deck.
    #[serde(default = "default_simulation_opponent_deck")]
    pub simulation_opponent_deck: Option<String>,
    /// Whether to backward-import and synchronize decks as Reference Decks in MaMo
    #[serde(default = "default_true")]
    pub sync_as_reference_decks: bool,
    /// The last MaMo account deck picked in the Play tab's "Your decks" selector, so a
    /// relaunch doesn't forget it. `None` for "— none —" explicitly picked, or never set.
    #[serde(default)]
    pub last_selected_deck_id: Option<String>,
    /// Folder containing the proxy-printing tool (`proxy_gui.py`), used by the print-proxies
    /// deeplink. `None` = look next to the Connector and in Documents\MaMo.
    #[serde(default)]
    pub proxy_printing_path: Option<String>,
    /// Non-secret facts about the MCP tokens stored in the credential store (service "MaMo MCP");
    /// the tokens themselves never live in this file. See agent_access.rs.
    #[serde(default)]
    pub agent_access: crate::agent_access::AgentAccessSettings,
    /// Set when the credential store could not be read at load time. `save` then leaves the
    /// stored tokens alone, so a transient failure cannot wipe them.
    #[serde(skip)]
    secrets_unreadable: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            saved_links: Vec::new(),
            forge_path: None,
            forge_auto_launch: true,
            auto_sync_on_startup: false,
            gamelog_config: GameLogConfig::default(),
            auth_token: None,
            moxfield_auth_token: None,
            forge_scripts_path: None,
            simulation_games: default_simulation_games(),
            simulation_opponent_deck: default_simulation_opponent_deck(),
            sync_as_reference_decks: true,
            last_selected_deck_id: None,
            proxy_printing_path: None,
            agent_access: Default::default(),
            secrets_unreadable: false,
        }
    }
}

/// Fills `slot` from the credential store. A token still sitting in the settings file (older
/// versions, or a store that refused an earlier write) is moved into the store instead.
/// Returns true when a plaintext token was moved, i.e. the file should be rewritten.
fn hydrate_slot(slot: &mut Option<String>, account: &str, store: &dyn SecretStore, unreadable: &mut bool) -> bool {
    if let Some(token) = slot.as_deref().filter(|t| !t.is_empty()) {
        return match store.set(account, token) {
            Ok(()) => true,
            Err(err) => {
                warn!("Could not move '{}' into the credential store, keeping it in the settings file: {}", account, err);
                false
            }
        };
    }
    match store.get(account) {
        Ok(stored) => *slot = stored,
        Err(err) => {
            warn!("Could not read '{}' from the credential store: {}", account, err);
            *unreadable = true;
        }
    }
    false
}

/// Writes `slot` to the credential store and blanks it for the settings file. A token the store
/// refuses stays in `slot`, so it is saved to the file rather than lost. An empty slot deletes
/// the stored secret (a deliberate sign-out) unless the store was unreadable at load time.
fn persist_slot(slot: &mut Option<String>, account: &str, store: &dyn SecretStore, unreadable: bool) {
    match slot.as_deref().filter(|t| !t.is_empty()) {
        Some(token) => match store.set(account, token) {
            Ok(()) => *slot = None,
            Err(err) => warn!("Could not store '{}' in the credential store, keeping it in the settings file: {}", account, err),
        },
        None => {
            *slot = None;
            if !unreadable {
                if let Err(err) = store.delete(account) {
                    warn!("Could not delete '{}' from the credential store: {}", account, err);
                }
            }
        }
    }
}

impl Settings {
    /// Fills the in-memory tokens from `store`; returns true when plaintext tokens from the
    /// settings file were moved into it and the file needs rewriting.
    fn hydrate_secrets(&mut self, store: &dyn SecretStore) -> bool {
        let mut unreadable = false;
        let api = hydrate_slot(&mut self.auth_token, ACCOUNT_API_TOKEN, store, &mut unreadable);
        let gamelog = hydrate_slot(&mut self.gamelog_config.auth_token, ACCOUNT_GAMELOG_TOKEN, store, &mut unreadable);
        self.secrets_unreadable = unreadable;
        api || gamelog
    }

    /// The copy that is serialized to disk: tokens the store accepted are blanked.
    fn persisted_copy(&self, store: &dyn SecretStore) -> Settings {
        let mut copy = self.clone();
        persist_slot(&mut copy.auth_token, ACCOUNT_API_TOKEN, store, self.secrets_unreadable);
        persist_slot(&mut copy.gamelog_config.auth_token, ACCOUNT_GAMELOG_TOKEN, store, self.secrets_unreadable);
        copy
    }

    /// Load settings from file, or create default if not exists
    // ponytail: every load reads the credential store (two lookups); cache the tokens if load()
    // ever ends up in a hot loop.
    pub fn load() -> Result<Self> {
        let path = get_settings_path()?;

        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read settings from {:?}", path))?;
            let mut settings: Settings = serde_json::from_str(&content)
                .with_context(|| "Failed to parse settings JSON")?;
            info!("Loaded settings with {} saved links", settings.saved_links.len());

            // Move plaintext tokens from older settings files into the credential store.
            if settings.hydrate_secrets(secrets::default_store().as_ref()) {
                info!("Moved the MaMo API tokens from the settings file into the credential store");
                let _ = settings.save();
            }

            // Migrate: ensure file_extensions uses JSON-only format
            // Old settings may have ["txt", "log"] from before the JSON-only change
            let has_json = settings.gamelog_config.file_extensions.iter().any(|e| e == "json");
            if !has_json {
                info!("Migrating gamelog file_extensions to JSON-only format");
                settings.gamelog_config.file_extensions = vec!["json".to_string()];
                // Persist the migration
                let _ = settings.save();
            }
            
            Ok(settings)
        } else {
            info!("No settings file found, using defaults");
            let mut settings = Settings::default();
            settings.hydrate_secrets(secrets::default_store().as_ref());
            Ok(settings)
        }
    }

    /// Save settings to file
    pub fn save(&self) -> Result<()> {
        let path = get_settings_path()?;
        
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create settings directory {:?}", parent))?;
            }
        }

        let content = serde_json::to_string_pretty(&self.persisted_copy(secrets::default_store().as_ref()))
            .with_context(|| "Failed to serialize settings")?;
        
        fs::write(&path, content)
            .with_context(|| format!("Failed to write settings to {:?}", path))?;
        
        info!("Saved settings with {} saved links to {:?}", self.saved_links.len(), path);
        Ok(())
    }

    /// Add a new saved link
    pub fn add_link(&mut self, link: SavedLink) {
        self.saved_links.push(link);
    }

    /// Remove a saved link by ID
    pub fn remove_link(&mut self, id: &str) -> bool {
        let len_before = self.saved_links.len();
        self.saved_links.retain(|l| l.id != id);
        len_before != self.saved_links.len()
    }

    /// Update a saved link
    pub fn update_link(&mut self, id: &str, name: String, enabled: bool) -> bool {
        if let Some(link) = self.saved_links.iter_mut().find(|l| l.id == id) {
            link.name = name;
            link.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Get all enabled links
    pub fn get_enabled_links(&self) -> Vec<&SavedLink> {
        self.saved_links.iter().filter(|l| l.enabled).collect()
    }

    /// Mark a link as synced by ID
    pub fn mark_link_synced(&mut self, id: &str) {
        if let Some(link) = self.saved_links.iter_mut().find(|l| l.id == id) {
            link.mark_synced();
        }
    }
}

/// Get the directory that holds all MaMo Connector data (settings, lock file, etc.)
pub fn get_settings_dir() -> Result<PathBuf> {
    let dir = if cfg!(windows) {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("MamoConnector")
    } else if cfg!(target_os = "macos") {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join("Library")
            .join("Application Support")
            .join("MamoConnector")
    } else {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("mamo-connector")
    };
    Ok(dir)
}

/// Get the path to the settings file
fn get_settings_path() -> Result<PathBuf> {
    let config_dir = if cfg!(windows) {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("MamoConnector")
    } else if cfg!(target_os = "macos") {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?
            .join("Library")
            .join("Application Support")
            .join("MamoConnector")
    } else {
        dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?
            .join("mamo-connector")
    };
    
    Ok(config_dir.join("settings.json"))
}

/// Generate a simple UUID v4
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    
    // Simple pseudo-random based on timestamp and a counter
    let rand1 = (timestamp & 0xFFFFFFFF) as u32;
    let rand2 = ((timestamp >> 32) & 0xFFFFFFFF) as u32;
    let rand3 = ((timestamp >> 64) & 0xFFFFFFFF) as u32;
    let rand4 = (timestamp.wrapping_mul(0x5851F42D4C957F2D) & 0xFFFFFFFF) as u32;
    
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        rand1,
        (rand2 >> 16) & 0xFFFF,
        rand2 & 0x0FFF,
        0x8000 | (rand3 & 0x3FFF),
        (rand4 as u64) | ((rand3 as u64) << 32) & 0xFFFFFFFFFFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saved_link_creation() {
        let link = SavedLink::new(
            "Test Deck".to_string(),
            SavedLinkType::MoxfieldDeck,
            "abc123".to_string(),
        );
        
        assert_eq!(link.name, "Test Deck");
        assert_eq!(link.link_type, SavedLinkType::MoxfieldDeck);
        assert_eq!(link.url, "abc123");
        assert!(link.enabled);
        assert!(!link.id.is_empty());
    }

    #[test]
    fn test_settings_add_remove_link() {
        let mut settings = Settings::default();
        
        let link = SavedLink::new(
            "Test Deck".to_string(),
            SavedLinkType::MoxfieldDeck,
            "abc123".to_string(),
        );
        let link_id = link.id.clone();
        
        settings.add_link(link);
        assert_eq!(settings.saved_links.len(), 1);
        
        assert!(settings.remove_link(&link_id));
        assert_eq!(settings.saved_links.len(), 0);
    }

    #[test]
    fn test_uuid_v4_format() {
        let uuid = uuid_v4();
        // Check format: 8-4-4-4-12
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
    }

    #[derive(Default)]
    struct MemStore {
        data: std::cell::RefCell<std::collections::HashMap<String, String>>,
        fail_get: bool,
        fail_set: bool,
    }

    impl SecretStore for MemStore {
        fn get(&self, account: &str) -> Result<Option<String>> {
            if self.fail_get {
                anyhow::bail!("store unreadable");
            }
            Ok(self.data.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, value: &str) -> Result<()> {
            if self.fail_set {
                anyhow::bail!("store refuses writes");
            }
            self.data.borrow_mut().insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<()> {
            self.data.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn settings_with_tokens(api: Option<&str>, gamelog: Option<&str>) -> Settings {
        let mut settings = Settings::default();
        settings.auth_token = api.map(str::to_string);
        settings.gamelog_config.auth_token = gamelog.map(str::to_string);
        settings
    }

    #[test]
    fn test_plaintext_tokens_move_into_the_store() {
        let store = MemStore::default();
        let mut settings = settings_with_tokens(Some("jwt-token"), Some("pat_gamelog"));

        assert!(settings.hydrate_secrets(&store), "file must be rewritten after migration");
        // the two tokens stay distinct: uploads keep using the credential they used before
        assert_eq!(store.get(ACCOUNT_API_TOKEN).unwrap().as_deref(), Some("jwt-token"));
        assert_eq!(store.get(ACCOUNT_GAMELOG_TOKEN).unwrap().as_deref(), Some("pat_gamelog"));
        assert_eq!(settings.auth_token.as_deref(), Some("jwt-token"));

        let on_disk = serde_json::to_string(&settings.persisted_copy(&store)).unwrap();
        assert!(!on_disk.contains("jwt-token") && !on_disk.contains("pat_gamelog"));
    }

    #[test]
    fn test_tokens_are_read_back_from_the_store() {
        let store = MemStore::default();
        store.set(ACCOUNT_API_TOKEN, "stored-api").unwrap();
        let mut settings = settings_with_tokens(None, None);

        assert!(!settings.hydrate_secrets(&store), "nothing to migrate");
        assert_eq!(settings.auth_token.as_deref(), Some("stored-api"));
        assert_eq!(settings.gamelog_config.auth_token, None);
    }

    #[test]
    fn test_token_stays_in_file_when_store_refuses_writes() {
        let store = MemStore { fail_set: true, ..Default::default() };
        let mut settings = settings_with_tokens(Some("jwt-token"), None);

        assert!(!settings.hydrate_secrets(&store));
        let copy = settings.persisted_copy(&store);
        assert_eq!(copy.auth_token.as_deref(), Some("jwt-token"));
    }

    #[test]
    fn test_unreadable_store_is_not_wiped_by_a_save() {
        let store = MemStore { fail_get: true, ..Default::default() };
        store.data.borrow_mut().insert(ACCOUNT_API_TOKEN.to_string(), "keep-me".to_string());
        let mut settings = settings_with_tokens(None, None);

        settings.hydrate_secrets(&store);
        settings.persisted_copy(&store);
        assert_eq!(store.data.borrow().get(ACCOUNT_API_TOKEN).map(String::as_str), Some("keep-me"));
    }

    #[test]
    fn test_clearing_the_token_deletes_the_stored_secret() {
        let store = MemStore::default();
        store.set(ACCOUNT_API_TOKEN, "stored-api").unwrap();
        let mut settings = settings_with_tokens(None, None);
        settings.hydrate_secrets(&store);

        settings.auth_token = None; // user signs out
        settings.persisted_copy(&store);
        assert_eq!(store.get(ACCOUNT_API_TOKEN).unwrap(), None);
    }
}
