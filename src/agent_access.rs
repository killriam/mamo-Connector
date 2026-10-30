//! AI agent access: redeems the one-time code the MaMo website hands over (`mamoConnector://agent-access?code=...`)
//! for an MCP token, and keeps that token in the OS credential store where the MaMo MCP server reads it
//! (service `MaMo MCP`, accounts `read` and `write`). The token never touches `settings.json`; only
//! non-secret facts (token id, expiry) are remembered there for display.
//!
//! Two tokens exist on purpose (read-only and write); the website issues them separately.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::secrets::SecretStore;

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Read,
    Write,
}

impl TokenKind {
    pub const ALL: [TokenKind; 2] = [TokenKind::Read, TokenKind::Write];

    /// Credential store account name; the MCP server reads the same names.
    pub fn account(self) -> &'static str {
        match self {
            TokenKind::Read => "read",
            TokenKind::Write => "write",
        }
    }

    pub fn scope(self) -> &'static str {
        match self {
            TokenKind::Read => "mcp:read",
            TokenKind::Write => "mcp:write",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TokenKind::Read => "Read access",
            TokenKind::Write => "Write access",
        }
    }

    /// The kind of a token that carries exactly one MCP scope; anything else is not ours to store.
    fn from_scopes(scopes: &[String]) -> Option<TokenKind> {
        match scopes {
            [only] => TokenKind::ALL.into_iter().find(|kind| kind.scope() == only),
            _ => None,
        }
    }
}

/// Non-secret facts about a stored token, kept in `settings.json` for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTokenInfo {
    pub id: String,
    pub expires_at: Option<String>,
    pub saved_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAccessSettings {
    #[serde(default)]
    pub read: Option<AgentTokenInfo>,
    #[serde(default)]
    pub write: Option<AgentTokenInfo>,
}

impl AgentAccessSettings {
    pub fn get(&self, kind: TokenKind) -> Option<&AgentTokenInfo> {
        match kind {
            TokenKind::Read => self.read.as_ref(),
            TokenKind::Write => self.write.as_ref(),
        }
    }

    fn set(&mut self, kind: TokenKind, info: Option<AgentTokenInfo>) {
        match kind {
            TokenKind::Read => self.read = info,
            TokenKind::Write => self.write = info,
        }
    }
}

/// A token just obtained from the website's one-time code.
#[derive(Debug)]
pub struct Redeemed {
    pub token: String,
    pub kind: TokenKind,
    pub info: AgentTokenInfo,
}

