pub mod config;
pub mod history;
pub mod intent;
pub mod lab;
pub mod model;
pub mod preferences;
pub mod preview;
pub mod provider;
pub mod refinement;
pub mod selection;
pub mod store;
pub mod sync;
pub mod text_evidence;
pub mod ui;
pub mod web;

pub type Result<T> = std::result::Result<T, Error>;

// Never include provider bodies, request URLs or secrets in user-facing errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("Local storage failed. Your previous complete snapshot is unchanged.")]
    Storage(#[from] rusqlite::Error),
    #[error("The provider response did not match the expected format.")]
    Json(#[from] serde_json::Error),
    #[error("{provider} request failed (HTTP {status}).")]
    Http { provider: &'static str, status: u16 },
    #[error("{provider} is rate limited. Retry after at least {seconds} seconds.")]
    RateLimit {
        provider: &'static str,
        seconds: u64,
    },
    #[error("Spotify developer quota exhausted. Sync paused; resume only when quota is available.")]
    Quota,
    #[error("Connection failed or timed out. Retry when the provider is available.")]
    Network,
}

pub fn message(text: impl Into<String>) -> Error {
    Error::Message(text.into())
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
