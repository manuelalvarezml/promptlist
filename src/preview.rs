use crate::{
    Result, message,
    model::{Snapshot, spotify_id},
    now, provider,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Phase {
    #[default]
    Draft,
    Creating,
    Created,
    Saved,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    #[serde(default)]
    pub run_id: Option<String>,
    pub id: String,
    pub name: String,
    pub track_ids: Vec<String>,
    pub criteria: Vec<String>,
    pub warnings: Vec<String>,
    pub snapshot_at: i64,
    pub created_at: i64,
    pub phase: Phase,
    pub playlist_id: Option<String>,
}

impl Preview {
    pub fn new(
        name: String,
        track_ids: Vec<String>,
        criteria: Vec<String>,
        warnings: Vec<String>,
        snapshot: &Snapshot,
    ) -> Result<Self> {
        let preview = Self {
            run_id: None,
            id: provider::random_secret(),
            name,
            track_ids,
            criteria,
            warnings,
            snapshot_at: snapshot
                .completed_at
                .ok_or_else(|| message("Complete a library sync first."))?,
            created_at: now(),
            phase: Phase::Draft,
            playlist_id: None,
        };
        preview.validate(snapshot)?;
        Ok(preview)
    }
    pub fn validate(&self, snapshot: &Snapshot) -> Result<()> {
        if snapshot.completed_at != Some(self.snapshot_at) || now() - self.created_at >= 86400 {
            return Err(message("Library or preview changed. Build a new preview."));
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.name.chars().any(char::is_control)
        {
            return Err(message("Playlist name must contain 1-100 characters."));
        }
        let unique: BTreeSet<_> = self.track_ids.iter().collect();
        if self.track_ids.len() > 100
            || unique.len() != self.track_ids.len()
            || self
                .track_ids
                .iter()
                .any(|id| !spotify_id(id) || !snapshot.tracks.contains_key(id))
        {
            return Err(message(
                "Preview must contain at most 100 unique tracks from this library.",
            ));
        }
        Ok(())
    }
    pub fn edit(&mut self, index: usize, action: &str) -> Result<()> {
        if self.phase != Phase::Draft || index >= self.track_ids.len() {
            return Err(message("This preview cannot be edited."));
        }
        match action {
            "remove" => {
                self.track_ids.remove(index);
            }
            "up" if index > 0 => self.track_ids.swap(index, index - 1),
            "down" if index + 1 < self.track_ids.len() => self.track_ids.swap(index, index + 1),
            _ => return Err(message("Invalid track action.")),
        }
        self.id = provider::random_secret();
        Ok(())
    }
    pub fn begin_save(&mut self, snapshot: &Snapshot) -> Result<()> {
        self.validate(snapshot)?;
        if self.track_ids.is_empty() {
            return Err(message("No tracks selected."));
        }
        match self.phase {
            Phase::Draft => self.phase = Phase::Creating,
            Phase::Created | Phase::Saved => {}
            Phase::Creating => {
                return Err(message(
                    "Spotify creation outcome is unknown. Check Spotify before creating another playlist; automatic retry is blocked.",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Snapshot, Preview) {
        let mut s = Snapshot {
            completed_at: Some(now()),
            ..Default::default()
        };
        for c in ['a', 'b'] {
            let id = c.to_string().repeat(22);
            s.tracks.insert(
                id.clone(),
                crate::model::Track {
                    id,
                    ..Default::default()
                },
            );
        }
        let p = Preview::new(
            "Test".into(),
            s.tracks.keys().cloned().collect(),
            vec![],
            vec![],
            &s,
        )
        .unwrap();
        (s, p)
    }
    #[test]
    fn rejects_nonlibrary_and_duplicate_tracks() {
        let (s, mut p) = fixture();
        p.track_ids.push("z".repeat(22));
        assert!(p.validate(&s).is_err());
        p.track_ids.pop();
        p.track_ids.push(p.track_ids[0].clone());
        assert!(p.validate(&s).is_err());
    }
    #[test]
    fn changed_snapshot_blocks_save() {
        let (mut s, mut p) = fixture();
        s.completed_at = Some(0);
        assert!(p.begin_save(&s).is_err());
    }
    #[test]
    fn ambiguous_create_cannot_repeat() {
        let (s, mut p) = fixture();
        p.begin_save(&s).unwrap();
        assert!(p.begin_save(&s).is_err());
    }
    #[test]
    fn edit_rotates_confirmation_nonce() {
        let (_, mut p) = fixture();
        let id = p.id.clone();
        p.edit(0, "down").unwrap();
        assert_ne!(id, p.id);
        assert!(p.track_ids[0].starts_with('b'));
        p.edit(1, "remove").unwrap();
        assert_eq!(p.track_ids.len(), 1);
    }
    #[test]
    fn created_preview_is_immutable_and_resumable() {
        let (s, mut p) = fixture();
        p.phase = Phase::Created;
        p.playlist_id = Some("c".repeat(22));
        assert!(p.edit(0, "remove").is_err());
        p.begin_save(&s).unwrap();
        assert_eq!(p.phase, Phase::Created);
    }
    #[test]
    fn empty_preview_cannot_publish() {
        let (s, mut p) = fixture();
        p.track_ids.clear();
        assert!(p.begin_save(&s).is_err());
    }
}
