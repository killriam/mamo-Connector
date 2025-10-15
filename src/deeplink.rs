use log::warn;
use url::Url;

#[derive(Debug, Clone)]
pub struct Deeplink {
    pub raw: String,
    pub action: String,
    pub params: Vec<(String, String)>,
    pub token: Option<String>,
    pub doc: Option<String>,
}

pub fn parse_deeplink(args: &[String], scheme_prefix: &str) -> Option<Deeplink> {
    let raw = args
        .iter()
        .find(|arg| arg.starts_with(scheme_prefix))?
        .clone();
    match Url::parse(&raw) {
        Ok(url) => {
            let action = url.host_str().unwrap_or_default().to_string();
            let mut params = Vec::new();
            let mut token = None;
            let mut doc = None;
            for (key, value) in url.query_pairs() {
                let value = value.to_string();
                if key == "token" {
                    token = Some(value.clone());
                }
                if key == "doc" {
                    doc = Some(value.clone());
                }
                params.push((key.to_string(), value));
            }
            Some(Deeplink {
                raw,
                action,
                params,
                token,
                doc,
            })
        }
        Err(err) => {
            warn!("Failed to parse deeplink '{raw}': {err}");
            None
        }
    }
}
