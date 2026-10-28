use crate::{Result, message};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use url::Url;

pub struct Config {
    pub client_id: String,
    pub redirect: Url,
    pub lastfm_key: Option<String>,
    pub openai_key: Option<String>,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let entries = dotenvy::from_path_iter(root.join(".env.local"))
            .map_err(|_| message("Cannot read .env.local in the project directory."))?;
        let mut values = HashMap::new();
        for entry in entries {
            let (key, value) = entry.map_err(|_| message("Invalid .env.local syntax."))?;
            if values.insert(key, value).is_some() {
                return Err(message("Duplicate setting in .env.local."));
            }
        }
        Self::from_values(values, root.join(".data"))
    }

    pub fn from_values(values: HashMap<String, String>, data_dir: PathBuf) -> Result<Self> {
        let client_id = values.get("SPOTIFY_CLIENT_ID").cloned().unwrap_or_default();
        if client_id.len() != 32 || !client_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(message("SPOTIFY_CLIENT_ID is missing or invalid."));
        }
        let redirect = Url::parse(
            values
                .get("SPOTIFY_REDIRECT_URI")
                .map(String::as_str)
                .unwrap_or(""),
        )
        .map_err(|_| message("Invalid SPOTIFY_REDIRECT_URI."))?;
        if redirect.scheme() != "http"
            || redirect.host_str() != Some("127.0.0.1")
            || redirect.path() != "/callback"
            || redirect.port().is_none()
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err(message(
                "Local callback must be http://127.0.0.1:PORT/callback.",
            ));
        }
        let lastfm_key = values
            .get("LASTFM_API_KEY")
            .filter(|s| !s.is_empty())
            .cloned();
        let openai_key = values
            .get("OPENAI_API_KEY")
            .filter(|s| !s.is_empty())
            .cloned();
        Ok(Self {
            client_id,
            redirect,
            lastfm_key,
            openai_key,
            data_dir,
        })
    }

    pub fn origin(&self) -> String {
        self.redirect.origin().ascii_serialization()
    }
    pub fn port(&self) -> u16 {
        self.redirect.port().expect("validated port")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restricts_callback_to_exact_loopback_route() {
        for uri in [
            "https://example.com/callback",
            "http://localhost:8787/callback",
            "http://127.0.0.1:8787/other",
            "http://127.0.0.1:8787/callback?x=y",
        ] {
            assert!(
                Config::from_values(
                    HashMap::from([
                        ("SPOTIFY_CLIENT_ID".into(), "a".repeat(32)),
                        ("SPOTIFY_REDIRECT_URI".into(), uri.into())
                    ]),
                    PathBuf::new()
                )
                .is_err()
            );
        }
    }
}
