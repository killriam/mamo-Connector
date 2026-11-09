//! Unifies "fetch the list of decks belonging to a user" — previously four independent
//! implementations reachable from different UI/deeplink entry points with different fallback
//! behavior: MaMo's own `/mydecks` (gamelog::fetch_my_decks), a MaMo public profile lookup
//! (deck::fetch_mamo_user_decks), and Moxfield, which had three separate fetchers (curl-direct,
//! reqwest-direct, reqwest-via-backend) depending on whether the caller was the desktop UI's
//! "Fetch User Decks" button, a `mamoConnector://import-user-decks` deeplink, or
//! `mamoConnector://list-user-decks`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use log::warn;

use crate::deck::{self, DeckStatus, MamoDeckEntry, MoxfieldDeckEntry, UserDecksImportResult};
use crate::gamelog::{self, UserDeck};
use crate::settings::Settings;

/// Which deck list is being asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDeckSource {
    /// The signed-in MaMo account's own decks (PAT-authenticated `/mydecks`). Not wired to any
    /// call site yet — the Play tab's account picker still calls `gamelog::fetch_my_decks`
    /// directly, since it feeds different state than the Decks-tab import list. Reserved for
    /// the Decks-tab "My MaMo Decks" row this backend consolidation was scoped to enable.
    #[allow(dead_code)]
    MamoMine,
    /// Another MaMo user's public profile, by username.
    MamoProfile(String),
    /// A Moxfield user's public profile, by username.
    MoxfieldProfile(String),
}

