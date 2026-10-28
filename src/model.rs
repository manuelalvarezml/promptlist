use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Membership {
    pub source: String,
    pub added_at: Option<String>,
    pub position: usize,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub duration_ms: Option<u64>,
    pub explicit: Option<bool>,
    pub isrc: Option<String>,
    pub release_date: Option<String>,
    pub release_precision: Option<String>,
    pub image: Option<String>,
    pub memberships: Vec<Membership>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub snapshot_id: Option<String>,
    pub accessible: bool,
    pub reason: Option<String>,
    pub count: usize,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Snapshot {
    pub tracks: BTreeMap<String, Track>,
    pub sources: Vec<Source>,
    pub skipped_items: usize,
    pub completed_at: Option<i64>,
}

pub fn spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn parse_track(item: &Value, source: &str, position: usize) -> Option<Track> {
    let raw = item.get("item").or_else(|| item.get("track"))?;
    if item["is_local"].as_bool() == Some(true)
        || raw["is_local"].as_bool() == Some(true)
        || raw["type"].as_str() != Some("track")
        || raw["is_playable"].as_bool() == Some(false)
        || raw
            .get("restrictions")
            .is_some_and(|r| r.get("reason").is_some())
    {
        return None;
    }
    let id = raw["id"].as_str()?.to_owned();
    if !spotify_id(&id) {
        return None;
    }
    let image = raw["album"]["images"]
        .as_array()
        .and_then(|images| images.last())
        .and_then(|v| v["url"].as_str())
        .filter(|s| {
            url::Url::parse(s)
                .is_ok_and(|u| u.scheme() == "https" && u.host_str() == Some("i.scdn.co"))
        })
        .map(str::to_owned);
    Some(Track {
        id,
        name: raw["name"].as_str()?.into(),
        artists: raw["artists"]
            .as_array()?
            .iter()
            .filter_map(|a| a["name"].as_str().map(str::to_owned))
            .collect(),
        duration_ms: raw["duration_ms"].as_u64(),
        explicit: raw["explicit"].as_bool(),
        isrc: raw["external_ids"]["isrc"].as_str().map(str::to_owned),
        release_date: raw["album"]["release_date"].as_str().map(str::to_owned),
        release_precision: raw["album"]["release_date_precision"]
            .as_str()
            .map(str::to_owned),
        image,
        memberships: vec![Membership {
            source: source.into(),
            added_at: item["added_at"].as_str().map(str::to_owned),
            position,
        }],
    })
}

impl Snapshot {
    pub fn ingest(&mut self, items: &[Value], source: &str, offset: usize) {
        for (position, item) in items.iter().enumerate() {
            if let Some(track) = parse_track(item, source, offset + position) {
                self.tracks
                    .entry(track.id.clone())
                    .and_modify(|old| old.memberships.extend(track.memberships.clone()))
                    .or_insert(track);
            } else {
                self.skipped_items += 1;
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Evidence {
    pub recco_checked: bool,
    pub features: BTreeMap<String, f64>,
    #[serde(default)]
    pub features_expires_at: i64,
    #[serde(default)]
    pub feature_count: usize,
    pub lastfm_checked: bool,
    pub tags: Vec<String>,
    #[serde(default)]
    pub artist_tags: Vec<String>,
    pub lastfm_url: Option<String>,
    #[serde(default)]
    pub lastfm_expires_at: i64,
    #[serde(default)]
    pub tag_count: usize,
    pub updated_at: i64,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Coverage {
    pub sample: Vec<String>,
    pub tracks: BTreeMap<String, Evidence>,
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub include_tags: bool,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CachedTags {
    pub tags: Vec<String>,
    pub expires_at: i64,
    pub checked_at: i64,
}

pub fn tag_key(artist: &str, track: Option<&str>) -> String {
    serde_json::to_string(&(
        artist.trim().to_lowercase(),
        track.map(|t| t.trim().to_lowercase()),
    ))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub fn fixture() -> Value {
        json!({"track":{"id":"aaaaaaaaaaaaaaaaaaaaaa","type":"track","name":"Song","artists":[{"name":"Artist"}],"album":{}},"added_at":"2026-01-01T00:00:00Z"})
    }
    #[test]
    fn preserves_unknowns() {
        let t = parse_track(&fixture(), "liked", 0).unwrap();
        assert_eq!(t.explicit, None);
        assert_eq!(t.release_date, None);
    }
    #[test]
    fn normalizes_new_item_contract() {
        let mut v = fixture();
        v["item"] = v["track"].take();
        assert!(parse_track(&v, "liked", 0).is_some());
    }
    #[test]
    fn rejects_episodes_and_local_and_null() {
        for v in [
            json!({"track":null}),
            json!({"track":{"type":"episode"}}),
            json!({"is_local":true,"track":fixture()["track"]}),
        ] {
            assert!(parse_track(&v, "liked", 0).is_none());
        }
    }
    #[test]
    fn merges_membership_without_merging_editions() {
        let mut s = Snapshot::default();
        s.ingest(&[fixture()], "liked", 0);
        s.ingest(&[fixture()], "playlist", 3);
        assert_eq!(s.tracks.len(), 1);
        assert_eq!(s.tracks.values().next().unwrap().memberships.len(), 2);
    }
}
