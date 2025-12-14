use log::warn;
use url::Url;

#[derive(Debug, Clone)]
pub struct Deeplink {
    pub raw: String,
    pub action: String,
    pub params: Vec<(String, String)>,
    pub token: Option<String>,
    pub doc: Option<String>,
    pub deck_id: Option<String>,
}

pub fn parse_deeplink(args: &[String], scheme_prefix: &str) -> Option<Deeplink> {
    let raw = args
        .iter()
        .find(|arg| arg.starts_with(scheme_prefix))?
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
                params.push((key.to_string(), value));
            }
            Some(Deeplink {
                raw,
                action,
                params,
                token,
                doc,
                deck_id,
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
    }

    #[test]
    fn test_parse_invalid_url() {
        let url = "not-a-valid-url";
        let result = parse_deeplink_url(url);
        
        assert!(result.is_none());
    }
}