impl UserDeckSource {
    fn username(&self) -> String {
        match self {
            UserDeckSource::MamoMine => "me".to_string(),
            UserDeckSource::MamoProfile(u) | UserDeckSource::MoxfieldProfile(u) => u.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckOrigin {
    Mamo,
    Moxfield,
}

/// One deck in a fetched user-deck list, normalized across MaMo's and Moxfield's shapes so a
/// caller can render either without knowing which source produced it.
#[derive(Debug, Clone)]
pub struct RemoteDeck {
    pub origin: DeckOrigin,
    pub id: String,
    pub name: String,
    pub format: Option<String>,
    pub commander: Option<String>,
    pub local_status: Option<DeckStatus>,
    pub local_date: Option<String>,
    pub remote_date: Option<String>,
}

impl From<MoxfieldDeckEntry> for RemoteDeck {
    fn from(d: MoxfieldDeckEntry) -> Self {
        let remote_date = d
            .last_updated_at_utc
            .as_ref()
            .and_then(|dt| dt.split('T').next())
            .map(|s| s.to_string());
        Self {
            origin: DeckOrigin::Moxfield,
            id: d.public_id,
            name: d.name,
            format: d.format,
            commander: None,
            local_status: d.local_status,
            local_date: d.local_date,
            remote_date,
        }
    }
}

impl From<MamoDeckEntry> for RemoteDeck {
    fn from(d: MamoDeckEntry) -> Self {
        Self {
            origin: DeckOrigin::Mamo,
            id: d.deck_id,
            name: d.deck_name,
            format: d.format,
            commander: d.commander_name,
            local_status: d.local_status,
            local_date: None,
            remote_date: d.updated_at,
        }
    }
}

impl From<UserDeck> for RemoteDeck {
    fn from(d: UserDeck) -> Self {
        Self {
            origin: DeckOrigin::Mamo,
            id: d.deck_id,
            name: d.deck_name,
            format: None,
            commander: None,
            local_status: None,
            local_date: None,
            remote_date: d.updated_at,
        }
    }
}

/// Fetch the deck list for `source`.
///
/// `api_base_url` is only used by the `MamoProfile`/`MoxfieldProfile` backend calls — callers
/// with a deeplink-supplied override (e.g. `handle_import_user_decks`) pass it through; the
/// desktop UI passes `settings.gamelog_config.api_url` (falling back to `deck::MAMO_API_URL`
/// when unset, matching the pattern already used by `sync_archidekt_deck`). `MamoMine` ignores
/// it and always uses `settings.gamelog_config`, since that's the existing `/mydecks` contract.
///
/// For Moxfield, this tries the backend proxy first (avoids Moxfield's Cloudflare protection
/// blocking a direct call) and falls back to the curl-direct implementation on failure — the
/// same order `deck::import_user_decks` already used, now the only path instead of one of three.
pub async fn fetch(source: &UserDeckSource, api_base_url: &str, settings: &Settings) -> Result<Vec<RemoteDeck>> {
    match source {
        UserDeckSource::MamoMine => {
            let decks = gamelog::fetch_my_decks(&settings.gamelog_config).await?;
            if let Err(e) = gamelog::save_cached_decks(&decks) {
                warn!("Failed to cache decks: {}", e);
            }
            Ok(decks.into_iter().map(RemoteDeck::from).collect())
        }
        UserDeckSource::MamoProfile(username) => {
            let decks = deck::fetch_mamo_user_decks(username).await?;
            Ok(decks.into_iter().map(RemoteDeck::from).collect())
        }
        UserDeckSource::MoxfieldProfile(username) => {
            match deck::fetch_user_decks_via_backend(username, api_base_url).await {
                Ok(decks) => Ok(decks.into_iter().map(RemoteDeck::from).collect()),
                Err(backend_err) => {
                    warn!(
                        "Backend Moxfield fetch failed ({}), falling back to direct fetch...",
                        backend_err
                    );
                    let token = settings.moxfield_auth_token.clone();
                    let username = username.clone();
                    let decks = tokio::task::spawn_blocking(move || {
                        deck::fetch_user_decks_direct_with_token(&username, token.as_deref())
                    })
                    .await??;
                    Ok(decks.into_iter().map(RemoteDeck::from).collect())
                }
            }
        }
    }
}

/// Import specific decks by id from `source`. Uses the same direct creation functions the
/// desktop UI's "Import Selected" flows already called (`create_deck_from_moxfield` /
/// `create_deck_from_mamo`) rather than the legacy `create_deck_from_id` backend-proxy function
/// that only the old `deck::import_user_decks` routed Moxfield imports through.
pub async fn import(source: &UserDeckSource, deck_ids: &[String]) -> Result<UserDecksImportResult> {
    let mut imported = Vec::new();
    let mut failed = Vec::new();

    for deck_id in deck_ids {
        let result = match source {
            UserDeckSource::MoxfieldProfile(_) => deck::create_deck_from_moxfield(deck_id).await,
            UserDeckSource::MamoProfile(_) | UserDeckSource::MamoMine => {
                deck::create_deck_from_mamo(deck_id).await
            }
        };
        match result {
            Ok(r) => imported.push(r),
            Err(e) => {
                warn!("Failed to import deck '{}': {}", deck_id, e);
                failed.push((deck_id.clone(), e.to_string()));
            }
        }
    }

    Ok(UserDecksImportResult::success(source.username(), imported, failed))
}

/// Fetch then import every deck for `source` — the "import all" semantics
/// `handle_import_user_decks` (deeplink-triggered) needs, as opposed to the UI's "import
/// selected" flow, which calls `import` directly with a user-picked subset of an already-fetched
/// `fetch()` result.
pub async fn import_all(source: &UserDeckSource, api_base_url: &str, settings: &Settings) -> Result<UserDecksImportResult> {
    let decks = fetch(source, api_base_url, settings).await?;
    if decks.is_empty() {
        return Ok(UserDecksImportResult::failed(
            source.username(),
            "No public decks found for this user".to_string(),
        ));
    }
    let ids: Vec<String> = decks.into_iter().map(|d| d.id).collect();
    import(source, &ids).await
}

/// The signed-in user's MaMo decks that are in Forge's deck folder but were edited on MaMo
/// after the Connector last downloaded them, so Forge would launch with an outdated copy.
///
/// A deck only counts when a local .dck written for it still exists — decks the user never
/// downloaded (or has since deleted from Forge) are not "outdated", just absent. The download
/// time comes from `DeckMappings::downloaded_at`; decks downloaded by a Connector build that
/// predates that field fall back to the local file's modification time.
///
/// `exclude_deck_id` is the deck the pending launch is about to download anyway.
pub async fn find_decks_changed_since_download(
    config: &gamelog::GameLogConfig,
    exclude_deck_id: Option<&str>,
) -> Result<Vec<UserDeck>> {
    let remote = gamelog::fetch_my_decks(config).await?;
    let mappings = gamelog::DeckMappings::load()?;
    let deck_dir = deck::get_deck_directory()?;

    Ok(remote
        .into_iter()
        .filter(|d| exclude_deck_id != Some(d.deck_id.as_str()))
        .filter(|d| {
            // One entry per local .dck file that exists for this deck; the inner Option is the
            // file's modification time, if the filesystem reports one.
            let local_files: Vec<Option<DateTime<Utc>>> = mappings
                .names_for_deck_id(&d.deck_id)
                .map(|name| deck_dir.join(format!("{}.dck", deck::sanitize_filename(name))))
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|meta| meta.modified().ok().map(DateTime::<Utc>::from))
                .collect();
            is_outdated(
                d.updated_at.as_deref(),
                mappings.downloaded_at.get(&d.deck_id).map(String::as_str),
                &local_files,
            )
        })
        .collect())
}

/// Whether a local copy is older than the deck on MaMo. `local_files` holds one entry per
/// existing local .dck for the deck (its modification time, if known); empty means the deck is
/// not in Forge at all. Unparseable or missing remote timestamps never count as outdated, so a
/// backend format change degrades to "no prompt" rather than a prompt on every launch.
fn is_outdated(remote_updated_at: Option<&str>, recorded_download: Option<&str>, local_files: &[Option<DateTime<Utc>>]) -> bool {
    if local_files.is_empty() {
        return false;
    }
    let Some(updated_at) = remote_updated_at.and_then(parse_utc) else {
        return false;
    };
    let downloaded_at = recorded_download
        .and_then(parse_utc)
        .or_else(|| local_files.iter().flatten().max().copied());
    downloaded_at.is_some_and(|at| updated_at > at)
}

fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_utc_reads_backend_iso_timestamps_and_rejects_garbage() {
        let backend = parse_utc("2026-11-09T10:15:00.000Z").expect("backend toISOString format");
        let stored = parse_utc("2026-11-09T11:15:00+01:00").expect("chrono to_rfc3339 format");
        assert_eq!(backend, stored);
        assert!(parse_utc("2026-11-09").is_none());
        assert!(parse_utc("").is_none());
    }

    #[test]
    fn is_outdated_compares_remote_edit_with_recorded_download() {
        let file = [parse_utc("2026-01-01T00:00:00Z")];
        let downloaded = Some("2026-11-09T10:00:00+00:00");

        assert!(is_outdated(Some("2026-11-09T10:00:01.000Z"), downloaded, &file), "edited after download");
        assert!(!is_outdated(Some("2026-11-09T09:59:59.000Z"), downloaded, &file), "edited before download");
        assert!(!is_outdated(Some("2026-11-09T10:00:00.000Z"), downloaded, &file), "same instant is up to date");
    }

    #[test]
    fn is_outdated_falls_back_to_newest_file_mtime_without_a_recorded_download() {
        let files = [parse_utc("2026-11-01T00:00:00Z"), None, parse_utc("2026-11-05T00:00:00Z")];

        assert!(is_outdated(Some("2026-11-06T00:00:00.000Z"), None, &files));
        assert!(!is_outdated(Some("2026-11-04T00:00:00.000Z"), None, &files));
        assert!(!is_outdated(Some("2026-11-06T00:00:00.000Z"), None, &[None]), "no time known at all");
    }

    #[test]
    fn is_outdated_ignores_decks_not_in_forge_or_without_a_remote_timestamp() {
        let downloaded = Some("2026-01-01T00:00:00+00:00");
        let file = [parse_utc("2026-01-01T00:00:00Z")];

        assert!(!is_outdated(Some("2026-11-09T00:00:00.000Z"), downloaded, &[]), "no local file");
        assert!(!is_outdated(None, downloaded, &file), "no remote timestamp");
        assert!(!is_outdated(Some("not a date"), downloaded, &file), "unparseable remote timestamp");
    }

    #[test]
    fn source_username_covers_all_three_variants() {
        assert_eq!(UserDeckSource::MamoMine.username(), "me");
        assert_eq!(UserDeckSource::MamoProfile("alice".to_string()).username(), "alice");
        assert_eq!(UserDeckSource::MoxfieldProfile("bob".to_string()).username(), "bob");
    }

    #[test]
    fn remote_deck_from_mamo_deck_entry_carries_commander_and_drops_local_date() {
        let entry = MamoDeckEntry {
            deck_id: "d1".to_string(),
            deck_name: "Atraxa Superfriends".to_string(),
            user_id: "u1".to_string(),
            commander_name: Some("Atraxa, Praetors' Voice".to_string()),
            commander_partner_name: None,
            color_identity: Some("WUBG".to_string()),
            format: Some("Commander".to_string()),
            updated_at: Some("2026-01-02".to_string()),
            created_at: Some("2025-12-01".to_string()),
            local_status: Some(DeckStatus::New),
        };

        let remote: RemoteDeck = entry.into();

        assert_eq!(remote.origin, DeckOrigin::Mamo);
        assert_eq!(remote.id, "d1");
        assert_eq!(remote.name, "Atraxa Superfriends");
        assert_eq!(remote.commander.as_deref(), Some("Atraxa, Praetors' Voice"));
        assert_eq!(remote.local_status, Some(DeckStatus::New));
        assert_eq!(remote.remote_date.as_deref(), Some("2026-01-02"));
        assert!(remote.local_date.is_none());
    }

    #[test]
    fn remote_deck_from_user_deck_has_no_local_status_or_commander() {
        let deck = UserDeck {
            deck_id: "d2".to_string(),
            deck_name: "Krenko Goblins".to_string(),
            user_id: "u1".to_string(),
            color_identity: Some(vec!["R".to_string()]),
            commander_id: Some("krenko-uuid".to_string()),
            commander_partner_id: None,
            updated_at: Some("2026-02-03".to_string()),
            created_at: Some("2025-11-01".to_string()),
        };

        let remote: RemoteDeck = deck.into();

        assert_eq!(remote.origin, DeckOrigin::Mamo);
        assert_eq!(remote.id, "d2");
        assert_eq!(remote.name, "Krenko Goblins");
        assert_eq!(remote.remote_date.as_deref(), Some("2026-02-03"));
        assert!(remote.commander.is_none());
        assert!(remote.local_status.is_none());
    }

    #[test]
    fn remote_deck_from_moxfield_deck_entry_splits_date_and_keeps_local_status() {
        let json = r#"{
            "publicId": "mox1",
            "name": "Yuriko Ninjas",
            "format": "commander",
            "lastUpdatedAtUtc": "2026-03-04T12:00:00Z"
        }"#;
        let mut entry: MoxfieldDeckEntry = serde_json::from_str(json).unwrap();
        entry.local_status = Some(DeckStatus::NeedsUpdate);
        entry.local_date = Some("2026-03-01".to_string());

        let remote: RemoteDeck = entry.into();

        assert_eq!(remote.origin, DeckOrigin::Moxfield);
        assert_eq!(remote.id, "mox1");
        assert_eq!(remote.name, "Yuriko Ninjas");
        assert_eq!(remote.remote_date.as_deref(), Some("2026-03-04"));
        assert_eq!(remote.local_date.as_deref(), Some("2026-03-01"));
        assert_eq!(remote.local_status, Some(DeckStatus::NeedsUpdate));
        assert!(remote.commander.is_none());
    }
}
