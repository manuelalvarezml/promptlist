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
    pub allowed_users: Vec<String>,
    listen_port: u16,
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let mut values = HashMap::new();
        if root.join(".env.local").exists() {
            let entries = dotenvy::from_path_iter(root.join(".env.local"))
                .map_err(|_| message("Cannot read .env.local in the project directory."))?;
            for entry in entries {
                let (key, value) = entry.map_err(|_| message("Invalid .env.local syntax."))?;
                if values.insert(key, value).is_some() {
                    return Err(message("Duplicate setting in .env.local."));
                }
            }
        }
        for key in [
            "SPOTIFY_CLIENT_ID",
            "SPOTIFY_REDIRECT_URI",
            "LASTFM_API_KEY",
            "OPENAI_API_KEY",
            "PROMPTLIST_MODE",
            "PROMPTLIST_PORT",
            "PROMPTLIST_DATA_DIR",
            "PROMPTLIST_ALLOWED_USERS",
        ] {
            if let Ok(value) = std::env::var(key) {
                values.insert(key.into(), value);
            }
        }
        let data_dir = values
            .get("PROMPTLIST_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(".data"));
        Self::from_values(values, data_dir)
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
        let hosted = match values
            .get("PROMPTLIST_MODE")
            .map(String::as_str)
            .unwrap_or("local")
        {
            "local" => false,
            "hosted" => true,
            _ => return Err(message("PROMPTLIST_MODE must be local or hosted.")),
        };
        let valid_origin = if hosted {
            redirect.scheme() == "https"
                && redirect.host_str().is_some()
                && redirect.port().is_none()
        } else {
            redirect.scheme() == "http"
                && redirect.host_str() == Some("127.0.0.1")
                && redirect.port().is_some()
        };
        if !valid_origin
            || redirect.path() != "/callback"
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err(message(
                "Callback must be http://127.0.0.1:PORT/callback locally or https://HOST/callback in hosted mode.",
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
        let allowed_users: Vec<String> = values
            .get("PROMPTLIST_ALLOWED_USERS")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if hosted && allowed_users.is_empty() {
            return Err(message(
                "Hosted mode requires PROMPTLIST_ALLOWED_USERS (Spotify account IDs).",
            ));
        }
        let listen_port = values
            .get("PROMPTLIST_PORT")
            .map(|v| v.parse::<u16>())
            .transpose()
            .map_err(|_| message("Invalid PROMPTLIST_PORT."))?
            .unwrap_or(8787);
        if listen_port == 0 {
            return Err(message("PROMPTLIST_PORT must not be zero."));
        }
        Ok(Self {
            client_id,
            redirect,
            lastfm_key,
            openai_key,
            data_dir,
            allowed_users,
            listen_port,
        })
    }

    pub fn origin(&self) -> String {
        self.redirect.origin().ascii_serialization()
    }
    pub fn port(&self) -> u16 {
        if self.hosted() {
            self.listen_port
        } else {
            self.redirect.port().expect("validated port")
        }
    }
    pub fn hosted(&self) -> bool {
        self.redirect.scheme() == "https"
    }
    pub fn host_header(&self) -> &str {
        &self.redirect[url::Position::BeforeHost..url::Position::AfterPort]
    }
    pub fn allows_user(&self, account: &str) -> bool {
        !self.hosted() || self.allowed_users.iter().any(|id| id == account)
    }
    pub fn session_cookie(&self, id: &str) -> String {
        format!(
            "promptlist_session={id}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{}",
            if self.hosted() { "; Secure" } else { "" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hosted_requires_https_and_an_explicit_allowlist() {
        let mut values = HashMap::from([
            ("SPOTIFY_CLIENT_ID".into(), "a".repeat(32)),
            (
                "SPOTIFY_REDIRECT_URI".into(),
                "https://promptlist.example/callback".into(),
            ),
            ("PROMPTLIST_MODE".into(), "hosted".into()),
        ]);
        assert!(Config::from_values(values.clone(), PathBuf::new()).is_err());
        values.insert("PROMPTLIST_ALLOWED_USERS".into(), "owner, tester".into());
        let config = Config::from_values(values.clone(), PathBuf::new()).unwrap();
        assert!(config.allows_user("owner"));
        assert!(!config.allows_user("outsider"));
        assert_eq!(config.host_header(), "promptlist.example");
        assert_eq!(config.port(), 8787);
        assert!(config.session_cookie("test").ends_with("; Secure"));
        for uri in [
            "http://promptlist.example/callback",
            "https://promptlist.example/callback?x=1",
            "https://user:pass@promptlist.example/callback",
            "https://promptlist.example:8443/callback",
        ] {
            values.insert("SPOTIFY_REDIRECT_URI".into(), uri.into());
            assert!(Config::from_values(values.clone(), PathBuf::new()).is_err());
        }
    }
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
