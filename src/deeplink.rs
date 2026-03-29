use log::warn;
use url::Url;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Deeplink {
    pub raw: String,
    pub action: String,
    pub params: Vec<(String, String)>,
    pub token: Option<String>,
    pub doc: Option<String>,
    pub deck_id: Option<String>,
    pub username: Option<String>,
}

pub fn parse_deeplink(args: &[String], scheme_prefix: &str) -> Option<Deeplink> {
    let scheme_prefix_lower = scheme_prefix.to_lowercase();
    let raw = args
        .iter()
        .find(|arg| arg.to_lowercase().starts_with(&scheme_prefix_lower))?
        .clone();
    parse_deeplink_url(&raw)
}

pub fn parse_deeplink_url(raw: &str) -> Option<Deeplink> {
    let raw = raw.to_string();
    match Url::parse(&raw) {
        Ok(url) => {
            let action = url.host_str().unwrap_or_default().to_string();
            let mut params = Vec::new();
            let mut token = None;
            let mut doc = None;
            let mut deck_id = None;
            let mut username = None;
            
            // Parse query parameters
            for (key, value) in url.query_pairs() {
                let value = value.to_string();
                if key == "token" {
                    token = Some(value.clone());
                }
                if key == "doc" {
                    doc = Some(value.clone());
                }
                if key == "id" || key == "deck_id" || key == "deckId" {
                    deck_id = Some(value.clone());
                }
                if key == "username" || key == "user" {
                    username = Some(value.clone());
                }
                params.push((key.to_string(), value));
            }
            
            // Also check path for deck ID (e.g., mamoConnector://deck/DECK_ID or mamoConnector://playtest/UUID)
            let path = url.path();
            if !path.is_empty() && path != "/" {
                let path_parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                if !path_parts.is_empty() && !path_parts[0].is_empty() {
                    // If action is "deck", "mamo", "playtest", "launch-forge", or "launchforge", the path is the deck ID
                    if (action == "deck" || action == "mamo" || action == "playtest" || action == "launch-forge" || action == "launchforge" || action == "replay-game" || action == "replaygame") && deck_id.is_none() {
                        deck_id = Some(path_parts[0].to_string());
                    } else if action == "user" && username.is_none() {
                        username = Some(path_parts[0].to_string());
                    }
                }
            }
            
            Some(Deeplink {
                raw,
                action,
                params,
                token,
                doc,
                deck_id,
                username,
            })
        }
        Err(err) => {
            warn!("Failed to parse deeplink '{raw}': {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEME_PREFIX: &str = "mamoConnector://";

    #[test]
    fn test_parse_create_deck_url_with_id() {
        let url = "mamoConnector://create-deck?id=12345&api_url=http://localhost:8080";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "create-deck");
        assert_eq!(result.deck_id, Some("12345".to_string()));
        assert!(result.params.iter().any(|(k, v)| k == "api_url" && v == "http://localhost:8080"));
    }

    #[test]
    fn test_parse_create_deck_url_with_deck_id() {
        let url = "mamoConnector://create-deck?deck_id=abc123";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "create-deck");
        assert_eq!(result.deck_id, Some("abc123".to_string()));
    }

    #[test]
    fn test_parse_create_deck_url_with_deckId_camelcase() {
        let url = "mamoConnector://createdeck?deckId=xyz789";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "createdeck");
        assert_eq!(result.deck_id, Some("xyz789".to_string()));
    }

    #[test]
    fn test_parse_deeplink_from_args() {
        let args = vec![
            "--some-flag".to_string(),
            "mamoConnector://create-deck?id=test123".to_string(),
        ];
        let result = parse_deeplink(&args, SCHEME_PREFIX).unwrap();
        
        assert_eq!(result.action, "create-deck");
        assert_eq!(result.deck_id, Some("test123".to_string()));
    }

    #[test]
    fn test_parse_deeplink_no_matching_arg() {
        let args = vec![
            "--flag".to_string(),
            "http://example.com".to_string(),
        ];
        let result = parse_deeplink(&args, SCHEME_PREFIX);
        
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_deeplink_empty_args() {
        let args: Vec<String> = vec![];
        let result = parse_deeplink(&args, SCHEME_PREFIX);
        
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_deeplink_with_token() {
        let url = "mamoConnector://open?token=secret123&doc=mydoc";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "open");
        assert_eq!(result.token, Some("secret123".to_string()));
        assert_eq!(result.doc, Some("mydoc".to_string()));
    }

    #[test]
    fn test_parse_deeplink_with_multiple_params() {
        let url = "mamoConnector://create-deck?id=123&api_url=http://api.test.com&format=forge&version=1";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "create-deck");
        assert_eq!(result.deck_id, Some("123".to_string()));
        assert_eq!(result.params.len(), 4);
    }

    #[test]
    fn test_parse_deeplink_with_encoded_url() {
        let url = "mamoConnector://create-deck?id=123&api_url=http%3A%2F%2Flocalhost%3A8080";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "create-deck");
        // URL should be decoded
        assert!(result.params.iter().any(|(k, v)| k == "api_url" && v == "http://localhost:8080"));
    }

    #[test]
    fn test_parse_deeplink_preserves_raw() {
        let url = "mamoConnector://create-deck?id=test";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.raw, url);
    }

    #[test]
    fn test_parse_deeplink_no_params() {
        let url = "mamoConnector://create-deck";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "create-deck");
        assert!(result.params.is_empty());
        assert!(result.deck_id.is_none());
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_invalid_url() {
        let url = "not-a-valid-url";
        let result = parse_deeplink_url(url);
        
        assert!(result.is_none());
    }

    // ==================== Username Parameter Tests ====================

    #[test]
    fn test_parse_import_user_decks_with_username() {
        let url = "mamoConnector://import-user-decks?username=IceMagma&api_url=http://localhost:8080";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "import-user-decks");
        assert_eq!(result.username, Some("IceMagma".to_string()));
        assert!(result.params.iter().any(|(k, v)| k == "api_url" && v == "http://localhost:8080"));
    }

    #[test]
    fn test_parse_import_user_decks_with_user_param() {
        let url = "mamoConnector://import-user-decks?user=TestUser";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "import-user-decks");
        assert_eq!(result.username, Some("TestUser".to_string()));
    }

    #[test]
    fn test_parse_list_user_decks() {
        let url = "mamoConnector://list-user-decks?username=IceMagma";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "list-user-decks");
        assert_eq!(result.username, Some("IceMagma".to_string()));
    }

    #[test]
    fn test_parse_deeplink_no_username() {
        let url = "mamoConnector://import-user-decks?api_url=http://localhost";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "import-user-decks");
        assert!(result.username.is_none());
    }

    // ==================== Playtest / Launch Forge Tests ====================

    #[test]
    fn test_parse_playtest_with_deck_id_in_path() {
        let url = "mamoConnector://playtest/b15ace87-3153-45c9-afc9-5c8a2163384d";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "playtest");
        assert_eq!(result.deck_id, Some("b15ace87-3153-45c9-afc9-5c8a2163384d".to_string()));
    }

    #[test]
    fn test_parse_launch_forge_with_deck_id_in_path() {
        let url = "mamoConnector://launch-forge/abc-123-def";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "launch-forge");
        assert_eq!(result.deck_id, Some("abc-123-def".to_string()));
    }

    #[test]
    fn test_parse_launchforge_with_deck_id_in_path() {
        let url = "mamoConnector://launchforge/xyz789";
        let result = parse_deeplink_url(url).unwrap();
        
        assert_eq!(result.action, "launchforge");
        assert_eq!(result.deck_id, Some("xyz789".to_string()));
    }

    // ── Replay Game Tests ───────────────────────────────────────────

    #[test]
    fn test_parse_replay_game_with_gamelog_id_in_path() {
        let url = "mamoConnector://replay-game/b15ace87-3153-45c9-afc9-5c8a2163384d";
        let result = parse_deeplink_url(url).unwrap();

        assert_eq!(result.action, "replay-game");
        assert_eq!(result.deck_id, Some("b15ace87-3153-45c9-afc9-5c8a2163384d".to_string()));
    }

    #[test]
    fn test_parse_replaygame_with_gamelog_id_in_path() {
        let url = "mamoConnector://replaygame/abc-123-def";
        let result = parse_deeplink_url(url).unwrap();

        assert_eq!(result.action, "replaygame");
        assert_eq!(result.deck_id, Some("abc-123-def".to_string()));
    }

    #[test]
    fn test_parse_replay_game_no_id() {
        let url = "mamoConnector://replay-game";
        let result = parse_deeplink_url(url).unwrap();

        assert_eq!(result.action, "replay-game");
        assert_eq!(result.deck_id, None);
    }
}
