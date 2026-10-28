use crate::{Error, Result, message, model::spotify_id, now};
use oauth2::{CsrfToken, PkceCodeChallenge, PkceCodeVerifier};
use reqwest::{Client, Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use url::Url;

pub const READ_SCOPES: &str = "user-library-read playlist-read-private playlist-read-collaborative";
pub const SCOPES: &str =
    "user-library-read playlist-read-private playlist-read-collaborative playlist-modify-private";

pub fn random_secret() -> String {
    CsrfToken::new_random().secret().clone()
}

pub struct PendingAuth {
    pub state: String,
    pub verifier: PkceCodeVerifier,
    pub created: Instant,
}

pub fn authorization(client_id: &str, redirect: &Url) -> (Url, PendingAuth) {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let state = random_secret();
    let mut url = Url::parse("https://accounts.spotify.com/authorize").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect.as_str()),
        ("scope", SCOPES),
        ("state", &state),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge.as_str()),
    ]);
    (
        url,
        PendingAuth {
            state,
            verifier,
            created: Instant::now(),
        },
    )
}

pub fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(12))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .user_agent("Promptlist/0.1 (personal local library builder)")
        .build()
        .map_err(|_| Error::Network)
}

pub fn network_error(error: reqwest::Error) -> crate::Error {
    use std::error::Error as _;
    if error.is_timeout() {
        return message("Provider connection timed out. Retry later.");
    }
    let mut cause = error.source();
    while let Some(source) = cause {
        let text = source.to_string().to_lowercase();
        if text.contains("certificate") {
            return message(
                "Provider TLS certificate validation failed. Check the system trust configuration.",
            );
        }
        if text.contains("dns") {
            return message(
                "Provider hostname could not be resolved. Check the network connection.",
            );
        }
        cause = source.source();
    }
    if error.is_connect() {
        return message("Cannot connect to the provider. Check network and proxy settings.");
    }
    Error::Network
}

#[derive(Deserialize)]
pub struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
}

pub struct Tokens {
    access: String,
    refresh: String,
    expires_at: i64,
}

impl Tokens {
    fn from_reply(reply: TokenReply, previous: Option<&str>) -> Result<Self> {
        if reply.access_token.is_empty() {
            return Err(message("Spotify did not return an access token."));
        }
        let refresh = reply
            .refresh_token
            .or_else(|| previous.map(str::to_owned))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| message("Spotify did not return a refresh token. Reconnect."))?;
        if let Some(scopes) = reply.scope {
            let granted: HashSet<_> = scopes.split_whitespace().collect();
            if !SCOPES.split_whitespace().all(|s| granted.contains(s)) {
                return Err(message(
                    "Required library and private-playlist permissions were not granted. Reconnect to Spotify.",
                ));
            }
        }
        Ok(Self {
            access: reply.access_token,
            refresh,
            expires_at: now() + reply.expires_in.min(86400) as i64,
        })
    }
}

pub async fn exchange(
    http: &Client,
    client_id: &str,
    redirect: &Url,
    code: &str,
    verifier: PkceCodeVerifier,
) -> Result<Tokens> {
    let response = http
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier.secret()),
        ])
        .send()
        .await
        .map_err(network_error)?;
    let body = read_json(response, "Spotify authentication").await?;
    Tokens::from_reply(serde_json::from_value(body)?, None)
}

#[derive(Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}
impl Profile {
    pub fn account(&self) -> &str {
        self.account_id.as_deref().unwrap_or(&self.id)
    }
}

pub struct Spotify {
    api_base: String,
    token_url: String,
    http: Client,
    client_id: String,
    token: Mutex<Tokens>,
    gate: Arc<Mutex<Instant>>,
    quota: Arc<std::sync::atomic::AtomicBool>,
}

