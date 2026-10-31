use crate::{
    Error, Result, message,
    model::{Snapshot, Source, spotify_id},
    now,
    provider::Spotify,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub enum Stage {
    Liked,
    Inventory,
    Playlists,
    Finished,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Draft {
    pub snapshot: Snapshot,
    pub stage: Stage,
    pub offset: usize,
    pub playlist_index: usize,
    pub total: Option<usize>,
    pub started_at: i64,
    pub owner_id: String,
    pub liked_head: Option<Value>,
    pub liked_total: Option<usize>,
    #[serde(default)]
    pub previous: Option<Snapshot>,
    #[serde(default)]
    pub reused_sources: usize,
}

impl Draft {
    pub fn new(owner_id: String) -> Self {
        let liked = Source {
            id: "liked".into(),
            name: "Liked Songs".into(),
            accessible: true,
            ..Default::default()
        };
        Self {
            snapshot: Snapshot {
                sources: vec![liked],
                ..Default::default()
            },
            stage: Stage::Liked,
            offset: 0,
            playlist_index: 1,
            total: None,
            started_at: now(),
            owner_id,
            liked_head: None,
            liked_total: None,
            previous: None,
            reused_sources: 0,
        }
    }

    pub fn incremental(owner_id: String, previous: Option<Snapshot>) -> Self {
        Self {
            previous,
            ..Self::new(owner_id)
        }
    }

    pub fn progress(&self) -> String {
        match self.stage {
            Stage::Liked => format!("Liked Songs: {} items", self.offset),
            Stage::Inventory => format!(
                "Playlist inventory: {} sources",
                self.snapshot.sources.len() - 1
            ),
            Stage::Playlists => format!(
                "Playlists: {} / {}. {} unique tracks; {} unchanged reused",
                self.playlist_index.saturating_sub(1),
                self.snapshot.sources.len().saturating_sub(1),
                self.snapshot.tracks.len(),
                self.reused_sources
            ),
            Stage::Finished => format!(
                "Sync complete: {} unique tracks",
                self.snapshot.tracks.len()
            ),
        }
    }

    pub fn progress_counts(&self) -> (Option<usize>, Option<usize>) {
        match self.stage {
            Stage::Liked | Stage::Inventory => (self.total.map(|n| self.offset.min(n)), self.total),
            Stage::Playlists => (
                Some(self.playlist_index.saturating_sub(1)),
                Some(self.snapshot.sources.len().saturating_sub(1)),
            ),
            Stage::Finished => (Some(1), Some(1)),
        }
    }

    pub async fn advance(&mut self, spotify: &Spotify) -> Result<()> {
        if now() - self.started_at > 3600 {
            return Err(message(
                "Checkpoint is over one hour old. Start a new sync.",
            ));
        }
        match self.stage {
            Stage::Liked => {
                let page = spotify
                    .get(&format!("/v1/me/tracks?limit=50&offset={}", self.offset))
                    .await?;
                let items = page_items(&page)?;
                if self.offset == 0 {
                    self.liked_head = items.first().cloned();
                    self.liked_total = page["total"].as_u64().map(|n| n as usize);
                }
                self.snapshot.ingest(items, "liked", self.offset);
                self.snapshot.sources[0].count += items.len();
                if advance_cursor(&page, &mut self.offset, &mut self.total)? {
                    let check = spotify.get("/v1/me/tracks?limit=50&offset=0").await?;
                    if page_items(&check)?.first() != self.liked_head.as_ref()
                        || check["total"].as_u64().map(|n| n as usize) != self.liked_total
                    {
                        return Err(message(
                            "Liked Songs changed during sync. Start a new sync.",
                        ));
                    }
                    self.stage = Stage::Inventory;
                    self.offset = 0;
                    self.total = None;
                }
            }
            Stage::Inventory => {
                let page = spotify
                    .get(&format!("/v1/me/playlists?limit=50&offset={}", self.offset))
                    .await?;
                for item in page_items(&page)? {
                    let Some(id) = item["id"].as_str().filter(|id| spotify_id(id)) else {
                        continue;
                    };
                    if self.snapshot.sources.iter().any(|s| s.id == id) {
                        return Err(message(
                            "Playlist inventory changed during sync. Start a new sync.",
                        ));
                    }
                    // Inventory includes followed sources. Let the documented API decide
                    // access; owner/collaborative metadata is not an authorization check.
                    self.snapshot.sources.push(Source {
                        id: id.into(),
                        name: item["name"].as_str().unwrap_or("Untitled playlist").into(),
                        snapshot_id: item["snapshot_id"].as_str().map(str::to_owned),
                        accessible: true,
                        reason: None,
                        count: 0,
                    });
                }
                if self.snapshot.sources.len() > 2001 {
                    return Err(message("Safety cap reached: 2,000 playlists."));
                }
                if advance_cursor(&page, &mut self.offset, &mut self.total)? {
                    self.stage = Stage::Playlists;
                    self.offset = 0;
                    self.total = None;
                }
            }
            Stage::Playlists => {
                if self.playlist_index >= self.snapshot.sources.len() {
                    self.stage = Stage::Finished;
                    self.snapshot.completed_at = Some(now());
                    return Ok(());
                }
                let source = self.snapshot.sources[self.playlist_index].clone();
                // Also attempt sources pre-excluded by an older resumed checkpoint.
                // Revalidate on every checkpoint, including a resumed partial playlist.
                if let Some(expected) = &source.snapshot_id {
                    match spotify
                        .get(&format!("/v1/playlists/{}?fields=snapshot_id", source.id))
                        .await
                    {
                        Ok(meta) if meta["snapshot_id"].as_str() != Some(expected) => {
                            return Err(message(
                                "A playlist changed during sync. Start a new sync.",
                            ));
                        }
                        Err(Error::Http {
                            status: status @ (403 | 404),
                            ..
                        }) => {
                            self.exclude_current(unavailable_reason(status));
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                        _ => {}
                    }
                }
                if self.offset == 0 && self.reuse_unchanged(&source) {
                    if self.snapshot.tracks.len() > 50_000 {
                        return Err(message("Library exceeds the 50,000-track safety limit."));
                    }
                    self.playlist_index += 1;
                    self.total = None;
                    return Ok(());
                }
                let page = match spotify
                    .get(&format!(
                        "/v1/playlists/{}/items?limit=50&offset={}",
                        source.id, self.offset
                    ))
                    .await
                {
                    Ok(page) => page,
                    Err(Error::Http {
                        status: status @ (403 | 404),
                        ..
                    }) => {
                        self.exclude_current(unavailable_reason(status));
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                let mut offset = self.offset;
                let mut total = self.total;
                let done = advance_cursor(&page, &mut offset, &mut total)?;
                if done && let Some(expected) = &source.snapshot_id {
                    let meta = match spotify
                        .get(&format!("/v1/playlists/{}?fields=snapshot_id", source.id))
                        .await
                    {
                        Ok(meta) => meta,
                        Err(Error::Http {
                            status: status @ (403 | 404),
                            ..
                        }) => {
                            self.exclude_current(unavailable_reason(status));
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    if meta["snapshot_id"].as_str() != Some(expected) {
                        return Err(message("A playlist changed during sync. Start a new sync."));
                    }
                }
                self.snapshot
                    .ingest(page_items(&page)?, &source.id, self.offset);
                let current = &mut self.snapshot.sources[self.playlist_index];
                current.count += page_items(&page)?.len();
                current.accessible = true;
                current.reason = None;
                self.offset = offset;
                self.total = total;
                if done {
                    self.playlist_index += 1;
                    self.offset = 0;
                    self.total = None;
                }
            }
            Stage::Finished => {}
        }
        if self.snapshot.tracks.len() > 50_000 {
            return Err(message("Safety cap reached: 50,000 unique tracks."));
        }
        Ok(())
    }

    fn reuse_unchanged(&mut self, source: &Source) -> bool {
        let Some(previous) = &self.previous else {
            return false;
        };
        let Some(old) = previous.sources.iter().find(|s| {
            s.id == source.id
                && s.accessible
                && s.snapshot_id.is_some()
                && s.snapshot_id == source.snapshot_id
        }) else {
            return false;
        };
        self.snapshot.sources[self.playlist_index].count = old.count;
        for (id, track) in &previous.tracks {
            let memberships: Vec<_> = track
                .memberships
                .iter()
                .filter(|m| m.source == source.id)
                .cloned()
                .collect();
            if memberships.is_empty() {
                continue;
            }
            self.snapshot
                .tracks
                .entry(id.clone())
                .and_modify(|t| t.memberships.extend(memberships.clone()))
                .or_insert_with(|| {
                    let mut t = track.clone();
                    t.memberships = memberships;
                    t
                });
        }
        self.reused_sources += 1;
        true
    }

    fn exclude_current(&mut self, reason: &str) {
        let source = &mut self.snapshot.sources[self.playlist_index];
        source.accessible = false;
        source.reason = Some(reason.into());
        source.count = 0;
        let id = source.id.clone();
        self.snapshot.tracks.retain(|_, t| {
            t.memberships.retain(|m| m.source != id);
            !t.memberships.is_empty()
        });
        self.playlist_index += 1;
        self.offset = 0;
        self.total = None;
    }
}

fn unavailable_reason(status: u16) -> &'static str {
    match status {
        403 => {
            "Spotify denied playlist access (HTTP 403). Item access may require ownership or collaboration."
        }
        _ => "Spotify playlist unavailable or not found (HTTP 404).",
    }
}

pub fn page_items(page: &Value) -> Result<&Vec<Value>> {
    page["items"]
        .as_array()
        .ok_or_else(|| message("Spotify response has no items array; sync not promoted."))
}

pub fn advance_cursor(page: &Value, offset: &mut usize, total: &mut Option<usize>) -> Result<bool> {
    let count = page_items(page)?.len();
    let found = page["total"]
        .as_u64()
        .ok_or_else(|| message("Spotify response has no total; sync not promoted."))?
        as usize;
    if total.is_some_and(|old| old != found) {
        return Err(message(
            "Library size changed during pagination. Start a new sync.",
        ));
    }
    *total = Some(found);
    let next = page
        .get("next")
        .ok_or_else(|| message("Spotify response has no next cursor."))?;
    let done = next.is_null();
    if count > 50
        || (!done && (count == 0 || !next.is_string()))
        || *offset + count > found
        || (done && *offset + count != found)
    {
        return Err(message(
            "Inconsistent Spotify pagination; sync not promoted.",
        ));
    }
    *offset += count;
    if *offset > 100_000 {
        return Err(message("Source exceeds the 100,000-item safety limit."));
    }
    Ok(done)
}

// Stable round-robin across membership strata avoids sampling only recent likes.
pub fn sample(snapshot: &Snapshot, limit: usize) -> Vec<String> {
    let mut groups = [Vec::new(), Vec::new(), Vec::new()];
    for (id, track) in &snapshot.tracks {
        let liked = track.memberships.iter().any(|m| m.source == "liked");
        let playlist = track.memberships.iter().any(|m| m.source != "liked");
        groups[match (liked, playlist) {
            (true, true) => 2,
            (true, false) => 0,
            _ => 1,
        }]
        .push(id.clone());
    }
    let mut result = Vec::new();
    for i in 0..snapshot.tracks.len() {
        for group in &groups {
            if result.len() == limit {
                return result;
            }
            if let Some(id) = group.get(i) {
                result.push(id.clone());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn mock_http_sync_handles_new_contract_and_excluded_sources() {
        use axum::{Json, Router, routing::get};
        let track = json!({"item":{"id":"aaaaaaaaaaaaaaaaaaaaaa","type":"track","name":"Fixture","artists":[{"name":"Artist"}],"album":{}}});
        let router=Router::new()
            .route("/v1/me/tracks",get(||async {Json(json!({"items":[],"next":null,"total":0}))}))
            .route("/v1/me/playlists",get(||async {Json(json!({"items":[{"id":"bbbbbbbbbbbbbbbbbbbbbb","name":"Owned","owner":{"id":"owner"},"snapshot_id":"s1"},{"id":"cccccccccccccccccccccc","name":"Followed","owner":{"id":"other"}}],"next":null,"total":2}))}))
            .route("/v1/playlists/bbbbbbbbbbbbbbbbbbbbbb",get(||async {Json(json!({"snapshot_id":"s1"}))}))
            .route("/v1/playlists/bbbbbbbbbbbbbbbbbbbbbb/items",get(move ||{let t=track.clone();async move {Json(json!({"items":[t],"next":null,"total":1}))}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let api = Spotify::fixture(base);
        let mut draft = Draft::new("owner".into());
        for _ in 0..10 {
            if draft.stage == Stage::Finished {
                break;
            }
            draft.advance(&api).await.unwrap();
        }
        assert_eq!(draft.stage, Stage::Finished);
        assert_eq!(draft.snapshot.tracks.len(), 1);
        assert!(!draft.snapshot.sources[2].accessible);
        assert!(
            draft.snapshot.sources[2]
                .reason
                .as_ref()
                .unwrap()
                .contains("404")
        );
        assert_eq!(
            draft.snapshot.tracks.values().next().unwrap().memberships[0].source,
            "bbbbbbbbbbbbbbbbbbbbbb"
        );
        server.abort();
    }

    #[tokio::test]
    async fn followed_and_collaborative_sources_are_attempted_with_pagination() {
        use axum::{
            Json, Router,
            extract::{Path, Query},
            routing::get,
        };
        use std::{collections::BTreeMap, sync::Arc};
        use tokio::sync::Mutex;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let calls = seen.clone();
        let followed = "f".repeat(22);
        let collaborative = "c".repeat(22);
        let router = Router::new()
            .route("/v1/me/playlists", get(|| async {
                Json(json!({"items":[
                    {"id":"f".repeat(22),"name":"Followed","owner":{"id":"other"},"collaborative":false},
                    {"id":"c".repeat(22),"name":"Collaborative","owner":{"id":"other"},"collaborative":true}
                ],"total":2,"next":null}))
            }))
            .route("/v1/playlists/{id}/items", get(move |Path(id): Path<String>, Query(query): Query<BTreeMap<String,String>>| {
                let calls = calls.clone();
                async move {
                    let offset: usize = query["offset"].parse().unwrap();
                    calls.lock().await.push((id.clone(),offset));
                    let count = if id.starts_with('f') { 51 } else { 1 };
                    let items: Vec<_> = (offset..count.min(offset+50)).map(|n| json!({"item":{
                        "id":format!("{n:022}"),"type":"track","name":"Fixture","artists":[],"album":{}
                    }})).collect();
                    Json(json!({"items":items,"total":count,"next":if offset+items.len()<count {Some("https://not-followed.example")} else {None}}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = Spotify::fixture(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut draft = Draft::new("owner".into());
        draft.stage = Stage::Inventory;
        for _ in 0..6 {
            draft.advance(&api).await.unwrap();
        }
        assert_eq!(draft.stage, Stage::Finished);
        assert_eq!(draft.snapshot.tracks.len(), 51);
        assert_eq!(draft.snapshot.sources[1].count, 51);
        assert_eq!(draft.snapshot.sources[2].count, 1);
        assert_eq!(
            draft.snapshot.tracks[&format!("{:022}", 0)]
                .memberships
                .len(),
            2
        );
        assert_eq!(
            *seen.lock().await,
            vec![(followed.clone(), 0), (followed, 50), (collaborative, 0)]
        );
        assert!(
            draft
                .snapshot
                .sources
                .iter()
                .all(|s| s.accessible && s.reason.is_none())
        );
        server.abort();
    }

    #[tokio::test]
    async fn actual_denials_at_each_read_stage_remove_only_unavailable_memberships() {
        use axum::{Json, Router, http::StatusCode, routing::get};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for status in [403, 404] {
            for denial_stage in ["before", "items", "after"] {
                let checks = Arc::new(AtomicUsize::new(0));
                let reads = checks.clone();
                let router = Router::new()
                    .route(
                        "/v1/playlists/pppppppppppppppppppppp",
                        get(move || {
                            let checks = reads.clone();
                            async move {
                                let n = checks.fetch_add(1, Ordering::SeqCst);
                                if denial_stage == "before" || (denial_stage == "after" && n > 0) {
                                    (
                                        StatusCode::from_u16(status).unwrap(),
                                        Json(json!({"error":"private-provider-body"})),
                                    )
                                } else {
                                    (StatusCode::OK, Json(json!({"snapshot_id":"s1"})))
                                }
                            }
                        }),
                    )
                    .route(
                        "/v1/playlists/pppppppppppppppppppppp/items",
                        get(move || async move {
                            if denial_stage == "items" {
                                (StatusCode::from_u16(status).unwrap(), Json(json!({})))
                            } else {
                                (
                                    StatusCode::OK,
                                    Json(json!({"items":[],"total":1,"next":null})),
                                )
                            }
                        }),
                    );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let api = Spotify::fixture(format!("http://{}", listener.local_addr().unwrap()));
                let server =
                    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
                let mut draft = Draft::new("owner".into());
                draft.stage = Stage::Playlists;
                draft.offset = 1;
                draft.total = Some(1);
                let id = "p".repeat(22);
                draft.snapshot.sources.push(Source {
                    id: id.clone(),
                    snapshot_id: Some("s1".into()),
                    accessible: true,
                    count: 1,
                    ..Default::default()
                });
                let item = json!({"item":{"id":"a".repeat(22),"type":"track","name":"Fixture","artists":[],"album":{}}});
                draft
                    .snapshot
                    .ingest(std::slice::from_ref(&item), "liked", 0);
                draft.snapshot.ingest(&[item], &id, 0);
                draft.advance(&api).await.unwrap();
                let source = &draft.snapshot.sources[1];
                assert!(!source.accessible);
                assert_eq!(source.count, 0);
                assert!(
                    source
                        .reason
                        .as_ref()
                        .unwrap()
                        .contains(&status.to_string())
                );
                assert!(
                    !source
                        .reason
                        .as_ref()
                        .unwrap()
                        .contains("private-provider-body")
                );
                assert_eq!(draft.snapshot.tracks.len(), 1);
                assert_eq!(
                    draft
                        .snapshot
                        .tracks
                        .values()
                        .next()
                        .unwrap()
                        .memberships
                        .len(),
                    1
                );
                assert_eq!(draft.playlist_index, 2);
                assert_eq!(draft.offset, 0);
                assert_eq!(draft.total, None);
                server.abort();
            }
        }
    }
    #[test]
    fn pagination_rejects_silent_truncation() {
        assert!(
            advance_cursor(
                &json!({"items":[],"next":null,"total":10}),
                &mut 0,
                &mut None
            )
            .is_err()
        );
    }
    #[test]
    fn pagination_rejects_shifted_totals() {
        assert!(
            advance_cursor(
                &json!({"items":[],"next":null,"total":0}),
                &mut 0,
                &mut Some(1)
            )
            .is_err()
        );
    }
    #[test]
    fn empty_library_is_valid() {
        assert!(
            advance_cursor(
                &json!({"items":[],"next":null,"total":0}),
                &mut 0,
                &mut None
            )
            .unwrap()
        );
    }
    #[test]
    fn cursor_uses_item_count_not_untrusted_next_url() {
        let mut offset = 0;
        assert!(
            !advance_cursor(
                &json!({"items":[{},{}],"next":"https://evil.example","total":3}),
                &mut offset,
                &mut None
            )
            .unwrap()
        );
        assert_eq!(offset, 2);
    }
    #[test]
    fn excluded_partial_source_cannot_leave_ghost_memberships() {
        let mut d = Draft::new("u".into());
        d.snapshot.sources.push(Source {
            id: "p".into(),
            ..Default::default()
        });
        d.snapshot.tracks.insert(
            "t".into(),
            crate::model::Track {
                memberships: vec![crate::model::Membership {
                    source: "p".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        d.exclude_current("denied");
        assert!(d.snapshot.tracks.is_empty());
    }
    #[test]
    fn sample_is_bounded_deterministic_and_library_only() {
        let mut s = Snapshot::default();
        for i in 0..30 {
            s.tracks.insert(i.to_string(), Default::default());
        }
        let a = sample(&s, 20);
        assert_eq!(a, sample(&s, 20));
        assert_eq!(a.len(), 20);
        assert!(a.iter().all(|id| s.tracks.contains_key(id)));
    }
    #[tokio::test]
    async fn incremental_sync_reuses_unchanged_sources_and_reconciles_removals() {
        use axum::{Json, Router, extract::Path, routing::get};
        use std::sync::{Arc, Mutex};
        let p = "p".repeat(22);
        let c = "c".repeat(22);
        let removed = "r".repeat(22);
        let item = |i: usize| json!({"track":{"id":format!("{i:022}"),"type":"track","name":"Synthetic","artists":[],"album":{}}});
        let mut previous = Snapshot {
            sources: vec![
                Source {
                    id: "liked".into(),
                    accessible: true,
                    count: 1,
                    ..Default::default()
                },
                Source {
                    id: p.clone(),
                    accessible: true,
                    count: 1,
                    snapshot_id: Some("one".into()),
                    ..Default::default()
                },
                Source {
                    id: c.clone(),
                    accessible: true,
                    count: 1,
                    snapshot_id: Some("one".into()),
                    ..Default::default()
                },
                Source {
                    id: removed.clone(),
                    accessible: true,
                    count: 1,
                    snapshot_id: Some("one".into()),
                    ..Default::default()
                },
            ],
            completed_at: Some(now() - 100),
            ..Default::default()
        };
        for (i, source) in [
            (1, p.clone()),
            (2, c.clone()),
            (3, "liked".into()),
            (6, removed.clone()),
        ] {
            previous.ingest(&[item(i)], &source, 0);
        }
        let calls = Arc::new(Mutex::new(vec![]));
        let reads = calls.clone();
        let liked = item(4);
        let changed = item(5);
        let router=Router::new()
            .route("/v1/me/tracks",get(move ||{let liked=liked.clone();async move {Json(json!({"items":[liked],"total":1,"next":null}))}}))
            .route("/v1/me/playlists",get(move ||{let p=p.clone();let c=c.clone();async move {Json(json!({"items":[{"id":p,"name":"Unchanged renamed","snapshot_id":"one"},{"id":c,"name":"Changed","snapshot_id":"two"}],"total":2,"next":null}))}}))
            .route("/v1/playlists/{id}",get(|Path(id):Path<String>|async move {Json(json!({"snapshot_id":if id.starts_with('p') {"one"} else {"two"}}))}))
            .route("/v1/playlists/{id}/items",get(move |Path(id):Path<String>|{let reads=reads.clone();let changed=changed.clone();async move {reads.lock().unwrap().push(id);Json(json!({"items":[changed],"total":1,"next":null}))}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = Spotify::fixture(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut draft = Draft::incremental("owner".into(), Some(previous));
        for _ in 0..20 {
            if draft.stage == Stage::Finished {
                break;
            }
            draft.advance(&api).await.unwrap();
        }
        assert_eq!(draft.stage, Stage::Finished);
        assert_eq!(draft.reused_sources, 1);
        assert_eq!(*calls.lock().unwrap(), vec!["c".repeat(22)]);
        assert_eq!(
            draft.snapshot.tracks.keys().cloned().collect::<Vec<_>>(),
            vec![
                format!("{:022}", 1),
                format!("{:022}", 4),
                format!("{:022}", 5)
            ]
        );
        assert!(!draft.snapshot.sources.iter().any(|s| s.id == removed));
        assert_eq!(draft.snapshot.sources[1].name, "Unchanged renamed");
        server.abort();
    }
}