/// A handoff code is 32 random bytes as lowercase hex.
pub fn is_valid_code(code: &str) -> bool {
    code.len() == 64 && code.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A MaMo personal access token: `pat_` plus 32 random bytes as lowercase hex.
pub fn is_valid_token(token: &str) -> bool {
    token
        .strip_prefix("pat_")
        .is_some_and(|rest| rest.len() == 64 && rest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The code is sent to the API address, so it must be https (http only for the developer's own machine).
fn check_api_url(api_url: &str) -> Result<()> {
    let url = url::Url::parse(api_url).map_err(|_| anyhow!("The MaMo API address is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("The MaMo API address must not contain credentials");
    }
    let local = matches!(url.host_str(), Some("localhost") | Some("127.0.0.1") | Some("[::1]"));
    match url.scheme() {
        "https" => Ok(()),
        "http" if local => Ok(()),
        _ => bail!("The MaMo API address must use https"),
    }
}

#[derive(Deserialize)]
struct ExchangeResponse {
    token: String,
    id: String,
    scopes: Vec<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

/// Exchanges the one-time code for a token. The error messages never contain the code or the token.
pub async fn redeem_code(api_url: &str, code: &str) -> Result<Redeemed> {
    if !is_valid_code(code) {
        bail!("The code is not valid. Create a new one on the MaMo website.");
    }
    check_api_url(api_url)?;

    let client = reqwest::Client::builder()
        .timeout(EXCHANGE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none()) // never hand the code to another address
        .build()
        .context("Could not prepare the request")?;

    let url = format!("{}/api/pat/exchange", api_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .map_err(|_| anyhow!("Could not reach MaMo. Check the internet connection and try again."))?;

    let status = response.status();
    if status.as_u16() == 400 {
        bail!("The code is invalid, already used or expired. Create a new one on the MaMo website.");
    }
    if status.as_u16() == 429 {
        bail!("MaMo is rate limiting this computer. Wait a few minutes and try again.");
    }
    if !status.is_success() {
        bail!("MaMo returned an unexpected answer (HTTP {}).", status.as_u16());
    }

    let body: ExchangeResponse = response
        .json()
        .await
        .map_err(|_| anyhow!("MaMo returned an answer this version of the Connector does not understand."))?;

    if !is_valid_token(&body.token) {
        bail!("MaMo returned a token in an unexpected format.");
    }
    let kind = TokenKind::from_scopes(&body.scopes)
        .ok_or_else(|| anyhow!("MaMo returned a token for something other than AI agent access."))?;

    Ok(Redeemed {
        token: body.token,
        kind,
        info: AgentTokenInfo {
            id: body.id,
            expires_at: body.expires_at,
            saved_at: chrono::Utc::now().to_rfc3339(),
        },
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExpiryState {
    /// The server did not give an expiry.
    Unknown,
    /// Still valid; the day (YYYY-MM-DD) it expires.
    ValidUntil(String),
    /// Already expired on this day (YYYY-MM-DD).
    Expired(String),
}

pub fn expiry_state(expires_at: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> ExpiryState {
    let Some(parsed) = expires_at.and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok()) else {
        return ExpiryState::Unknown;
    };
    let day = parsed.format("%Y-%m-%d").to_string();
    if parsed.with_timezone(&chrono::Utc) <= now {
        ExpiryState::Expired(day)
    } else {
        ExpiryState::ValidUntil(day)
    }
}

/// Stores the token in the credential store and remembers its non-secret facts. A new token replaces
/// the old one of the same kind. The caller persists `settings`.
pub fn apply_redeemed(store: &dyn SecretStore, settings: &mut AgentAccessSettings, redeemed: &Redeemed) -> Result<()> {
    store
        .set(redeemed.kind.account(), &redeemed.token)
        .map_err(|_| anyhow!("Could not store the token in the Windows credential store."))?;
    settings.set(redeemed.kind, Some(redeemed.info.clone()));
    Ok(())
}

/// Removes a token from this computer (it stays valid on the server until revoked on the MaMo website).
pub fn remove_token(store: &dyn SecretStore, settings: &mut AgentAccessSettings, kind: TokenKind) -> Result<()> {
    store
        .delete(kind.account())
        .map_err(|_| anyhow!("Could not remove the token from the Windows credential store."))?;
    settings.set(kind, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const CODE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    fn token(fill: &str) -> String {
        format!("pat_{}", fill.repeat(64 / fill.len()))
    }

    #[derive(Default)]
    struct MemStore {
        data: RefCell<HashMap<String, String>>,
        fail_set: bool,
    }

    impl SecretStore for MemStore {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.data.borrow().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            if self.fail_set {
                bail!("store refuses writes with {value}");
            }
            self.data.borrow_mut().insert(account.to_string(), value.to_string());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<()> {
            self.data.borrow_mut().remove(account);
            Ok(())
        }
    }

    /// Serves one canned HTTP answer on localhost and returns the raw request it received.
    async fn mock_server(status_line: &'static str, extra_headers: &'static str, body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                received.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if received.len() >= header_end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
            String::from_utf8_lossy(&received).to_string()
        });
        (base, handle)
    }

    fn exchange_body(token: &str, scope: &str) -> String {
        serde_json::json!({
            "success": true, "token": token, "id": "tok-1", "name": "MaMo MCP",
            "scopes": [scope], "created_at": "2026-10-30T10:00:00Z", "expires_at": "2027-01-28T10:00:00Z"
        })
        .to_string()
    }

    /// End-to-end, opt-in (`cargo test -- --ignored`): redeems a REAL one-time code against a REAL backend and
    /// stores the token in the REAL credential store under a test service name. Needs MAMO_E2E_API_URL,
    /// MAMO_E2E_CODE and MAMO_E2E_SERVICE; prints the facts the verification gate checks.
    #[tokio::test]
    #[ignore]
    async fn e2e_redeem_and_store_against_a_real_backend() {
        let api = std::env::var("MAMO_E2E_API_URL").expect("MAMO_E2E_API_URL");
        let code = std::env::var("MAMO_E2E_CODE").expect("MAMO_E2E_CODE");
        let service = std::env::var("MAMO_E2E_SERVICE").expect("MAMO_E2E_SERVICE");

        let redeemed = redeem_code(&api, &code).await.unwrap();
        let store = crate::secrets::agent_store(&service);
        let mut settings = AgentAccessSettings::default();
        apply_redeemed(store.as_ref(), &mut settings, &redeemed).unwrap();

        assert_eq!(store.get(redeemed.kind.account()).unwrap().as_deref(), Some(redeemed.token.as_str()));
        assert_eq!(settings.get(redeemed.kind).unwrap().id, redeemed.info.id);
        println!("E2E kind={} id={} expires={}", redeemed.kind.account(), redeemed.info.id, redeemed.info.expires_at.clone().unwrap_or_default());
    }

    /// Opt-in companion: the same code must not work a second time.
    #[tokio::test]
    #[ignore]
    async fn e2e_a_used_code_is_refused() {
        let api = std::env::var("MAMO_E2E_API_URL").expect("MAMO_E2E_API_URL");
        let code = std::env::var("MAMO_E2E_CODE").expect("MAMO_E2E_CODE");
        let error = redeem_code(&api, &code).await.unwrap_err().to_string();
        assert!(error.contains("already used or expired"), "{error}");
        println!("E2E refused: {error}");
    }

    #[test]
    fn test_expiry_state() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(expiry_state(Some("2027-01-28T10:00:00Z"), now), ExpiryState::ValidUntil("2027-01-28".into()));
        assert_eq!(expiry_state(Some("2026-10-30T11:59:59Z"), now), ExpiryState::Expired("2026-10-30".into()));
        assert_eq!(expiry_state(None, now), ExpiryState::Unknown);
        assert_eq!(expiry_state(Some("garbage"), now), ExpiryState::Unknown);
    }

    #[test]
    fn test_code_and_token_shapes() {
        assert!(is_valid_code(CODE));
        for bad in ["", "abc", &CODE.to_uppercase(), &format!("{CODE}0"), &CODE.replace('0', "g")] {
            assert!(!is_valid_code(bad), "{bad}");
        }
        assert!(is_valid_token(&token("ab")));
        for bad in ["", "pat_", "pat_xyz", &token("ab").to_uppercase(), &format!("tok_{}", "ab".repeat(32))] {
            assert!(!is_valid_token(bad), "{bad}");
        }
    }

    #[test]
    fn test_only_https_or_local_http_api_addresses() {
        for good in ["https://api.example.com", "http://localhost:3001", "http://127.0.0.1:3001/"] {
            assert!(check_api_url(good).is_ok(), "{good}");
        }
        for bad in ["http://api.example.com", "ftp://x", "nonsense", "https://user:pw@api.example.com"] {
            assert!(check_api_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_kind_follows_the_single_scope() {
        assert_eq!(TokenKind::from_scopes(&["mcp:read".into()]), Some(TokenKind::Read));
        assert_eq!(TokenKind::from_scopes(&["mcp:write".into()]), Some(TokenKind::Write));
        assert_eq!(TokenKind::from_scopes(&["gamelog:upload".into()]), None);
        assert_eq!(TokenKind::from_scopes(&["mcp:read".into(), "mcp:write".into()]), None);
        assert_eq!(TokenKind::from_scopes(&[]), None);
    }

    #[tokio::test]
    async fn test_redeems_a_read_token_and_sends_only_the_code() {
        let t = token("ab");
        let (base, request) = mock_server("201 Created", "", exchange_body(&t, "mcp:read")).await;
        let redeemed = redeem_code(&base, CODE).await.unwrap();
        assert_eq!(redeemed.kind, TokenKind::Read);
        assert_eq!(redeemed.token, t);
        assert_eq!(redeemed.info.id, "tok-1");
        assert_eq!(redeemed.info.expires_at.as_deref(), Some("2027-01-28T10:00:00Z"));

        let raw = request.await.unwrap();
        assert!(raw.starts_with("POST /api/pat/exchange HTTP/1.1"), "{raw}");
        assert!(raw.contains(&format!("{{\"code\":\"{CODE}\"}}")));
        assert!(!raw.to_lowercase().contains("authorization"), "the exchange needs no login");
    }

    #[tokio::test]
    async fn test_redeems_a_write_token() {
        let (base, _request) = mock_server("201 Created", "", exchange_body(&token("cd"), "mcp:write")).await;
        assert_eq!(redeem_code(&base, CODE).await.unwrap().kind, TokenKind::Write);
    }

    #[tokio::test]
    async fn test_failures_are_explained_without_leaking_secrets() {
        let t = token("ef");
        let cases: Vec<(&'static str, String, &str)> = vec![
            ("400 Bad Request", r#"{"code":"INVALID_HANDOFF_CODE"}"#.to_string(), "invalid, already used or expired"),
            ("429 Too Many Requests", "{}".to_string(), "rate limiting"),
            ("500 Internal Server Error", "{}".to_string(), "HTTP 500"),
            ("201 Created", "not json".to_string(), "does not understand"),
            ("201 Created", exchange_body("pat_short", "mcp:read"), "unexpected format"),
            ("201 Created", exchange_body(&t, "gamelog:upload"), "something other than AI agent access"),
        ];
        for (status, body, expected) in cases {
            let (base, _request) = mock_server(status, "", body).await;
            let error = redeem_code(&base, CODE).await.unwrap_err().to_string();
            assert!(error.contains(expected), "{status}: {error}");
            assert!(!error.contains(CODE) && !error.contains(&t), "{error}");
        }
    }

    #[tokio::test]
    async fn test_does_not_follow_redirects() {
        let (base, _request) = mock_server("302 Found", "Location: http://127.0.0.1:1/elsewhere\r\n", String::new()).await;
        let error = redeem_code(&base, CODE).await.unwrap_err().to_string();
        assert!(error.contains("HTTP 302"), "{error}");
    }

    #[tokio::test]
    async fn test_unreachable_api_and_bad_input_never_call_out() {
        let unreachable = redeem_code("http://127.0.0.1:1", CODE).await.unwrap_err().to_string();
        assert!(unreachable.contains("Could not reach MaMo") && !unreachable.contains(CODE));
        assert!(redeem_code("http://127.0.0.1:1", "short").await.unwrap_err().to_string().contains("not valid"));
        assert!(redeem_code("http://api.example.com", CODE).await.unwrap_err().to_string().contains("https"));
    }

    #[test]
    fn test_storing_and_removing_tokens_keeps_the_two_kinds_apart() {
        let store = MemStore::default();
        let mut settings = AgentAccessSettings::default();
        let read = Redeemed { token: token("ab"), kind: TokenKind::Read, info: AgentTokenInfo { id: "r1".into(), expires_at: None, saved_at: "t".into() } };
        let write = Redeemed { token: token("cd"), kind: TokenKind::Write, info: AgentTokenInfo { id: "w1".into(), expires_at: None, saved_at: "t".into() } };

        apply_redeemed(&store, &mut settings, &read).unwrap();
        apply_redeemed(&store, &mut settings, &write).unwrap();
        assert_eq!(store.get("read").unwrap().as_deref(), Some(token("ab").as_str()));
        assert_eq!(store.get("write").unwrap().as_deref(), Some(token("cd").as_str()));
        assert_eq!(settings.get(TokenKind::Read).unwrap().id, "r1");

        // a new token replaces only its own kind
        let newer = Redeemed { token: token("12"), kind: TokenKind::Read, info: AgentTokenInfo { id: "r2".into(), expires_at: None, saved_at: "t".into() } };
        apply_redeemed(&store, &mut settings, &newer).unwrap();
        assert_eq!(settings.get(TokenKind::Read).unwrap().id, "r2");
        assert_eq!(settings.get(TokenKind::Write).unwrap().id, "w1");

        remove_token(&store, &mut settings, TokenKind::Read).unwrap();
        assert_eq!(store.get("read").unwrap(), None);
        assert!(settings.get(TokenKind::Read).is_none());
        assert!(store.get("write").unwrap().is_some() && settings.get(TokenKind::Write).is_some());
    }

    #[test]
    fn test_a_store_failure_neither_leaks_the_token_nor_records_the_token() {
        let store = MemStore { fail_set: true, ..Default::default() };
        let mut settings = AgentAccessSettings::default();
        let t = token("ab");
        let redeemed = Redeemed { token: t.clone(), kind: TokenKind::Read, info: AgentTokenInfo { id: "r1".into(), expires_at: None, saved_at: "t".into() } };
        let error = apply_redeemed(&store, &mut settings, &redeemed).unwrap_err().to_string();
        assert!(!error.contains(&t), "{error}");
        assert!(settings.get(TokenKind::Read).is_none(), "no metadata for a token that was not stored");
    }
}