impl Spotify {
    pub fn new(
        http: Client,
        client_id: String,
        token: Tokens,
        gate: Arc<Mutex<Instant>>,
        quota: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            api_base: "https://api.spotify.com".into(),
            token_url: "https://accounts.spotify.com/api/token".into(),
            http,
            client_id,
            token: Mutex::new(token),
            gate,
            quota,
        }
    }
    pub fn acknowledge_quota(&self) {
        self.quota.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fixture(base: String) -> Self {
        let mut api = Self::new(
            http_client().unwrap(),
            "fixture-client".into(),
            Tokens {
                access: "fixture-token".into(),
                refresh: "fixture-refresh".into(),
                expires_at: now() + 3600,
            },
            Arc::new(Mutex::new(Instant::now())),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        api.api_base = base.clone();
        api.token_url = format!("{base}/token");
        api
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(Method::GET, path, None).await
    }

    /// Call only after confirmation. Persist Creating before calling and the returned
    /// dedicated playlist ID before filling. An ambiguous create must not be retried.
    pub async fn create_private_playlist(&self, name: &str) -> Result<String> {
        if name.trim().is_empty()
            || name.chars().count() > 100
            || name.chars().any(char::is_control)
        {
            return Err(message(
                "Playlist name must contain 1-100 characters without control characters.",
            ));
        }
        // https://developer.spotify.com/documentation/web-api/reference/create-playlist
        let body = self
            .request(
                Method::POST,
                "/v1/me/playlists",
                Some(serde_json::json!({
                    "name": name, "public": false, "collaborative": false
                })),
            )
            .await?;
        body["id"].as_str().filter(|id| spotify_id(id)).map(str::to_owned)
            .ok_or_else(|| message("Spotify returned no valid created playlist ID. Creation may have succeeded; do not create again blindly."))
    }

    /// Low-level full replacement, never append. The caller must supply only its
    /// persisted, dedicated Created receipt ID, never a user-selected playlist ID.
    /// Prefer fill_private_playlist for manual-edit protection on a Created retry.
    pub async fn replace_playlist_items(&self, id: &str, track_ids: &[String]) -> Result<()> {
        validate_playlist_write(id, track_ids)?;
        let uris: Vec<_> = track_ids
            .iter()
            .map(|id| format!("spotify:track:{id}"))
            .collect();
        // One PUT, at most 100 items; chunking PUT would discard preceding chunks.
        // https://developer.spotify.com/documentation/web-api/reference/reorder-or-replace-playlists-items
        let body = self
            .request(
                Method::PUT,
                &format!("/v1/playlists/{id}/items"),
                Some(serde_json::json!({"uris": uris})),
            )
            .await?;
        if body["snapshot_id"].as_str().is_none_or(str::is_empty) {
            return Err(message(
                "Spotify returned no replacement snapshot. Verify the existing created playlist before retrying.",
            ));
        }
        Ok(())
    }

    /// Fill only an empty dedicated Created playlist, or accept the exact previous
    /// result without a write. Spotify replacement has no documented compare-and-set;
    /// a remote edit between this preflight and PUT cannot be excluded atomically.
    pub async fn fill_private_playlist(&self, id: &str, track_ids: &[String]) -> Result<()> {
        validate_playlist_write(id, track_ids)?;
        let current = self.playlist_item_ids(id).await?;
        if current == track_ids {
            return Ok(());
        }
        if !current.is_empty() {
            return Err(message(
                "Created playlist contains different items. It may have been edited; nothing was replaced.",
            ));
        }
        self.replace_playlist_items(id, track_ids).await?;
        self.verify_playlist_items(id, track_ids).await
    }

    /// Exact ordered comparison, including duplicates. No normalization, filtering
    /// or deduplication may turn an unavailable or unexpected item into a match.
    pub async fn verify_playlist_items(&self, id: &str, track_ids: &[String]) -> Result<()> {
        validate_playlist_write(id, track_ids)?;
        if self.playlist_item_ids(id).await? != track_ids {
            return Err(message(
                "Spotify playlist readback differs from the approved track order or count.",
            ));
        }
        Ok(())
    }

    async fn playlist_item_ids(&self, id: &str) -> Result<Vec<String>> {
        let meta_path = format!("/v1/playlists/{id}?fields=snapshot_id");
        let before = self.get(&meta_path).await?;
        let snapshot = before["snapshot_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| message("Spotify playlist readback has no snapshot ID."))?;
        let mut ids = Vec::new();
        let mut offset = 0;
        let mut total = None;
        loop {
            let page = self
                .get(&format!(
                    "/v1/playlists/{id}/items?limit=50&offset={offset}"
                ))
                .await?;
            let done = crate::sync::advance_cursor(&page, &mut offset, &mut total)?;
            if total.is_none_or(|n| n > 100) {
                return Err(message(
                    "Created playlist exceeds the approved 100-item limit; nothing was replaced.",
                ));
            }
            for item in crate::sync::page_items(&page)? {
                let raw = item
                    .get("item")
                    .or_else(|| item.get("track"))
                    .unwrap_or(&Value::Null);
                let id = raw["id"].as_str().filter(|id| spotify_id(id));
                if raw["type"].as_str() != Some("track")
                    || id.is_none()
                    || item["is_local"].as_bool() == Some(true)
                    || raw["is_local"].as_bool() == Some(true)
                {
                    return Err(message(
                        "Spotify readback contains an unavailable or unexpected item.",
                    ));
                }
                let id = id.unwrap();
                if raw
                    .get("uri")
                    .is_some_and(|uri| uri.as_str() != Some(&format!("spotify:track:{id}")))
                {
                    return Err(message(
                        "Spotify readback contains an inconsistent track identity.",
                    ));
                }
                ids.push(id.to_owned());
            }
            if done {
                break;
            }
        }
        let after = self.get(&meta_path).await?;
        if after["snapshot_id"].as_str() != Some(snapshot) {
            return Err(message(
                "Spotify playlist changed during readback; verification failed.",
            ));
        }
        Ok(ids)
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        if !path.starts_with("/v1/") || path.contains("..") || path.contains('#') {
            return Err(message("Invalid Spotify API path."));
        }
        if self.quota.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Quota);
        }
        let mut gate = self.gate.lock().await;
        wait_gate(&gate, "Spotify").await?;
        let is_read = method == Method::GET;
        for attempt in 0..if is_read { 2 } else { 1 } {
            let mut token = self.token.lock().await;
            if token.expires_at <= now() + 30 {
                let response = self
                    .http
                    .post(&self.token_url)
                    .form(&[
                        ("client_id", self.client_id.as_str()),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", token.refresh.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(network_error)?;
                let body = read_json(response, "Spotify authentication").await?;
                *token = Tokens::from_reply(serde_json::from_value(body)?, Some(&token.refresh))?;
            }
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.api_base))
                .bearer_auth(&token.access);
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request.send()
                .await
                .map_err(|error| if is_read { network_error(error) } else {
                    message("Spotify write outcome is unknown. Do not repeat creation; reconcile the persisted receipt before retrying.")
                })?;
            *gate = Instant::now() + Duration::from_millis(350);
            if response.status() == StatusCode::UNAUTHORIZED {
                token.expires_at = 0;
                if is_read && attempt == 0 {
                    continue;
                }
            }
            drop(token);
            let result = read_json(response, "Spotify").await;
            match &result {
                Err(Error::RateLimit { seconds, .. }) => {
                    *gate = Instant::now() + Duration::from_secs(*seconds)
                }
                Err(Error::Quota) => self.quota.store(true, std::sync::atomic::Ordering::SeqCst),
                _ => {}
            }
            return result;
        }
        Err(message("Spotify session expired. Reconnect."))
    }
}

fn validate_playlist_write(id: &str, track_ids: &[String]) -> Result<()> {
    if !spotify_id(id) || track_ids.len() > 100 || track_ids.iter().any(|id| !spotify_id(id)) {
        return Err(message(
            "Invalid playlist ID or track list; at most 100 Spotify track IDs are allowed.",
        ));
    }
    Ok(())
}

async fn wait_gate(next: &Instant, provider: &'static str) -> Result<()> {
    let wait = next.saturating_duration_since(Instant::now());
    if wait > Duration::from_secs(2) {
        return Err(Error::RateLimit {
            provider,
            seconds: wait.as_secs() + 1,
        });
    }
    tokio::time::sleep(wait).await;
    Ok(())
}

pub fn classify_status(
    status: u16,
    retry: Option<u64>,
    body: &Value,
    provider: &'static str,
) -> Result<()> {
    if status == 429 {
        if provider == "Spotify" && body["error"]["reason"].as_str() == Some("QUOTA_EXCEEDED") {
            return Err(Error::Quota);
        }
        return Err(Error::RateLimit {
            provider,
            seconds: retry.unwrap_or(60).clamp(1, 86400),
        });
    }
    if !(200..300).contains(&status) {
        return Err(Error::Http { provider, status });
    }
    Ok(())
}

