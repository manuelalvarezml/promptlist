//! Bounded, opt-in coverage experiments. Never changes the active catalog.
use crate::{
    Result,
    config::Config,
    message,
    model::{Coverage, Snapshot, Track},
    now, provider,
};
use reqwest::{Client, header::HeaderMap};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CacheObservation {
    pub cache_control: String,
    pub date: Option<String>,
    pub expires: Option<String>,
    pub age_seconds: i64,
    pub etag_present: bool,
    pub effective_ttl: i64,
}
impl CacheObservation {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(200).collect::<String>())
        };
        let cache_control = get("cache-control").unwrap_or_default();
        let age_seconds = get("age")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        Self {
            effective_ttl: provider::cache_ttl(&cache_control)
                .saturating_sub(age_seconds)
                .max(0),
            cache_control,
            date: get("date"),
            expires: get("expires"),
            age_seconds,
            etag_present: headers.contains_key("etag"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct RequestMetric {
    pub provider: String,
    pub operation: String,
    pub status: u16,
    pub elapsed_ms: u128,
    pub cache: CacheObservation,
}

#[derive(Default, Serialize, Deserialize)]
pub struct LabReport {
    pub schema_version: u32,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub snapshot_at: Option<i64>,
    pub baseline_sample: usize,
    pub baseline_numeric: usize,
    pub numeric_misses: usize,
    pub valid_isrc_queries: usize,
    pub isrc_candidates: usize,
    pub identity_accepted: usize,
    pub identity_rejected: usize,
    pub feature_recoveries: usize,
    pub text_sample: usize,
    pub track_info_found: usize,
    pub track_info_tagged: usize,
    pub wiki_present: usize,
    pub wiki_with_fresh_cache: usize,
    pub text_identity_rejected: usize,
    pub unique_artist_queries: usize,
    pub artist_tagged_tracks: usize,
    pub artist_only_tagged_tracks: usize,
    pub text_either_tagged: usize,
    pub fresh_tagged_tracks: usize,
    pub requests: Vec<RequestMetric>,
    pub errors: Vec<String>,
}

struct LabHttp {
    client: Client,
    next: Instant,
    requests: Vec<RequestMetric>,
}
impl LabHttp {
    fn new() -> Result<Self> {
        Ok(Self {
            client: provider::http_client()?,
            next: Instant::now(),
            requests: vec![],
        })
    }
    async fn get(
        &mut self,
        provider: &'static str,
        operation: &str,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<(Value, CacheObservation)> {
        tokio::time::sleep(self.next.saturating_duration_since(Instant::now())).await;
        let start = Instant::now();
        let result = self.client.get(url).query(query).send().await;
        self.next = Instant::now() + Duration::from_millis(1100);
        let response = match result {
            Ok(r) => r,
            Err(error) => {
                self.requests.push(RequestMetric {
                    provider: provider.into(),
                    operation: operation.into(),
                    elapsed_ms: start.elapsed().as_millis(),
                    ..Default::default()
                });
                return Err(provider::network_error(error));
            }
        };
        let status = response.status().as_u16();
        let cache = CacheObservation::from_headers(response.headers());
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        self.requests.push(RequestMetric {
            provider: provider.into(),
            operation: operation.into(),
            status,
            elapsed_ms: start.elapsed().as_millis(),
            cache: cache.clone(),
        });
        if status == 429 {
            return Err(crate::Error::RateLimit {
                provider,
                seconds: retry,
            });
        }
        if status != 200 {
            return Err(crate::Error::Http { provider, status });
        }
        if response.content_length().is_some_and(|n| n > 5_000_000) {
            return Err(message("Provider response too large"));
        }
        let bytes = response.bytes().await.map_err(|_| crate::Error::Network)?;
        if bytes.len() > 5_000_000 {
            return Err(message("Provider response too large"));
        }
        let body: Value = serde_json::from_slice(&bytes)?;
        let not_found = body["error"].as_u64() == Some(6)
            && body["message"].as_str().is_some_and(|s| {
                let s = s.to_lowercase();
                s.contains("track not found") || s.contains("artist not found")
            });
        if body.get("error").is_some() && !not_found {
            return Err(message(format!("{provider} API error; experiment stopped")));
        }
        Ok((body, cache))
    }
}

fn normalized(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn valid_isrc(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_alphabetic)
        && b[2..5].iter().all(u8::is_ascii_alphanumeric)
        && b[5..].iter().all(u8::is_ascii_digit)
}
fn spotify_href(value: &Value) -> Option<String> {
    let u = url::Url::parse(value.as_str()?).ok()?;
    let id = u.path().strip_prefix("/track/")?;
    (u.scheme() == "https" && u.host_str() == Some("open.spotify.com") && provider_id(id))
        .then(|| id.to_owned())
}
fn provider_id(id: &str) -> bool {
    crate::model::spotify_id(id)
}

/// Deliberately exact edition/title matching; ISRC alone is insufficient.
pub fn candidate_matches(track: &Track, candidate: &Value) -> bool {
    let Some(isrc) = track.isrc.as_deref().filter(|s| valid_isrc(s)) else {
        return false;
    };
    let Some(duration) = track.duration_ms else {
        return false;
    };
    let Some(candidate_duration) = candidate["durationMs"].as_u64() else {
        return false;
    };
    let Some(title) = candidate["trackTitle"].as_str() else {
        return false;
    };
    let Some(artists) = candidate["artists"].as_array() else {
        return false;
    };
    let a: BTreeSet<_> = track.artists.iter().map(|a| normalized(a)).collect();
    let b: BTreeSet<_> = artists
        .iter()
        .filter_map(|a| a["name"].as_str())
        .map(normalized)
        .collect();
    candidate["isrc"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case(isrc))
        && normalized(&track.name) == normalized(title)
        && !a.is_empty()
        && a == b
        && duration.abs_diff(candidate_duration) <= 2000
        && spotify_href(&candidate["href"]).is_some()
}

fn load_baseline(db: &Connection) -> Result<(String, Snapshot, Coverage, String)> {
    let mut statement = db.prepare("SELECT s.account,s.payload,c.payload FROM records s JOIN records c ON s.account=c.account AND c.kind='coverage' WHERE s.kind='snapshot' AND s.updated>=? AND c.updated>=?")?;
    let rows = statement
        .query_map(params![now() - 86400, now() - 86400], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() != 1 {
        return Err(message(
            "Experiment requires exactly one fresh, completed account snapshot and coverage record",
        ));
    }
    let (account, raw, coverage) = rows.into_iter().next().unwrap();
    let snapshot: Snapshot = serde_json::from_str(&raw)?;
    let coverage: Coverage = serde_json::from_str(&coverage)?;
    if snapshot.completed_at.is_none()
        || coverage.completed_at.is_none()
        || coverage.sample.len() > 200
    {
        return Err(message("Complete the baseline coverage first"));
    }
    Ok((account, snapshot, coverage, raw))
}

fn save_report(
    db: &mut Connection,
    account: &str,
    snapshot_raw: &str,
    run_id: &str,
    report: &LabReport,
) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM records WHERE account=? AND kind='snapshot' AND payload=? AND updated>=?)", params![account,snapshot_raw,now()-86400], |r| r.get(0))?;
    if !exists {
        return Err(message(
            "Account disconnected or library changed; experiment not saved",
        ));
    }
    tx.execute("INSERT INTO records(account,kind,payload,updated) VALUES(?,?,?,?) ON CONFLICT(account,kind) DO UPDATE SET payload=excluded.payload,updated=excluded.updated",params![account,run_id,serde_json::to_string(report)?,now()])?;
    tx.commit()?;
    Ok(())
}

pub async fn run_coverage(root: &Path) -> Result<LabReport> {
    let config = Config::load(root)?;
    let key = config
        .lastfm_key
        .as_deref()
        .ok_or_else(|| message("LASTFM_API_KEY required"))?;
    let path = config.data_dir.join("catalog.sqlite");
    if config.data_dir.is_symlink() || path.is_symlink() {
        return Err(message("Refusing symlink for experiment database"));
    }
    let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA secure_delete=ON;")?;
    let (account, snapshot, coverage, raw) = load_baseline(&db)?;
    let mut report = LabReport {
        schema_version: 1,
        started_at: now(),
        snapshot_at: snapshot.completed_at,
        baseline_sample: coverage.sample.len(),
        ..Default::default()
    };
    let mut http = LabHttp::new()?;
    let run_id = format!("lab:{}:{}", now(), provider::random_secret());
    save_report(&mut db, &account, &raw, &run_id, &report)?;
    let result = async {
        let misses: Vec<_> = coverage
            .sample
            .iter()
            .filter_map(|id| snapshot.tracks.get(id))
            .filter(|track| {
                coverage
                    .tracks
                    .get(&track.id)
                    .is_none_or(|e| e.features.is_empty() && e.feature_count == 0)
            })
            .collect();
        report.numeric_misses = misses.len();
        report.baseline_numeric = report.baseline_sample - report.numeric_misses;
        let isrcs: BTreeSet<_> = misses
            .iter()
            .filter_map(|t| t.isrc.as_deref())
            .filter(|s| valid_isrc(s))
            .map(str::to_ascii_uppercase)
            .collect();
        report.valid_isrc_queries = isrcs.len();
        let isrcs: Vec<_> = isrcs.into_iter().collect();
        let mut candidates = vec![];
        for batch in isrcs.chunks(40) {
            let ids = batch.join(",");
            let (body, _) = http
                .get(
                    "ReccoBeats",
                    "isrc_metadata",
                    "https://api.reccobeats.com/v1/track",
                    &[("ids", &ids)],
                )
                .await?;
            candidates.extend(
                body["content"]
                    .as_array()
                    .ok_or_else(|| message("Unexpected ReccoBeats metadata"))?
                    .clone(),
            );
        }
        let mut accepted = vec![];
        for track in misses {
            let same_isrc: Vec<_> = candidates
                .iter()
                .filter(|c| {
                    c["isrc"]
                        .as_str()
                        .zip(track.isrc.as_deref())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                })
                .collect();
            if !same_isrc.is_empty() {
                report.isrc_candidates += 1;
            }
            // Multiple compatible IDs stay ambiguous until manually inspected.
            if same_isrc.len() == 1 && candidate_matches(track, same_isrc[0]) {
                accepted.push((
                    track.id.clone(),
                    spotify_href(&same_isrc[0]["href"]).unwrap(),
                ));
                report.identity_accepted += 1;
            } else if !same_isrc.is_empty() {
                report.identity_rejected += 1;
            }
        }
        for batch in accepted.chunks(40) {
            let ids: Vec<_> = batch.iter().map(|(_, id)| id.clone()).collect();
            let joined = ids.join(",");
            let (body, _) = http
                .get(
                    "ReccoBeats",
                    "isrc_features",
                    "https://api.reccobeats.com/v1/audio-features",
                    &[("ids", &joined)],
                )
                .await?;
            let features = provider::parse_features(&body, &ids)?;
            report.feature_recoveries += batch
                .iter()
                .filter(|(_, id)| features.get(id).is_some_and(|f| !f.is_empty()))
                .count();
        }
        report.requests = std::mem::take(&mut http.requests);
        save_report(&mut db, &account, &raw, &run_id, &report)?;
        println!(
            "Numeric branch: {} misses, {} checked matches, {} feature recoveries",
            report.numeric_misses, report.identity_accepted, report.feature_recoveries
        );

        // A paired, prefix-stable comparison to the original 40-track cohort.
        let mut artist_cache: BTreeMap<String, (bool, bool)> = BTreeMap::new();
        for id in coverage.sample.iter().take(40) {
            let track = snapshot
                .tracks
                .get(id)
                .ok_or_else(|| message("Baseline refers to absent track"))?;
            let Some(artist) = track.artists.first() else {
                continue;
            };
            report.text_sample += 1;
            let (body, cache) = http
                .get(
                    "Last.fm",
                    "track_info",
                    "https://ws.audioscrobbler.com/2.0/",
                    &[
                        ("method", "track.getInfo"),
                        ("artist", artist),
                        ("track", &track.name),
                        ("api_key", key),
                        ("autocorrect", "0"),
                        ("format", "json"),
                    ],
                )
                .await?;
            let info = &body["track"];
            let identity = info["name"]
                .as_str()
                .is_some_and(|n| normalized(n) == normalized(&track.name))
                && info["artist"]["name"]
                    .as_str()
                    .is_some_and(|n| normalized(n) == normalized(artist));
            let mut track_tagged = false;
            let mut track_fresh = false;
            if identity {
                report.track_info_found += 1;
                track_tagged = info["toptags"]["tag"]
                    .as_array()
                    .is_some_and(|tags| !tags.is_empty());
                report.track_info_tagged += usize::from(track_tagged);
                track_fresh = track_tagged && cache.effective_ttl > 0;
                let wiki = info["wiki"]["content"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty());
                report.wiki_present += usize::from(wiki);
                report.wiki_with_fresh_cache += usize::from(wiki && cache.effective_ttl > 0);
            } else if !info.is_null() {
                report.text_identity_rejected += 1;
            }
            let artist_key = normalized(artist);
            let (artist_tagged, artist_fresh) = if let Some(hit) = artist_cache.get(&artist_key) {
                *hit
            } else {
                report.unique_artist_queries += 1;
                let (body, cache) = http
                    .get(
                        "Last.fm",
                        "artist_tags",
                        "https://ws.audioscrobbler.com/2.0/",
                        &[
                            ("method", "artist.getTopTags"),
                            ("artist", artist),
                            ("api_key", key),
                            ("autocorrect", "0"),
                            ("format", "json"),
                        ],
                    )
                    .await?;
                let identity = body["toptags"]["@attr"]["artist"]
                    .as_str()
                    .is_some_and(|n| normalized(n) == artist_key);
                let tags = identity
                    && body["toptags"]["tag"]
                        .as_array()
                        .is_some_and(|t| !t.is_empty());
                let observation = (tags, tags && cache.effective_ttl > 0);
                // Only counts are reused within this run; no stale tags are reused.
                artist_cache.insert(artist_key, observation);
                observation
            };
            report.artist_tagged_tracks += usize::from(artist_tagged);
            report.artist_only_tagged_tracks += usize::from(artist_tagged && !track_tagged);
            report.text_either_tagged += usize::from(artist_tagged || track_tagged);
            report.fresh_tagged_tracks += usize::from(track_fresh || artist_fresh);
            report.requests.append(&mut http.requests);
            save_report(&mut db, &account, &raw, &run_id, &report)?;
            if report.text_sample.is_multiple_of(10) {
                println!("Text branch: {} / 40 checks", report.text_sample);
            }
        }
        Ok::<(), crate::Error>(())
    }
    .await;
    report.requests.append(&mut http.requests);
    match result {
        Ok(()) => report.completed_at = Some(now()),
        Err(e) => report.errors.push(e.to_string()),
    }
    save_report(&mut db, &account, &raw, &run_id, &report)?;
    Ok(report)
}

pub fn summary(report: &LabReport) -> Value {
    let mut by_operation = BTreeMap::<String, Value>::new();
    for r in &report.requests {
        let entry=by_operation.entry(r.operation.clone()).or_insert_with(||json!({"calls":0,"http_errors":0,"elapsed_ms":0,"fresh_responses":0,"cache_control":{},"expires_present":0}));
        entry["calls"] = json!(entry["calls"].as_u64().unwrap() + 1);
        entry["http_errors"] =
            json!(entry["http_errors"].as_u64().unwrap() + u64::from(r.status != 200));
        entry["elapsed_ms"] = json!(entry["elapsed_ms"].as_u64().unwrap() + r.elapsed_ms as u64);
        entry["fresh_responses"] = json!(
            entry["fresh_responses"].as_u64().unwrap() + u64::from(r.cache.effective_ttl > 0)
        );
        entry["expires_present"] = json!(
            entry["expires_present"].as_u64().unwrap() + u64::from(r.cache.expires.is_some())
        );
        let policy = if r.cache.cache_control.is_empty() {
            "(absent)"
        } else {
            &r.cache.cache_control
        };
        let count = entry["cache_control"][policy].as_u64().unwrap_or(0);
        entry["cache_control"][policy] = json!(count + 1);
    }
    let mut v = serde_json::to_value(report).expect("serializable report");
    v.as_object_mut().unwrap().remove("requests");
    v["request_summary"] = json!(by_operation);
    v
}

pub async fn public_cache_probe(root: &Path) -> Result<Value> {
    let config = Config::load(root)?;
    let key = config
        .lastfm_key
        .as_deref()
        .ok_or_else(|| message("LASTFM_API_KEY required"))?;
    let mut http = LabHttp::new()?;
    let mut observations = vec![];
    for (artist, track) in crate::text_evidence::PUBLIC_FIXTURES {
        for method in ["track.getTopTags", "track.getInfo", "artist.getTopTags"] {
            let mut query = vec![
                ("method", method),
                ("artist", artist),
                ("api_key", key),
                ("format", "json"),
                ("autocorrect", "0"),
            ];
            if method != "artist.getTopTags" {
                query.push(("track", track));
            }
            let (body, cache) = http
                .get(
                    "Last.fm",
                    method,
                    "https://ws.audioscrobbler.com/2.0/",
                    &query,
                )
                .await?;
            let tags = if method == "track.getInfo" {
                &body["track"]["toptags"]["tag"]
            } else {
                &body["toptags"]["tag"]
            };
            observations.push(json!({"public_artist":artist,"public_track":track,"method":method,
                "tag_count":tags.as_array().map_or(0,Vec::len),
                "wiki_present":body["track"]["wiki"]["content"].as_str().is_some_and(|s|!s.trim().is_empty()),
                "cache":cache}));
        }
    }
    Ok(
        json!({"observed_at":now(),"user_library_used":false,"observations":observations,"request_count":http.requests.len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Explicit opt-in: nine Last.fm calls on public fixtures; no library access"]
    async fn live_public_cache_probe() {
        let report = public_cache_probe(&std::env::current_dir().unwrap())
            .await
            .unwrap();
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
    fn fixture() -> (Track, Value) {
        (
            Track {
                name: "Title - Club Mix".into(),
                artists: vec!["Artist".into()],
                isrc: Some("USABC2400001".into()),
                duration_ms: Some(200000),
                ..Default::default()
            },
            json!({"trackTitle":"Title - Club Mix","artists":[{"name":"Artist"}],"isrc":"USABC2400001","durationMs":200900,"href":"https://open.spotify.com/track/aaaaaaaaaaaaaaaaaaaaaa"}),
        )
    }
    #[test]
    fn accepts_same_edition_with_bounded_duration_difference() {
        let (t, c) = fixture();
        assert!(candidate_matches(&t, &c));
    }
    #[test]
    fn rejects_wrong_edition_artist_duration_or_isrc() {
        let (t, c) = fixture();
        for (field, value) in [
            ("trackTitle", json!("Title")),
            ("artists", json!([{"name":"Other"}])),
            ("durationMs", json!(230000)),
            ("isrc", json!("USABC2400002")),
            (
                "href",
                json!("https://evil.example/track/aaaaaaaaaaaaaaaaaaaaaa"),
            ),
        ] {
            let mut bad = c.clone();
            bad[field] = value;
            assert!(!candidate_matches(&t, &bad));
        }
    }
    #[test]
    fn validates_isrc_without_stripping_information() {
        assert!(valid_isrc("USABC2400001"));
        for s in ["USABC240000X", "US-ABC-24-00001", "USABC24000", ""] {
            assert!(!valid_isrc(s));
        }
    }
    #[test]
    fn records_cache_policy_without_storing_body() {
        let mut h = HeaderMap::new();
        h.insert("cache-control", "max-age=120".parse().unwrap());
        h.insert("age", "20".parse().unwrap());
        assert_eq!(CacheObservation::from_headers(&h).effective_ttl, 100);
        h.insert("cache-control", "no-cache, max-age=120".parse().unwrap());
        assert_eq!(CacheObservation::from_headers(&h).effective_ttl, 0);
    }
    #[test]
    fn disconnect_cannot_be_undone_by_lab_receipt() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE records(account TEXT,kind TEXT,payload TEXT,updated INTEGER,PRIMARY KEY(account,kind));").unwrap();
        assert!(save_report(&mut db, "a", "{}", "lab:test", &LabReport::default()).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, usize>(0))
                .unwrap(),
            0
        );
    }
}