async fn read_json(mut response: reqwest::Response, provider: &'static str) -> Result<Value> {
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    if response.content_length().unwrap_or(0) > 5_000_000 {
        return Err(message("Provider response exceeded the safety limit."));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if chunk.len() > 5_000_000 - bytes.len() {
            return Err(message("Provider response exceeded the safety limit."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    classify_status(status, retry, &body, provider)?;
    if body.is_null() {
        return Err(message("Provider returned an invalid JSON response."));
    }
    Ok(body)
}

pub type FeatureMap = BTreeMap<String, BTreeMap<String, f64>>;

/// Values are available for the current request even when retention is forbidden.
/// Persist them only when retention_allowed AND expires_at > now().
pub struct FeatureBatch {
    pub features: FeatureMap,
    pub expires_at: i64,
    pub retention_allowed: bool,
}

pub struct Enrichment {
    http: Client,
    recco_url: String,
    #[cfg(test)]
    lastfm_url: String,
    key: Option<String>,
    recco_gate: Mutex<Instant>,
    lastfm_gate: Mutex<Instant>,
}

impl Enrichment {
    pub fn new(http: Client, key: Option<String>) -> Self {
        Self {
            http,
            recco_url: "https://api.reccobeats.com/v1/audio-features".into(),
            #[cfg(test)]
            lastfm_url: "https://ws.audioscrobbler.com/2.0/".into(),
            key,
            recco_gate: Mutex::new(Instant::now()),
            lastfm_gate: Mutex::new(Instant::now()),
        }
    }

    /// Synthetic credentials and loopback endpoints only; unavailable in production.
    #[cfg(test)]
    pub(crate) fn fixture(base: impl AsRef<str>) -> Self {
        let base = base.as_ref().trim_end_matches('/');
        Self::fixture_urls(format!("{base}/features"), format!("{base}/2.0/"))
    }

    #[cfg(test)]
    fn fixture_urls(recco_url: String, lastfm_url: String) -> Self {
        for endpoint in [&recco_url, &lastfm_url] {
            let url = Url::parse(endpoint).expect("valid mock URL");
            assert!(
                url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "mock providers must use explicit loopback HTTP endpoints"
            );
        }
        let mut api = Self::new(http_client().unwrap(), Some("fixture-key".into()));
        api.recco_url = recco_url;
        api.lastfm_url = lastfm_url;
        api
    }

    pub fn has_lastfm(&self) -> bool {
        self.key.is_some()
    }
    pub async fn features(&self, ids: &[String]) -> Result<FeatureMap> {
        // Compatibility for transient lab callers. Persistence must use the policy API.
        Ok(self.features_with_policy(ids).await?.features)
    }

    pub async fn features_with_policy(&self, ids: &[String]) -> Result<FeatureBatch> {
        if ids.is_empty() || ids.len() > 40 || ids.iter().any(|id| !spotify_id(id)) {
            return Err(message("Invalid ReccoBeats batch."));
        }
        let mut gate = self.recco_gate.lock().await;
        wait_gate(&gate, "ReccoBeats").await?;
        let response = self
            .http
            .get(&self.recco_url)
            .query(&[("ids", ids.join(","))])
            .send()
            .await
            .map_err(network_error)?;
        let received_at = now();
        let ttl = retention_ttl(response.headers(), received_at);
        *gate = Instant::now() + Duration::from_secs(1);
        let result = read_json(response, "ReccoBeats").await;
        if let Err(Error::RateLimit { seconds, .. }) = &result {
            *gate = Instant::now() + Duration::from_secs(*seconds);
        }
        Ok(FeatureBatch {
            features: parse_features(&result?, ids)?,
            expires_at: if ttl > 0 { received_at + ttl } else { 0 },
            retention_allowed: ttl > 0,
        })
    }

    /// Fresh, separately scoped track and artist evidence. No tags are cached here.
    /// Use tags() and artist_tags() separately to deduplicate artist calls within a build.
    pub async fn genre_tags(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let (track_tags, _, _) = self.tags(artist, track).await?;
        let artist_tags = self.artist_tags(artist).await?;
        Ok((track_tags, artist_tags))
    }

    /// Fresh artist-level evidence, not a claim about any individual recording.
    /// Call artist_tags_with_policy when retaining values beyond the active request.
    pub async fn artist_tags(&self, artist: &str) -> Result<Vec<String>> {
        Ok(self.artist_tags_with_policy(artist).await?.0)
    }

    pub async fn artist_tags_with_policy(&self, artist: &str) -> Result<(Vec<String>, i64)> {
        let (body, expires) = self.lastfm_tags(artist, None).await?;
        if body["error"].as_u64() == Some(6) {
            return Ok((vec![], expires));
        }
        if body.get("error").is_some() {
            return Err(message(
                "Last.fm rejected the tag request. Check the key or retry later.",
            ));
        }
        if body["toptags"]["@attr"]["artist"]
            .as_str()
            .map(normalized_tag_identity)
            != Some(normalized_tag_identity(artist))
        {
            return Err(message(
                "Last.fm returned an ambiguous artist identity; tags not accepted.",
            ));
        }
        Ok((parse_tag_names(&body)?, expires))
    }

    async fn lastfm_tags(&self, artist: &str, track: Option<&str>) -> Result<(Value, i64)> {
        if artist.trim().is_empty() || track.is_some_and(|s| s.trim().is_empty()) {
            return Err(message(
                "Last.fm requires a nonempty artist and track identity.",
            ));
        }
        let key = self
            .key
            .as_deref()
            .ok_or_else(|| message("LASTFM_API_KEY is not configured."))?;
        let mut query = vec![
            (
                "method",
                if track.is_some() {
                    "track.getTopTags"
                } else {
                    "artist.getTopTags"
                },
            ),
            ("artist", artist),
            ("api_key", key),
            ("format", "json"),
            ("autocorrect", "0"),
        ];
        if let Some(track) = track {
            query.push(("track", track));
        }
        #[cfg(not(test))]
        let endpoint = "https://ws.audioscrobbler.com/2.0/";
        #[cfg(test)]
        let endpoint = &self.lastfm_url;
        let mut gate = self.lastfm_gate.lock().await;
        wait_gate(&gate, "Last.fm").await?;
        // Pace failed network attempts too, and hold the same gate for both methods.
        *gate = Instant::now() + Duration::from_millis(250);
        drop(gate);
        let response = self
            .http
            .get(endpoint)
            .query(&query)
            .send()
            .await
            .map_err(network_error)?;
        let received_at = now();
        let ttl = lastfm_retention_ttl(response.headers(), received_at);
        let result = read_json(response, "Last.fm").await;
        if let Err(Error::RateLimit { seconds, .. }) = &result {
            let mut gate = self.lastfm_gate.lock().await;
            *gate = (*gate).max(Instant::now() + Duration::from_secs(*seconds));
        }
        let body = result?;
        if body["error"].as_u64() == Some(29) {
            let mut gate = self.lastfm_gate.lock().await;
            *gate = (*gate).max(Instant::now() + Duration::from_secs(60));
            return Err(Error::RateLimit {
                provider: "Last.fm",
                seconds: 60,
            });
        }
        Ok((body, received_at + ttl))
    }

    /// Live values remain usable even when expires_at does not permit retention.
    pub async fn tags(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<(Vec<String>, Option<String>, i64)> {
        let (body, expires_at) = self.lastfm_tags(artist, Some(track)).await?;
        if body.get("error").is_some() {
            if body["error"].as_u64() == Some(6)
                && body["message"]
                    .as_str()
                    .is_some_and(|s| s.to_lowercase().contains("track not found"))
            {
                return Ok((Vec::new(), None, expires_at));
            }
            return Err(message(
                "Last.fm rejected the tag request. Check the key or retry later.",
            ));
        }
        validate_tag_identity(&body, artist, track)?;
        let tags = parse_tag_names(&body)?;
        let link = body["toptags"]["tag"]
            .as_array()
            .and_then(|list| list.first())
            .and_then(|v| v["url"].as_str())
            .filter(|s| {
                Url::parse(s)
                    .is_ok_and(|u| u.scheme() == "https" && u.host_str() == Some("www.last.fm"))
            })
            .map(str::to_owned);
        Ok((tags, link, expires_at))
    }
}

fn normalized_tag_identity(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_tag_names(body: &Value) -> Result<Vec<String>> {
    let invalid = || message("Last.fm returned an unexpected tag response.");
    let list = body["toptags"]["tag"].as_array().ok_or_else(invalid)?;
    let mut tags = Vec::new();
    for (i, tag) in list.iter().enumerate() {
        let name = tag["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(invalid)?;
        if i < 20 {
            tags.push(name.to_owned());
        }
    }
    Ok(tags)
}

fn validate_tag_identity(body: &Value, artist: &str, track: &str) -> Result<()> {
    let attrs = &body["toptags"]["@attr"];
    if attrs["artist"].as_str().map(normalized_tag_identity)
        != Some(normalized_tag_identity(artist))
        || attrs["track"].as_str().map(normalized_tag_identity)
            != Some(normalized_tag_identity(track))
    {
        return Err(message(
            "Last.fm returned an ambiguous track identity; tags not accepted.",
        ));
    }
    Ok(())
}

pub fn cache_ttl(header: &str) -> i64 {
    let mut ttl = None;
    for part in header.split(',') {
        let (name, value) = part.trim().split_once('=').unwrap_or((part.trim(), ""));
        if name.trim().eq_ignore_ascii_case("no-store")
            || name.trim().eq_ignore_ascii_case("no-cache")
        {
            return 0;
        }
        if name.trim().eq_ignore_ascii_case("max-age") {
            let Ok(seconds) = value.trim().trim_matches('"').parse::<i64>() else {
                return 0;
            };
            // Conflicting freshness values fail conservatively to the shortest lifetime.
            ttl = Some(ttl.map_or(seconds, |old: i64| old.min(seconds)));
        }
    }
    ttl.unwrap_or(0).clamp(0, 86400)
}

fn lastfm_retention_ttl(headers: &reqwest::header::HeaderMap, received_at: i64) -> i64 {
    // Local freshness policy for licensed public tags, not a provider freshness claim.
    // Explicit restrictive headers always take precedence over heuristic caching.
    if headers.contains_key("pragma") {
        return 0;
    }
    if !headers.contains_key("cache-control")
        && !headers.contains_key("expires")
        && !headers.contains_key("pragma")
    {
        7 * 86400
    } else {
        retention_ttl(headers, received_at).min(7 * 86400)
    }
}

fn retention_ttl(headers: &reqwest::header::HeaderMap, received_at: i64) -> i64 {
    let mut cache = Vec::new();
    for value in headers.get_all("cache-control") {
        let Ok(value) = value.to_str() else { return 0 };
        cache.push(value);
    }
    let mut age = 0;
    for value in headers.get_all("age") {
        let Some(value) = value
            .to_str()
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n >= 0)
        else {
            return 0;
        };
        age = age.max(value);
    }
    if let Some(date) = headers.get("date") {
        let Some(date) = date
            .to_str()
            .ok()
            .and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok())
        else {
            return 0;
        };
        age = age.max(received_at.saturating_sub(date.timestamp()));
    }
    cache_ttl(&cache.join(",")).saturating_sub(age).max(0)
}

pub fn parse_features(
    body: &Value,
    ids: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, f64>>> {
    let records = body["content"]
        .as_array()
        .ok_or_else(|| message("ReccoBeats returned an unexpected response."))?;
    let mut output = BTreeMap::new();
    for record in records {
        let Some(href) = record["href"].as_str() else {
            continue;
        };
        let Ok(url) = Url::parse(href) else { continue };
        if url.scheme() != "https" || url.host_str() != Some("open.spotify.com") {
            continue;
        }
        let Some(id) = url.path().strip_prefix("/track/") else {
            continue;
        };
        if !ids.iter().any(|expected| expected == id) {
            continue;
        }
        let mut fields = BTreeMap::new();
        for field in [
            "acousticness",
            "danceability",
            "energy",
            "instrumentalness",
            "key",
            "liveness",
            "loudness",
            "mode",
            "speechiness",
            "tempo",
            "valence",
        ] {
            if let Some(value) = record[field].as_f64().filter(|v| v.is_finite()) {
                let valid = match field {
                    "key" => value.fract() == 0.0 && (0.0..=11.0).contains(&value),
                    "mode" => value == 0.0 || value == 1.0,
                    "tempo" => value > 0.0 && value <= 500.0,
                    "loudness" => (-100.0..=20.0).contains(&value),
                    _ => (0.0..=1.0).contains(&value),
                };
                if valid {
                    fields.insert(field.to_string(), value);
                }
            }
        }
        if output.insert(id.into(), fields).is_some() {
            return Err(message(
                "ReccoBeats returned duplicate identities; batch not accepted.",
            ));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rejects_remix_tag_mismatch() {
        let body = json!({"toptags":{"@attr":{"artist":"Artist","track":"Song (Remix)"}}});
        assert!(validate_tag_identity(&body, "Artist", "Song").is_err());
        assert!(validate_tag_identity(&body, "artist", "Song (Remix)").is_ok());
    }

    #[tokio::test]
    #[ignore = "Explicit opt-in: two public provider calls; Last.fm key from local configuration"]
    async fn live_public_providers() {
        let config = crate::config::Config::load(&std::env::current_dir().unwrap()).unwrap();
        let enrichment = Enrichment::new(http_client().unwrap(), config.lastfm_key);
        let features = enrichment
            .features(&["0ROMalDdNg3L2HoZP4qFyJ".into()])
            .await
            .unwrap();
        assert!(features["0ROMalDdNg3L2HoZP4qFyJ"].contains_key("tempo"));
        let (tags, _, _) = enrichment.tags("Radiohead", "Karma Police").await.unwrap();
        assert!(!tags.is_empty());
        println!(
            "Public Rust adapter smoke passed: numeric features and descriptive tags returned."
        );
    }

    #[tokio::test]
    async fn refreshes_after_401_and_keeps_refresh_token() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            routing::{get, post},
        };
        let router = Router::new()
            .route(
                "/v1/me",
                get(|headers: HeaderMap| async move {
                    if headers.get("authorization").and_then(|v| v.to_str().ok())
                        == Some("Bearer new-access")
                    {
                        (StatusCode::OK, Json(json!({"id":"user"})))
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error":{"status":401}})),
                        )
                    }
                }),
            )
            .route(
                "/token",
                post(|| async { Json(json!({"access_token":"new-access","expires_in":3600})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let api = Spotify::fixture(base);
        assert_eq!(api.get("/v1/me").await.unwrap()["id"], "user");
        assert_eq!(api.token.lock().await.refresh, "fixture-refresh");
        server.abort();
    }
    #[test]
    fn oauth_is_pkce_with_private_write_only() {
        let (url, p) = authorization(
            "client",
            &Url::parse("http://127.0.0.1:8787/callback").unwrap(),
        );
        let q: BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["state"], p.state);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["scope"], SCOPES);
        assert!(q["scope"].contains("playlist-modify-private"));
        assert!(!q["scope"].contains("playlist-modify-public"));
        assert_eq!(
            q["code_challenge"],
            PkceCodeChallenge::from_code_verifier_sha256(&p.verifier).as_str()
        );
    }
    #[test]
    fn preserves_refresh_token_when_not_rotated() {
        let t = Tokens::from_reply(
            TokenReply {
                access_token: "new".into(),
                refresh_token: None,
                expires_in: 3600,
                scope: None,
            },
            Some("old"),
        )
        .unwrap();
        assert_eq!(t.refresh, "old");
    }
    #[test]
    fn distinguishes_quota_from_temporary_rate_limit() {
        assert!(matches!(
            classify_status(
                429,
                Some(5),
                &json!({"error":{"reason":"QUOTA_EXCEEDED"}}),
                "Spotify"
            ),
            Err(Error::Quota)
        ));
        assert!(matches!(
            classify_status(429, Some(5), &json!({}), "Spotify"),
            Err(Error::RateLimit { seconds: 5, .. })
        ));
    }
    #[test]
    fn missing_retry_header_has_bounded_fallback() {
        assert!(matches!(
            classify_status(429, None, &Value::Null, "Last.fm"),
            Err(Error::RateLimit { seconds: 60, .. })
        ));
    }
    #[test]
    fn partial_feature_batch_joins_by_identity() {
        let a = "a".repeat(22);
        let b = "b".repeat(22);
        let result=parse_features(&json!({"content":[{"href":format!("https://open.spotify.com/track/{b}"),"tempo":120,"energy":0.5,"key":-1}]}),&[a.clone(),b.clone()]).unwrap();
        assert!(!result.contains_key(&a));
        assert_eq!(result[&b]["tempo"], 120.0);
        assert!(!result[&b].contains_key("key"));
    }
    #[test]
    fn ignores_outside_library_provider_results() {
        let a = "a".repeat(22);
        let result = parse_features(
            &json!({"content":[{"href":format!("https://evil.example/track/{a}"),"tempo":120}]}),
            &[a],
        )
        .unwrap();
        assert!(result.is_empty());
    }
    #[test]
    fn respects_restrictive_cache_headers() {
        assert_eq!(cache_ttl("public, max-age=300"), 300);
        assert_eq!(cache_ttl("no-store, max-age=1000"), 0);
        assert_eq!(cache_ttl(""), 0);
    }

    async fn mock(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (base, server)
    }

    fn lastfm_fixture(base: &str) -> Enrichment {
        Enrichment::fixture(base)
    }

    #[tokio::test]
    async fn enrichment_fixture_routes_both_providers_to_local_mocks() {
        use axum::{Json, Router, routing::get};
        let router = Router::new()
            .route("/features", get(|| async {
                Json(json!({"content":[{"href":format!("https://open.spotify.com/track/{}", "a".repeat(22)),"energy":0.9}]}))
            }))
            .route("/2.0/", get(|| async {
                Json(json!({"toptags":{"@attr":{"artist":"Artist"},"tag":[{"name":"metal"}]}}))
            }));
        let (base, server) = mock(router).await;
        let api = Enrichment::fixture(&base);
        assert_eq!(
            api.features(&["a".repeat(22)]).await.unwrap()[&"a".repeat(22)]["energy"],
            0.9
        );
        assert_eq!(api.artist_tags("Artist").await.unwrap(), ["metal"]);
        server.abort();
    }

    #[test]
    fn enrichment_fixture_rejects_nonlocal_endpoints() {
        let local = "http://127.0.0.1:1234/mock";
        for endpoint in [
            "https://ws.audioscrobbler.com/2.0/",
            "http://untrusted.example:1234/mock",
            "http://127.0.0.1/mock",
            "http://user:secret@127.0.0.1:1234/mock",
            "http://127.0.0.1:1234/mock?api_key=secret",
        ] {
            assert!(
                std::panic::catch_unwind(|| Enrichment::fixture_urls(
                    local.into(),
                    endpoint.into()
                ))
                .is_err()
            );
            assert!(
                std::panic::catch_unwind(|| Enrichment::fixture_urls(
                    endpoint.into(),
                    local.into()
                ))
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn genre_tags_are_live_separate_paced_and_not_cached() {
        use axum::{Json, Router, extract::Query, routing::get};
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let router = Router::new().route("/2.0/", get(move |Query(query): Query<BTreeMap<String, String>>| {
            let calls = observed.clone();
            async move {
                calls.lock().await.push((query.clone(), Instant::now()));
                let (attrs, name) = if query["method"] == "track.getTopTags" {
                    (json!({"artist":" ARTIST ","track":"Song (Remix)"}), "track genre")
                } else {
                    (json!({"artist":"artist"}), "artist genre")
                };
                ([("cache-control", "no-store")], Json(json!({"toptags":{"@attr":attrs,"tag":[{"name":name,"url":"https://untrusted.example/tag"}]}})))
            }
        }));
        let (base, server) = mock(router).await;
        let api = lastfm_fixture(&base);
        let (track, artist) = api.genre_tags("Artist", "Song (Remix)").await.unwrap();
        assert_eq!(track, ["track genre"]);
        assert_eq!(artist, ["artist genre"]);
        let (tags, link, expires) = api.tags("Artist", "Song (Remix)").await.unwrap();
        assert_eq!(tags, track);
        assert!(link.is_none());
        assert!(expires <= now());
        assert_eq!(api.artist_tags("Artist").await.unwrap(), artist);
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 4);
        for (query, _) in calls.iter() {
            assert_eq!(query["autocorrect"], "0");
            assert_eq!(query["api_key"], "fixture-key");
            assert_eq!(query["format"], "json");
            assert_eq!(query["artist"], "Artist");
            assert_eq!(
                query.contains_key("track"),
                query["method"] == "track.getTopTags"
            );
        }
        for pair in calls.windows(2) {
            assert!(pair[1].1.duration_since(pair[0].1) >= Duration::from_millis(200));
        }
        server.abort();
    }

    #[tokio::test]
    async fn artist_tags_validate_identity_and_response_shape() {
        use axum::{Json, Router, routing::get};
        for (body, valid) in [
            (
                json!({"toptags":{"@attr":{"artist":"  THE   Artist "},"tag":[{"name":"rock"}]}}),
                true,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"the artist"},"tag":[]}}),
                true,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"Different"},"tag":[{"name":"rock"}]}}),
                false,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"the artist tribute"},"tag":[]}}),
                false,
            ),
            (json!({"toptags":{"tag":[]}}), false),
            (
                json!({"toptags":{"@attr":{"artist":"the artist"},"tag":{}}}),
                false,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"the artist"},"tag":[{"name":7}]}}),
                false,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"the artist"},"tag":[{"name":" "}]}}),
                false,
            ),
            (
                json!({"toptags":{"@attr":{"artist":"the artist"},"tag":[{}]}}),
                false,
            ),
            (json!([]), false),
        ] {
            let router = Router::new().route(
                "/2.0/",
                get(move || {
                    let body = body.clone();
                    async move { Json(body) }
                }),
            );
            let (base, server) = mock(router).await;
            assert_eq!(
                lastfm_fixture(&base)
                    .artist_tags("The Artist")
                    .await
                    .is_ok(),
                valid
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn track_tags_reject_wrong_editions_and_missing_identity() {
        use axum::{Json, Router, routing::get};
        for (artist, track, tags) in [
            (json!("Artist"), json!("Song"), json!([{"name":"rock"}])),
            (
                json!("Other"),
                json!("Song (Remix)"),
                json!([{"name":"rock"}]),
            ),
            (Value::Null, Value::Null, json!([])),
        ] {
            let router = Router::new().route(
                "/2.0/",
                get(move || {
                    let body =
                        json!({"toptags":{"@attr":{"artist":artist,"track":track},"tag":tags}});
                    async move { Json(body) }
                }),
            );
            let (base, server) = mock(router).await;
            assert!(
                lastfm_fixture(&base)
                    .tags("Artist", "Song (Remix)")
                    .await
                    .is_err()
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn lastfm_errors_are_sanitized_and_cooldowns_shared() {
        use axum::{Json, Router, routing::get};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (status, body, cooldown) in [
            (
                200,
                json!({"error":29,"message":"private-response"}),
                Some(60),
            ),
            (429, json!({"message":"private-response"}), Some(7)),
            (200, json!({"error":10,"message":"private-response"}), None),
            (503, json!({"message":"private-response"}), None),
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let requests = count.clone();
            let router = Router::new().route(
                "/2.0/",
                get(move || {
                    let body = body.clone();
                    requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        (
                            StatusCode::from_u16(status).unwrap(),
                            [("retry-after", "7")],
                            Json(body),
                        )
                    }
                }),
            );
            let (base, server) = mock(router).await;
            let api = lastfm_fixture(&base);
            let error = api.artist_tags("Artist").await.unwrap_err();
            let text = format!("{error:?} {error}");
            for sensitive in ["private-response", "fixture-key", &base] {
                assert!(!text.contains(sensitive));
            }
            if let Some(expected) = cooldown {
                assert!(matches!(error, Error::RateLimit { seconds, .. } if seconds == expected));
                assert!(matches!(
                    api.tags("Artist", "Song").await,
                    Err(Error::RateLimit { .. })
                ));
            }
            assert_eq!(count.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }

    #[tokio::test]
    async fn lastfm_query_values_cannot_override_endpoint_or_parameters() {
        use axum::{Json, Router, extract::Query, routing::get};
        let artist = "https://untrusted.example/?method=evil&api_key=stolen";
        let router = Router::new().route("/2.0/", get(move |Query(query): Query<BTreeMap<String, String>>| async move {
            assert_eq!(query.len(), 5);
            assert_eq!(query["artist"], artist);
            assert_eq!(query["method"], "artist.getTopTags");
            assert_eq!(query["api_key"], "fixture-key");
            Json(json!({"toptags":{"@attr":{"artist":artist},"tag":[{"name":"rock","url":"https://untrusted.example"}]}}))
        }));
        let (base, server) = mock(router).await;
        assert_eq!(
            lastfm_fixture(&base).artist_tags(artist).await.unwrap(),
            ["rock"]
        );
        server.abort();
    }

    #[tokio::test]
    async fn lastfm_rejects_invalid_json_redirects_and_missing_inputs() {
        use axum::{Router, routing::get};
        for status in [200, 302] {
            let router = Router::new().route(
                "/2.0/",
                get(move || async move {
                    (
                        StatusCode::from_u16(status).unwrap(),
                        [("location", "https://untrusted.example")],
                        "private-invalid-json",
                    )
                }),
            );
            let (base, server) = mock(router).await;
            let error = lastfm_fixture(&base)
                .artist_tags("Artist")
                .await
                .unwrap_err();
            assert!(!format!("{error:?}").contains("private-invalid-json"));
            if status == 302 {
                assert!(matches!(error, Error::Http { status: 302, .. }));
            }
            server.abort();
        }
        let api = lastfm_fixture("http://127.0.0.1:1");
        assert!(
            api.artist_tags(" ")
                .await
                .unwrap_err()
                .to_string()
                .contains("nonempty")
        );
        assert!(
            api.tags("Artist", " ")
                .await
                .unwrap_err()
                .to_string()
                .contains("nonempty")
        );
        let mut api = api;
        api.key = None;
        assert!(
            api.artist_tags("Artist")
                .await
                .unwrap_err()
                .to_string()
                .contains("not configured")
        );
    }

    #[tokio::test]
    async fn lastfm_limits_bodies_with_and_without_content_length() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for chunked in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let headers: &[u8] = if chunked {
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5000001\r\n\r\n"
                };
                stream.write_all(headers).await.unwrap();
                if chunked {
                    let chunk = vec![b'x'; 1_000_000];
                    for _ in 0..6 {
                        if stream.write_all(b"f4240\r\n").await.is_err()
                            || stream.write_all(&chunk).await.is_err()
                            || stream.write_all(b"\r\n").await.is_err()
                        {
                            return;
                        }
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                }
            });
            let error = lastfm_fixture(&base)
                .artist_tags("Artist")
                .await
                .unwrap_err();
            assert!(error.to_string().contains("safety limit"));
            server.await.unwrap();
        }
    }

    #[test]
    fn tag_names_are_bounded_and_malformed_entries_not_silently_skipped() {
        let mut tags: Vec<_> = (0..25)
            .map(|i| json!({"name":format!("tag {i}")}))
            .collect();
        assert_eq!(
            parse_tag_names(&json!({"toptags":{"tag":tags}}))
                .unwrap()
                .len(),
            20
        );
        tags.push(json!({"name":null}));
        assert!(parse_tag_names(&json!({"toptags":{"tag":tags}})).is_err());
    }

    #[test]
    fn rejects_missing_private_write_permission() {
        assert!(
            Tokens::from_reply(
                TokenReply {
                    access_token: "fixture".into(),
                    refresh_token: Some("fixture".into()),
                    expires_in: 3600,
                    scope: Some(READ_SCOPES.into()),
                },
                None
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn creates_private_and_replaces_in_one_ordered_request() {
        use axum::{
            Json, Router,
            routing::{post, put},
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        let writes = calls.clone();
        let creates = calls.clone();
        let id = "p".repeat(22);
        let router = Router::new()
            .route(
                "/v1/me/playlists",
                post(move |Json(body): Json<Value>| {
                    let calls = creates.clone();
                    async move {
                        calls.lock().await.push(("POST", body));
                        (StatusCode::CREATED, Json(json!({"id":"p".repeat(22)})))
                    }
                }),
            )
            .route(
                &format!("/v1/playlists/{id}/items"),
                put(move |Json(body): Json<Value>| {
                    let calls = writes.clone();
                    async move {
                        calls.lock().await.push(("PUT", body));
                        Json(json!({"snapshot_id":"new"}))
                    }
                }),
            );
        let (base, server) = mock(router).await;
        let api = Spotify::fixture(base);
        assert_eq!(api.create_private_playlist("Fixture").await.unwrap(), id);
        let ids: Vec<_> = (0..100).map(|n| format!("{n:022}")).collect();
        api.replace_playlist_items(&id, &ids).await.unwrap();
        let calls = calls.lock().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1,
            json!({"name":"Fixture","public":false,"collaborative":false})
        );
        assert_eq!(
            calls[1].1["uris"],
            json!(
                ids.iter()
                    .map(|id| format!("spotify:track:{id}"))
                    .collect::<Vec<_>>()
            )
        );
        server.abort();
    }

    #[tokio::test]
    async fn invalid_write_arguments_never_send() {
        let api = Spotify::fixture("http://127.0.0.1:1".into());
        for name in ["", "  ", "bad\nname", &"x".repeat(101)] {
            assert!(
                api.create_private_playlist(name)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("name")
            );
        }
        for (id, tracks) in [
            ("bad".into(), vec![]),
            ("p".repeat(22), vec!["a".repeat(22); 101]),
            ("p".repeat(22), vec!["spotify:track:bad".into()]),
        ] {
            assert!(
                api.replace_playlist_items(&id, &tracks)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Invalid")
            );
        }
    }

    #[tokio::test]
    async fn mutations_never_retry_http_errors_and_hide_provider_bodies() {
        use axum::{Json, Router, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for status in [401, 403, 404, 429, 500] {
            for method in [Method::POST, Method::PUT] {
                let count = Arc::new(AtomicUsize::new(0));
                let requests = count.clone();
                let handler = move || {
                    let count = requests.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::from_u16(status).unwrap(),
                            [("retry-after", "7")],
                            Json(json!({"secret":"do-not-leak","error":{"message":"do-not-leak"}})),
                        )
                    }
                };
                let router = Router::new()
                    .route("/v1/me/playlists", post(handler.clone()))
                    .route(
                        "/v1/playlists/pppppppppppppppppppppp/items",
                        axum::routing::put(handler),
                    );
                let (base, server) = mock(router).await;
                let api = Spotify::fixture(base);
                let error = if method == Method::POST {
                    api.create_private_playlist("Fixture").await.map(|_| ())
                } else {
                    api.replace_playlist_items(&"p".repeat(22), &["a".repeat(22)])
                        .await
                }
                .unwrap_err();
                assert!(!format!("{error:?} {error}").contains("do-not-leak"));
                assert_eq!(count.load(Ordering::SeqCst), 1);
                if status == 401 {
                    assert_eq!(api.token.lock().await.expires_at, 0);
                }
                if status == 429 {
                    assert!(matches!(error, Error::RateLimit { seconds: 7, .. }));
                    assert!(api.create_private_playlist("Fixture").await.is_err());
                    assert_eq!(count.load(Ordering::SeqCst), 1);
                }
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn readback_rejects_unexpected_items_bad_pagination_and_snapshot_changes() {
        use axum::{Json, Router, routing::get};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let id = "p".repeat(22);
        let good = json!({"item":{"id":"a".repeat(22),"type":"track"}});
        for (items, total, next, changing) in [
            (vec![json!({"item":null})], 1, Value::Null, false),
            (
                vec![json!({"item":{"id":"a".repeat(22),"type":"episode"}})],
                1,
                Value::Null,
                false,
            ),
            (
                vec![json!({"is_local":true,"item":good["item"]})],
                1,
                Value::Null,
                false,
            ),
            (vec![good.clone()], 2, Value::Null, false),
            (vec![], 1, json!("https://untrusted.example"), false),
            (vec![good], 1, Value::Null, true),
        ] {
            let checks = Arc::new(AtomicUsize::new(0));
            let router = Router::new()
                .route(&format!("/v1/playlists/{id}"), get(move || {
                    let checks = checks.clone();
                    async move {
                        let n = checks.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"snapshot_id":if changing && n > 0 {"changed"} else {"before"}}))
                    }
                }))
                .route(&format!("/v1/playlists/{id}/items"), get(move || {
                    let items = items.clone(); let next = next.clone();
                    async move { Json(json!({"items":items,"total":total,"next":next})) }
                }));
            let (base, server) = mock(router).await;
            assert!(
                Spotify::fixture(base)
                    .verify_playlist_items(&id, &["a".repeat(22)])
                    .await
                    .is_err()
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn ambiguous_create_timeout_is_not_repeated() {
        use axum::{Json, Router, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let requests = count.clone();
        let router = Router::new().route(
            "/v1/me/playlists",
            post(move || {
                let count = requests.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Json(json!({"id":"p".repeat(22)}))
                }
            }),
        );
        let (base, server) = mock(router).await;
        let mut api = Spotify::fixture(base);
        api.http = Client::builder()
            .timeout(Duration::from_millis(100))
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let error = api.create_private_playlist("Fixture").await.unwrap_err();
        assert!(error.to_string().contains("outcome is unknown"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn guarded_fill_paginates_and_preserves_manual_edits() {
        use axum::{Json, Router, extract::Query, routing::get};
        let items = Arc::new(Mutex::new(Vec::<String>::new()));
        let reads = items.clone();
        let writes = items.clone();
        let puts = Arc::new(Mutex::new(0));
        let put_count = puts.clone();
        let id = "p".repeat(22);
        let router = Router::new()
            .route(&format!("/v1/playlists/{id}"), get(|| async { Json(json!({"snapshot_id":"fixture"})) }))
            .route(&format!("/v1/playlists/{id}/items"), get(move |Query(query): Query<BTreeMap<String,String>>| {
                let items = reads.clone();
                async move {
                    let items = items.lock().await;
                    let offset: usize = query["offset"].parse().unwrap();
                    let page: Vec<_> = items.iter().skip(offset).take(50).map(|id| json!({"item":{"id":id,"type":"track"}})).collect();
                    Json(json!({"items":page,"total":items.len(),"next":if offset + page.len() < items.len() {Some("https://untrusted.example")} else {None}}))
                }
            }).put(move |Json(body): Json<Value>| {
                let items = writes.clone(); let count = put_count.clone();
                async move {
                    *count.lock().await += 1;
                    *items.lock().await = body["uris"].as_array().unwrap().iter().map(|uri| uri.as_str().unwrap().strip_prefix("spotify:track:").unwrap().to_owned()).collect();
                    Json(json!({"snapshot_id":"filled"}))
                }
            }));
        let (base, server) = mock(router).await;
        let api = Spotify::fixture(base);
        let ids: Vec<_> = (0..51).map(|n| format!("{n:022}")).collect();
        api.fill_private_playlist(&id, &ids).await.unwrap();
        assert_eq!(*puts.lock().await, 1);
        api.fill_private_playlist(&id, &ids).await.unwrap();
        assert_eq!(*puts.lock().await, 1);
        items.lock().await.swap(0, 1);
        assert!(api.fill_private_playlist(&id, &ids).await.is_err());
        assert!(api.verify_playlist_items(&id, &ids).await.is_err());
        assert_eq!(*puts.lock().await, 1);
        *items.lock().await = vec![ids[0].clone(), ids[0].clone()];
        api.verify_playlist_items(&id, &[ids[0].clone(), ids[0].clone()])
            .await
            .unwrap();
        assert!(
            api.verify_playlist_items(&id, &[ids[0].clone()])
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn feature_policy_preserves_live_values_without_retention() {
        use axum::{Json, Router, routing::get};
        for (cache, age, retained) in [
            ("no-store, max-age=600", "0", false),
            ("", "0", false),
            ("max-age=120", "120", false),
            ("max-age=120", "20", true),
        ] {
            let router = Router::new().route("/features", get(move || async move {
                ([("cache-control",cache),("age",age)],Json(json!({"content":[{"href":format!("https://open.spotify.com/track/{}","a".repeat(22)),"tempo":120}]})))
            }));
            let (base, server) = mock(router).await;
            let mut api = Enrichment::new(http_client().unwrap(), None);
            api.recco_url = format!("{base}/features");
            let before = now();
            let result = api.features_with_policy(&["a".repeat(22)]).await.unwrap();
            assert_eq!(result.features[&"a".repeat(22)]["tempo"], 120.0);
            assert_eq!(result.retention_allowed, retained);
            if retained {
                assert!((before + 100..=now() + 100).contains(&result.expires_at));
            } else {
                assert_eq!(result.expires_at, 0);
            }
            assert_eq!(
                api.features(&["a".repeat(22)]).await.unwrap(),
                result.features
            );
            server.abort();
        }
    }

    #[test]
    fn retention_is_conservative_for_multiple_headers_and_stale_dates() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        headers.append(
            "cache-control",
            HeaderValue::from_static("MAX-AGE = \"600\""),
        );
        assert_eq!(retention_ttl(&headers, now()), 600);
        headers.append("cache-control", HeaderValue::from_static("No-Store"));
        assert_eq!(retention_ttl(&headers, now()), 0);
        headers.remove("cache-control");
        headers.insert("cache-control", HeaderValue::from_static("max-age=600"));
        headers.insert("age", HeaderValue::from_static("-1"));
        assert_eq!(retention_ttl(&headers, now()), 0);
        headers.remove("age");
        headers.insert(
            "date",
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retention_ttl(&headers, now()), 0);
        assert_eq!(cache_ttl("max-age=600,no-cache=\"field\""), 0);
        assert_eq!(cache_ttl("max-age=600,max-age=10"), 10);
    }
    #[test]
    fn public_tag_heuristic_cache_never_overrides_restrictive_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        assert_eq!(lastfm_retention_ttl(&h, now()), 7 * 86400);
        assert_eq!(
            retention_ttl(&h, now()),
            0,
            "Numeric policy must not inherit tag heuristic"
        );
        for rule in ["no-store", "no-cache", "max-age=0", "public"] {
            h.insert("cache-control", HeaderValue::from_str(rule).unwrap());
            assert_eq!(lastfm_retention_ttl(&h, now()), 0);
        }
        h.insert("cache-control", HeaderValue::from_static("max-age=600"));
        assert_eq!(lastfm_retention_ttl(&h, now()), 600);
        h.insert("pragma", HeaderValue::from_static("no-cache"));
        assert_eq!(lastfm_retention_ttl(&h, now()), 0);
    }
}
