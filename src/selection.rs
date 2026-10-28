//! Pure, deterministic hard-filter selection over supplied library evidence.
//! Coverage must be request-scoped or retention-checked by the caller. No IO occurs here.
use crate::{
    Result, message,
    model::{Coverage, Snapshot, Track},
};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range<T> {
    pub min: Option<T>,
    pub max: Option<T>,
}

impl<T: PartialOrd + Copy> Range<T> {
    pub fn contains(&self, value: T) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
    fn valid(&self, low: T, high: T) -> bool {
        (self.min.is_some() || self.max.is_some())
            && self.min.is_none_or(|v| v >= low && v <= high)
            && self.max.is_none_or(|v| v >= low && v <= high)
            && !matches!((self.min, self.max), (Some(a), Some(b)) if a > b)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Explicit {
    #[default]
    Any,
    CleanOnly,
    ExplicitOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenreMatchScope {
    TrackOnly,
    #[default]
    TrackOrArtist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergyPreset {
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestContext {
    Workout,
    ToFailure,
}

/// Auditable product vocabulary, not an inferred music taxonomy.
pub fn supported_genres() -> &'static [&'static str] {
    &[
        "hardcore metal",
        "metalcore",
        "metal",
        "hardcore",
        "rock",
        "pop",
        "hip hop",
        "rap",
        "jazz",
        "classical",
        "electronic",
        "house",
        "techno",
        "ambient",
        "reggaeton",
        "punk",
        "drum and bass",
        "trance",
        "disco",
        "soul",
    ]
}

fn normalize_tag(value: &str) -> String {
    value
        .to_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// OR within each group, AND across groups. Unknown names never mean unconstrained.
pub fn genre_groups_for(genre: &str) -> Option<Vec<Vec<String>>> {
    let groups: Vec<&[&str]> = match normalize_tag(genre).as_str() {
        "hardcore metal" | "harcore metal" => vec![
            &[
                "metal",
                "heavy metal",
                "thrash metal",
                "death metal",
                "metalcore",
                "deathcore",
                "hardcore metal",
            ],
            &[
                "hardcore",
                "hardcore punk",
                "metalcore",
                "deathcore",
                "hardcore metal",
            ],
        ],
        "metalcore" => vec![&["metalcore", "melodic metalcore"]],
        "metal" => vec![&[
            "metal",
            "heavy metal",
            "thrash metal",
            "death metal",
            "black metal",
            "power metal",
            "progressive metal",
            "metalcore",
            "deathcore",
            "hardcore metal",
        ]],
        "hardcore" | "harcore" => {
            vec![&["hardcore", "hardcore punk", "metalcore", "hardcore metal"]]
        }
        "rock" => vec![&[
            "rock",
            "alternative rock",
            "hard rock",
            "indie rock",
            "progressive rock",
            "punk rock",
        ]],
        "pop" => vec![&["pop", "synthpop", "synth pop", "indie pop", "dance pop"]],
        "hip hop" | "hiphop" => vec![&["hip hop", "hiphop", "rap"]],
        "rap" => vec![&["rap", "hip hop", "hiphop"]],
        "jazz" => vec![&["jazz", "bebop", "smooth jazz", "jazz fusion"]],
        "classical" => vec![&["classical", "baroque", "orchestral"]],
        "electronic" => vec![&[
            "electronic",
            "electronica",
            "house",
            "techno",
            "trance",
            "drum and bass",
        ]],
        "house" => vec![&[
            "house",
            "deep house",
            "progressive house",
            "tech house",
            "electro house",
        ]],
        "techno" => vec![&["techno", "minimal techno", "detroit techno"]],
        "ambient" => vec![&["ambient", "dark ambient"]],
        "reggaeton" => vec![&["reggaeton", "reggaetón"]],
        "punk" => vec![&["punk", "punk rock", "hardcore punk"]],
        "drum and bass" | "dnb" => vec![&["drum and bass", "drum & bass", "dnb"]],
        "trance" => vec![&["trance", "progressive trance", "psytrance"]],
        "disco" => vec![&["disco", "nu disco"]],
        "soul" => vec![&["soul", "neo soul"]],
        _ => return None,
    };
    Some(
        groups
            .into_iter()
            .map(|g| g.iter().map(|s| (*s).into()).collect())
            .collect(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Acousticness,
    Danceability,
    Energy,
    Instrumentalness,
    Key,
    Liveness,
    Loudness,
    Mode,
    Speechiness,
    Tempo,
    Valence,
}

impl Feature {
    pub const ALL: [Self; 11] = [
        Self::Acousticness,
        Self::Danceability,
        Self::Energy,
        Self::Instrumentalness,
        Self::Key,
        Self::Liveness,
        Self::Loudness,
        Self::Mode,
        Self::Speechiness,
        Self::Tempo,
        Self::Valence,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acousticness => "acousticness",
            Self::Danceability => "danceability",
            Self::Energy => "energy",
            Self::Instrumentalness => "instrumentalness",
            Self::Key => "key",
            Self::Liveness => "liveness",
            Self::Loudness => "loudness",
            Self::Mode => "mode",
            Self::Speechiness => "speechiness",
            Self::Tempo => "tempo",
            Self::Valence => "valence",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("bpm") {
            return Some(Self::Tempo);
        }
        Self::ALL
            .into_iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(value))
    }
    fn bounds(self) -> (f64, f64) {
        match self {
            Self::Tempo => (0.0, 500.0),
            Self::Loudness => (-100.0, 20.0),
            Self::Key => (0.0, 11.0),
            _ => (0.0, 1.0),
        }
    }
    fn valid_value(self, value: f64) -> bool {
        let (low, high) = self.bounds();
        value.is_finite()
            && value >= low
            && value <= high
            && (self != Self::Tempo || value > 0.0)
            && (!matches!(self, Self::Key | Self::Mode) || value.fract() == 0.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Criteria {
    pub source_ids: Vec<String>,
    pub artist_contains: Option<String>,
    pub title_contains: Option<String>,
    pub explicit: Explicit,
    pub release_year: Option<Range<u16>>,
    /// Saved in Liked Songs, not playlist-added dates. Calendar year in UTC.
    pub saved_year: Option<Range<u16>>,
    pub duration_ms: Option<Range<u64>>,
    pub features: BTreeMap<Feature, Range<f64>>,
    pub genre_groups: Vec<Vec<String>>,
    pub genre_match_scope: GenreMatchScope,
    pub energy_preset: Option<EnergyPreset>,
    pub context: Vec<RequestContext>,
    pub limit: usize,
}

impl Default for Criteria {
    fn default() -> Self {
        Self {
            source_ids: vec![],
            artist_contains: None,
            title_contains: None,
            explicit: Explicit::Any,
            release_year: None,
            saved_year: None,
            duration_ms: None,
            features: BTreeMap::new(),
            genre_groups: vec![],
            genre_match_scope: GenreMatchScope::default(),
            energy_preset: None,
            context: vec![],
            limit: 30,
        }
    }
}

impl Criteria {
    pub fn genre_needed(&self) -> bool {
        !self.genre_groups.is_empty()
    }
    pub fn is_genre(&self) -> bool {
        self.genre_needed()
    }
    pub fn is_numeric(&self) -> bool {
        !self.features.is_empty()
    }
    pub fn validate(&self) -> Result<()> {
        let allowed: BTreeSet<String> = supported_genres()
            .iter()
            .flat_map(|g| genre_groups_for(g).unwrap().into_iter().flatten())
            .collect();
        if self.genre_groups.len() > 8
            || self
                .genre_groups
                .iter()
                .any(|g| g.is_empty() || g.len() > 16 || g.iter().any(|t| !allowed.contains(t)))
        {
            return Err(message(
                "Genre groups must use the bounded static tag mapping; clarify unmapped genres.",
            ));
        }
        if self.energy_preset == Some(EnergyPreset::High)
            && self.features.get(&Feature::Energy)
                != Some(&Range {
                    min: Some(0.7),
                    max: None,
                })
        {
            return Err(message(
                "High energy preset must use exactly energy >= 0.7.",
            ));
        }
        if self.context.len() > 2
            || self
                .context
                .iter()
                .enumerate()
                .any(|(i, c)| self.context[..i].contains(c))
        {
            return Err(message("Request context must not contain duplicates."));
        }
        if !(1..=100).contains(&self.limit) {
            return Err(message("Track limit must be 1..100."));
        }
        if self.source_ids.len() > 50
            || self
                .source_ids
                .iter()
                .any(|s| s != "liked" && !crate::model::spotify_id(s))
        {
            return Err(message(
                "Choose at most 50 source IDs: liked or 22-character Spotify playlist IDs.",
            ));
        }
        for text in [&self.artist_contains, &self.title_contains]
            .into_iter()
            .flatten()
        {
            if text.trim().is_empty() || text.len() > 200 || text.chars().any(char::is_control) {
                return Err(message(
                    "Artist/title substring must be nonempty, at most 200 bytes, and contain no control characters.",
                ));
            }
        }
        for range in [&self.release_year, &self.saved_year].into_iter().flatten() {
            if !range.valid(1, 9999) {
                return Err(message("Year bounds must be ordered and within 1..9999."));
            }
        }
        if self
            .duration_ms
            .as_ref()
            .is_some_and(|r| !r.valid(1, 86_400_000))
        {
            return Err(message(
                "Duration bounds must be ordered and within 1..86400000 milliseconds.",
            ));
        }
        for (&feature, range) in &self.features {
            let (low, high) = feature.bounds();
            if !range.valid(low, high)
                || [range.min, range.max]
                    .into_iter()
                    .flatten()
                    .any(|v| !feature.valid_value(v))
            {
                return Err(message(
                    "Feature bounds are invalid, nonfinite, reversed, or outside the supported scale.",
                ));
            }
        }
        Ok(())
    }
    /// Text for local previews only; never include this library-derived UI in AI input.
    pub fn summary(&self) -> Vec<String> {
        let mut out = vec![format!(
            "Up to {} tracks; stable track-ID order",
            self.limit
        )];
        out.push(if self.source_ids.is_empty() {
            "All accessible library sources".into()
        } else {
            format!("Any selected source: {}", self.source_ids.join(", "))
        });
        if let Some(v) = &self.artist_contains {
            out.push(format!("Artist contains {v:?} (case-insensitive)"));
        }
        if let Some(v) = &self.title_contains {
            out.push(format!("Title contains {v:?} (case-insensitive)"));
        }
        match self.explicit {
            Explicit::Any => {}
            Explicit::CleanOnly => out.push("Known non-explicit only".into()),
            Explicit::ExplicitOnly => out.push("Known explicit only".into()),
        }
        if let Some(v) = &self.release_year {
            out.push(format!("Edition release year: {}", describe(v)));
        }
        if let Some(v) = &self.saved_year {
            out.push(format!("Liked Songs saved year (UTC): {}", describe(v)));
        }
        if let Some(v) = &self.duration_ms {
            out.push(format!("Duration in milliseconds: {}", describe(v)));
        }
        for (f, r) in &self.features {
            out.push(format!("ReccoBeats {}: {}", f.as_str(), describe(r)));
        }
        if self.genre_needed() {
            out.push(format!(
                "Static genre tag rule: {}",
                self.genre_groups
                    .iter()
                    .map(|g| format!("({})", g.join(" OR ")))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ));
            out.push(match self.genre_match_scope {
                GenreMatchScope::TrackOnly => "Last.fm track-level tags only; community labels, not verified acoustic genre".into(),
                GenreMatchScope::TrackOrArtist => "Last.fm track-level tags preferred; artist-level tags allowed only as a weaker genre prior, not evidence of this track's sound".into(),
            });
        }
        if self.energy_preset.is_some() {
            out.push("Preset assumption: high energy means ReccoBeats energy >= 0.7, a heuristic, not acoustic certainty or a clinical/exercise-safety claim".into());
        }
        for context in &self.context {
            out.push(format!(
                "Request context: {}; not a literal track property or a guarantee of performance",
                match context {
                    RequestContext::Workout => "workout",
                    RequestContext::ToFailure => "training to failure",
                }
            ));
        }
        out.push("All bounds inclusive; missing required evidence excludes a track".into());
        out
    }
}

fn describe<T: std::fmt::Display>(r: &Range<T>) -> String {
    match (&r.min, &r.max) {
        (Some(a), Some(b)) => format!("{a}..{b}"),
        (Some(a), None) => format!(">= {a}"),
        (None, Some(b)) => format!("<= {b}"),
        _ => "invalid empty range".into(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectedTrack {
    pub id: String,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub tracks: Vec<SelectedTrack>,
    pub total_matches: usize,
    pub considered: usize,
    pub excluded_unknown: usize,
    pub excluded_mismatch: usize,
}

fn release_year(track: &Track) -> Option<u16> {
    let date = track.release_date.as_deref()?;
    let year = match track.release_precision.as_deref()? {
        "year" if date.len() == 4 && date.bytes().all(|b| b.is_ascii_digit()) => {
            date.parse::<i32>().ok()?
        }
        "month" if date.len() == 7 => NaiveDate::parse_from_str(&format!("{date}-01"), "%Y-%m-%d")
            .ok()?
            .year(),
        "day" if date.len() == 10 => NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?.year(),
        _ => return None,
    };
    u16::try_from(year).ok().filter(|v| (1..=9999).contains(v))
}

/// All predicates are ANDed, with OR within source IDs. Results deduplicate exact IDs only.
/// Invalid criteria or a missing/inaccessible requested source is an error, never relaxed.
/// Exclusion counts partition rejected tracks; unknown takes precedence over mismatch.
pub fn select(snapshot: &Snapshot, coverage: &Coverage, criteria: &Criteria) -> Result<Selection> {
    select_inner(snapshot, coverage, criteria, false)
}

/// Apply every supplied filter but collect every match, ignoring the output limit.
/// For pre-genre candidates, callers explicitly clear genre_groups on a clone.
pub fn select_all(
    snapshot: &Snapshot,
    coverage: &Coverage,
    criteria: &Criteria,
) -> Result<Selection> {
    select_inner(snapshot, coverage, criteria, true)
}

pub fn all_candidate_ids(
    snapshot: &Snapshot,
    coverage: &Coverage,
    criteria: &Criteria,
) -> Result<Vec<String>> {
    Ok(select_all(snapshot, coverage, criteria)?
        .tracks
        .into_iter()
        .map(|t| t.id)
        .collect())
}

fn select_inner(
    snapshot: &Snapshot,
    coverage: &Coverage,
    criteria: &Criteria,
    all: bool,
) -> Result<Selection> {
    criteria.validate()?;
    let accessible: BTreeSet<&str> = snapshot
        .sources
        .iter()
        .filter(|s| s.accessible)
        .map(|s| s.id.as_str())
        .collect();
    if criteria
        .source_ids
        .iter()
        .any(|s| !accessible.contains(s.as_str()))
    {
        return Err(message(
            "A selected source is missing or inaccessible. Refresh sources or explicitly change the selection.",
        ));
    }
    let selected: BTreeSet<&str> = if criteria.source_ids.is_empty() {
        accessible
    } else {
        criteria.source_ids.iter().map(String::as_str).collect()
    };
    let mut result = Selection::default();
    for (id, track) in &snapshot.tracks {
        result.considered += 1;
        if id != &track.id || !crate::model::spotify_id(id) {
            result.excluded_unknown += 1;
            continue;
        }
        let mut sources: Vec<&str> = track
            .memberships
            .iter()
            .filter(|m| selected.contains(m.source.as_str()))
            .map(|m| m.source.as_str())
            .collect();
        sources.sort_unstable();
        sources.dedup();
        if sources.is_empty() {
            result.excluded_mismatch += 1;
            continue;
        }
        let mut reasons = vec![format!("Library membership: {}", sources.join(", "))];
        let mut unknown = false;
        let mut mismatch = false;
        let mut check = |known_match: Option<bool>, reason: String| match known_match {
            Some(true) => reasons.push(reason),
            Some(false) => mismatch = true,
            None => unknown = true,
        };
        if let Some(v) = &criteria.artist_contains {
            let term = v.to_lowercase();
            check(
                (!track.artists.is_empty()).then(|| {
                    track
                        .artists
                        .iter()
                        .any(|a| a.to_lowercase().contains(&term))
                }),
                format!("Artist contains {v:?}"),
            );
        }
        if let Some(v) = &criteria.title_contains {
            check(
                (!track.name.is_empty())
                    .then(|| track.name.to_lowercase().contains(&v.to_lowercase())),
                format!("Title contains {v:?}"),
            );
        }
        if criteria.explicit != Explicit::Any {
            let expected = criteria.explicit == Explicit::ExplicitOnly;
            check(
                track.explicit.map(|v| v == expected),
                format!("Explicit: {expected}"),
            );
        }
        if let Some(r) = &criteria.release_year {
            let year = release_year(track);
            check(
                year.map(|y| r.contains(y)),
                format!("Edition release year: {}", year.unwrap_or_default()),
            );
        }
        if let Some(r) = &criteria.saved_year {
            let years: Vec<u16> = track
                .memberships
                .iter()
                .filter(|m| m.source == "liked")
                .filter_map(|m| chrono::DateTime::parse_from_rfc3339(m.added_at.as_deref()?).ok())
                .filter_map(|d| u16::try_from(d.with_timezone(&chrono::Utc).year()).ok())
                .collect();
            let matches: Vec<u16> = years.iter().copied().filter(|y| r.contains(*y)).collect();
            check(
                (!years.is_empty()).then_some(!matches.is_empty()),
                format!("Liked Songs saved year (UTC): {matches:?}"),
            );
        }
        if let Some(r) = &criteria.duration_ms {
            check(
                track.duration_ms.filter(|v| *v > 0).map(|v| r.contains(v)),
                format!("Duration: {} ms", track.duration_ms.unwrap_or_default()),
            );
        }
        for (&f, r) in &criteria.features {
            let value = coverage
                .tracks
                .get(id)
                .and_then(|e| e.features.get(f.as_str()))
                .copied()
                .filter(|v| f.valid_value(*v));
            check(
                value.map(|v| r.contains(v)),
                format!("ReccoBeats {}: {}", f.as_str(), value.unwrap_or_default()),
            );
        }
        if criteria.genre_needed() {
            let evidence = coverage.tracks.get(id);
            let matching = |tags: &[String]| -> Option<String> {
                let normalized: Vec<String> = tags.iter().map(|t| normalize_tag(t)).collect();
                let matches: Option<Vec<&String>> = criteria
                    .genre_groups
                    .iter()
                    .map(|g| g.iter().find(|t| normalized.contains(t)))
                    .collect();
                matches.map(|m| m.into_iter().cloned().collect::<Vec<_>>().join(", "))
            };
            let track_match = evidence.and_then(|e| matching(&e.tags));
            let artist_match = (criteria.genre_match_scope == GenreMatchScope::TrackOrArtist)
                .then(|| evidence.and_then(|e| matching(&e.artist_tags)))
                .flatten();
            if let Some(tags) = track_match {
                check(
                    Some(true),
                    format!("Last.fm track-level genre tags: {tags} (community labels)"),
                );
            } else if let Some(tags) = artist_match {
                check(
                    Some(true),
                    format!(
                        "Last.fm artist-level genre prior: {tags}; weaker evidence, not a track-level genre claim"
                    ),
                );
            } else {
                // Missing/nonmatching tags cannot establish a hard genre match.
                check(None, String::new());
            }
        }
        if unknown {
            result.excluded_unknown += 1;
        } else if mismatch {
            result.excluded_mismatch += 1;
        } else {
            result.total_matches += 1;
            if all || result.tracks.len() < criteria.limit {
                result.tracks.push(SelectedTrack {
                    id: id.clone(),
                    reasons,
                });
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Evidence, Membership, Source};

    fn fixture() -> (Snapshot, Coverage) {
        let mut snapshot = Snapshot::default();
        snapshot.sources.push(Source {
            id: "liked".into(),
            accessible: true,
            ..Default::default()
        });
        snapshot.sources.push(Source {
            id: "p".repeat(22),
            accessible: true,
            ..Default::default()
        });
        snapshot.sources.push(Source {
            id: "x".repeat(22),
            accessible: false,
            ..Default::default()
        });
        let mut coverage = Coverage::default();
        for (c, explicit, energy) in [
            ('a', Some(false), Some(0.0)),
            ('b', Some(true), Some(0.8)),
            ('c', None, None),
        ] {
            let id = c.to_string().repeat(22);
            snapshot.tracks.insert(
                id.clone(),
                Track {
                    id: id.clone(),
                    name: if c == 'b' { "Evening Edit" } else { "Morning" }.into(),
                    artists: vec!["Example Artist".into(), "Guest".into()],
                    explicit,
                    duration_ms: Some(180_000),
                    release_date: Some("2020-02".into()),
                    release_precision: Some("month".into()),
                    memberships: vec![
                        Membership {
                            source: "liked".into(),
                            added_at: Some("2026-01-01T00:00:00Z".into()),
                            position: 0,
                        },
                        Membership {
                            source: "p".repeat(22),
                            added_at: Some("2024-01-01T00:00:00Z".into()),
                            position: 1,
                        },
                    ],
                    ..Default::default()
                },
            );
            if let Some(energy) = energy {
                coverage.tracks.insert(
                    id,
                    Evidence {
                        features: BTreeMap::from([
                            ("energy".into(), energy),
                            ("tempo".into(), 120.0),
                        ]),
                        ..Default::default()
                    },
                );
            }
        }
        (snapshot, coverage)
    }
    fn exact<T: Copy>(v: T) -> Range<T> {
        Range {
            min: Some(v),
            max: Some(v),
        }
    }
    fn run(criteria: Criteria) -> Selection {
        let (s, c) = fixture();
        select(&s, &c, &criteria).unwrap()
    }

    #[test]
    fn defaults_are_stable_and_exact_id_deduplicated() {
        let (s, c) = fixture();
        let a = select(&s, &c, &Criteria::default()).unwrap();
        assert_eq!(a, select(&s, &c, &Criteria::default()).unwrap());
        assert_eq!(
            a.tracks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            vec!["a".repeat(22), "b".repeat(22), "c".repeat(22)]
        );
        assert_eq!(a.total_matches, 3);
        assert_eq!(a.considered, 3);
    }
    #[test]
    fn limits_after_counting_all_matches() {
        let r = run(Criteria {
            limit: 1,
            ..Default::default()
        });
        assert_eq!(r.tracks.len(), 1);
        assert_eq!(r.total_matches, 3);
    }
    #[test]
    fn all_candidates_ignore_output_limit_without_a_hundred_track_cap() {
        let (mut s, c) = fixture();
        let template = s.tracks.values().next().unwrap().clone();
        for i in 0..150 {
            let id = format!("{i:022}");
            let mut track = template.clone();
            track.id = id.clone();
            s.tracks.insert(id, track);
        }
        let criteria = Criteria {
            limit: 1,
            ..Default::default()
        };
        assert_eq!(select(&s, &c, &criteria).unwrap().tracks.len(), 1);
        assert_eq!(select_all(&s, &c, &criteria).unwrap().tracks.len(), 153);
        assert_eq!(all_candidate_ids(&s, &c, &criteria).unwrap().len(), 153);
    }
    #[test]
    fn hardcore_metal_requires_both_groups_or_metalcore() {
        let (s, mut coverage) = fixture();
        let criteria = Criteria {
            genre_groups: genre_groups_for("harcore metal").unwrap(),
            ..Default::default()
        };
        assert_eq!(
            criteria.genre_groups,
            genre_groups_for("hardcore metal").unwrap()
        );
        for (tags, expected) in [
            (vec!["metal"], 0),
            (vec!["hardcore"], 0),
            (vec!["metalcore"], 1),
            (vec!["metal", "hardcore"], 1),
            (vec!["HEAVY-METAL", "Hardcore Punk"], 1),
            (vec!["not metalcore"], 0),
            (vec!["metalcore fan"], 0),
        ] {
            coverage.tracks.get_mut(&"a".repeat(22)).unwrap().tags =
                tags.into_iter().map(String::from).collect();
            let selection = select(&s, &coverage, &criteria).unwrap();
            assert_eq!(selection.total_matches, expected);
        }
    }
    #[test]
    fn genre_prefers_track_evidence_then_labels_artist_prior() {
        let (s, mut coverage) = fixture();
        let e = coverage.tracks.get_mut(&"a".repeat(22)).unwrap();
        e.tags = vec!["metalcore".into()];
        e.artist_tags = vec!["metalcore".into()];
        let mut criteria = Criteria {
            genre_groups: genre_groups_for("hardcore metal").unwrap(),
            ..Default::default()
        };
        let track = select(&s, &coverage, &criteria).unwrap().tracks.remove(0);
        assert!(
            track
                .reasons
                .iter()
                .any(|r| r.contains("track-level genre tags"))
        );
        assert!(!track.reasons.iter().any(|r| r.contains("artist-level")));
        coverage
            .tracks
            .get_mut(&"a".repeat(22))
            .unwrap()
            .tags
            .clear();
        let track = select(&s, &coverage, &criteria).unwrap().tracks.remove(0);
        assert!(
            track
                .reasons
                .iter()
                .any(|r| r.contains("artist-level") && r.contains("weaker"))
        );
        criteria.genre_match_scope = GenreMatchScope::TrackOnly;
        assert_eq!(select(&s, &coverage, &criteria).unwrap().total_matches, 0);
    }
    #[test]
    fn genre_cannot_mix_partial_scopes_or_infer_from_names() {
        let (mut s, mut c) = fixture();
        let e = c.tracks.get_mut(&"a".repeat(22)).unwrap();
        e.tags = vec!["metal".into()];
        e.artist_tags = vec!["hardcore".into()];
        s.tracks.get_mut(&"a".repeat(22)).unwrap().name = "metalcore".into();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().artists = vec!["metalcore".into()];
        let criteria = Criteria {
            genre_groups: genre_groups_for("hardcore metal").unwrap(),
            ..Default::default()
        };
        assert_eq!(select(&s, &c, &criteria).unwrap().total_matches, 0);
        assert_eq!(
            select(&s, &Coverage::default(), &criteria)
                .unwrap()
                .excluded_unknown,
            3
        );
    }
    #[test]
    fn context_is_not_a_track_property_and_preset_requires_real_numeric_evidence() {
        let (s, c) = fixture();
        let criteria = Criteria {
            context: vec![RequestContext::Workout, RequestContext::ToFailure],
            ..Default::default()
        };
        assert_eq!(select(&s, &c, &criteria).unwrap().total_matches, 3);
        let criteria = Criteria {
            energy_preset: Some(EnergyPreset::High),
            features: BTreeMap::from([(
                Feature::Energy,
                Range {
                    min: Some(0.7),
                    max: None,
                },
            )]),
            ..criteria
        };
        let result = select(&s, &c, &criteria).unwrap();
        assert_eq!(result.total_matches, 1);
        assert_eq!(result.excluded_unknown, 1);
    }
    #[test]
    fn genre_vocabulary_is_finite_bounded_and_validated() {
        for name in supported_genres() {
            Criteria {
                genre_groups: genre_groups_for(name).unwrap(),
                ..Default::default()
            }
            .validate()
            .unwrap();
        }
        assert!(genre_groups_for("imaginarycore").is_none());
        for groups in [
            vec![vec![]],
            vec![vec!["imaginarycore".into()]],
            vec![vec!["metal".into()]; 9],
        ] {
            assert!(
                Criteria {
                    genre_groups: groups,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn rejects_zero_and_excessive_limit() {
        for limit in [0, 101, usize::MAX] {
            assert!(
                Criteria {
                    limit,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn clean_excludes_explicit_and_unknown_separately() {
        let r = run(Criteria {
            explicit: Explicit::CleanOnly,
            ..Default::default()
        });
        assert_eq!(r.tracks[0].id, "a".repeat(22));
        assert_eq!(r.excluded_unknown, 1);
        assert_eq!(r.excluded_mismatch, 1);
    }
    #[test]
    fn explicit_only_requires_positive_evidence() {
        let r = run(Criteria {
            explicit: Explicit::ExplicitOnly,
            ..Default::default()
        });
        assert_eq!(r.tracks[0].id, "b".repeat(22));
        assert_eq!(r.tracks.len(), 1);
    }
    #[test]
    fn artist_matches_any_artist_case_insensitively() {
        assert_eq!(
            run(Criteria {
                artist_contains: Some("gUeSt".into()),
                ..Default::default()
            })
            .total_matches,
            3
        );
    }
    #[test]
    fn title_is_substring_not_a_genre_lookup() {
        assert_eq!(
            run(Criteria {
                title_contains: Some("EDIT".into()),
                ..Default::default()
            })
            .tracks[0]
                .id,
            "b".repeat(22)
        );
    }
    #[test]
    fn missing_source_is_error_not_broadened_search() {
        let (s, c) = fixture();
        assert!(
            select(
                &s,
                &c,
                &Criteria {
                    source_ids: vec!["z".repeat(22)],
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn inaccessible_source_is_error_even_with_accessible_source() {
        let (s, c) = fixture();
        assert!(
            select(
                &s,
                &c,
                &Criteria {
                    source_ids: vec!["liked".into(), "x".repeat(22)],
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn inaccessible_only_membership_never_enters_default_results() {
        let (mut s, c) = fixture();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().memberships = vec![Membership {
            source: "x".repeat(22),
            ..Default::default()
        }];
        assert_eq!(
            select(&s, &c, &Criteria::default()).unwrap().total_matches,
            2
        );
    }
    #[test]
    fn sources_are_union_and_other_filters_intersection() {
        assert_eq!(
            run(Criteria {
                source_ids: vec!["liked".into(), "p".repeat(22)],
                title_contains: Some("edit".into()),
                ..Default::default()
            })
            .total_matches,
            1
        );
    }
    #[test]
    fn numeric_unknown_is_not_zero() {
        let r = run(Criteria {
            features: BTreeMap::from([(Feature::Energy, exact(0.0))]),
            ..Default::default()
        });
        assert_eq!(r.total_matches, 1);
        assert_eq!(r.tracks[0].id, "a".repeat(22));
        assert_eq!(r.excluded_unknown, 1);
    }
    #[test]
    fn numeric_bounds_are_inclusive_and_conjunctive() {
        let r = run(Criteria {
            features: BTreeMap::from([
                (
                    Feature::Energy,
                    Range {
                        min: Some(0.8),
                        max: Some(1.0),
                    },
                ),
                (Feature::Tempo, exact(120.0)),
            ]),
            ..Default::default()
        });
        assert_eq!(r.total_matches, 1);
    }
    #[test]
    fn invalid_numeric_evidence_is_unknown() {
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            let (s, mut c) = fixture();
            c.tracks
                .get_mut(&"a".repeat(22))
                .unwrap()
                .features
                .insert("energy".into(), value);
            let r = select(
                &s,
                &c,
                &Criteria {
                    features: BTreeMap::from([(
                        Feature::Energy,
                        Range {
                            min: Some(0.0),
                            max: None,
                        },
                    )]),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(r.excluded_unknown, 2);
        }
    }
    #[test]
    fn feature_bounds_validate_domain_and_order() {
        for (f, r) in [
            (Feature::Tempo, exact(0.0)),
            (Feature::Key, exact(1.5)),
            (Feature::Mode, exact(2.0)),
            (Feature::Energy, exact(f64::NAN)),
            (
                Feature::Energy,
                Range {
                    min: Some(0.9),
                    max: Some(0.1),
                },
            ),
            (
                Feature::Energy,
                Range {
                    min: None,
                    max: None,
                },
            ),
        ] {
            assert!(
                Criteria {
                    features: BTreeMap::from([(f, r)]),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn all_supported_feature_scales_select() {
        for f in Feature::ALL {
            let value = match f {
                Feature::Tempo => 120.0,
                Feature::Loudness => -10.0,
                Feature::Key => 4.0,
                Feature::Mode => 1.0,
                _ => 0.5,
            };
            let (s, mut c) = fixture();
            c.tracks
                .get_mut(&"a".repeat(22))
                .unwrap()
                .features
                .insert(f.as_str().into(), value);
            assert!(
                select(
                    &s,
                    &c,
                    &Criteria {
                        features: BTreeMap::from([(f, exact(value))]),
                        ..Default::default()
                    }
                )
                .unwrap()
                .tracks
                .iter()
                .any(|t| t.id == "a".repeat(22))
            );
        }
    }
    #[test]
    fn release_precision_year_month_day_are_respected() {
        for (date, precision) in [
            ("2020", "year"),
            ("2020-02", "month"),
            ("2020-02-29", "day"),
        ] {
            let (mut s, c) = fixture();
            let t = s.tracks.get_mut(&"a".repeat(22)).unwrap();
            t.release_date = Some(date.into());
            t.release_precision = Some(precision.into());
            assert_eq!(
                select(
                    &s,
                    &c,
                    &Criteria {
                        release_year: Some(exact(2020)),
                        ..Default::default()
                    }
                )
                .unwrap()
                .total_matches,
                3
            );
        }
    }
    #[test]
    fn malformed_or_unproven_release_date_is_unknown() {
        for (date, precision) in [
            ("2020-02-30", Some("day")),
            ("2020-13", Some("month")),
            ("2020-01-01", Some("year")),
            ("2020", None),
        ] {
            let (mut s, c) = fixture();
            let t = s.tracks.get_mut(&"a".repeat(22)).unwrap();
            t.release_date = Some(date.into());
            t.release_precision = precision.map(str::to_owned);
            assert_eq!(
                select(
                    &s,
                    &c,
                    &Criteria {
                        release_year: Some(exact(2020)),
                        ..Default::default()
                    }
                )
                .unwrap()
                .excluded_unknown,
                1
            );
        }
    }
    #[test]
    fn saved_year_is_not_playlist_added_year() {
        assert_eq!(
            run(Criteria {
                saved_year: Some(exact(2024)),
                ..Default::default()
            })
            .total_matches,
            0
        );
        assert_eq!(
            run(Criteria {
                saved_year: Some(exact(2026)),
                ..Default::default()
            })
            .total_matches,
            3
        );
    }
    #[test]
    fn saved_year_uses_utc_not_timestamp_local_year() {
        let (mut s, c) = fixture();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().memberships[0].added_at =
            Some("2025-12-31T23:30:00-05:00".into());
        assert_eq!(
            select(
                &s,
                &c,
                &Criteria {
                    saved_year: Some(exact(2026)),
                    ..Default::default()
                }
            )
            .unwrap()
            .total_matches,
            3
        );
    }
    #[test]
    fn malformed_saved_dates_do_not_pass() {
        let (mut s, c) = fixture();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().memberships[0].added_at =
            Some("2026-garbage".into());
        assert_eq!(
            select(
                &s,
                &c,
                &Criteria {
                    saved_year: Some(exact(2026)),
                    ..Default::default()
                }
            )
            .unwrap()
            .excluded_unknown,
            1
        );
    }
    #[test]
    fn duration_unknown_fails_even_without_lower_bound() {
        let (mut s, c) = fixture();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().duration_ms = None;
        assert_eq!(
            select(
                &s,
                &c,
                &Criteria {
                    duration_ms: Some(Range {
                        min: None,
                        max: Some(180000)
                    }),
                    ..Default::default()
                }
            )
            .unwrap()
            .excluded_unknown,
            1
        );
    }
    #[test]
    fn coverage_cannot_introduce_external_tracks() {
        let (s, mut c) = fixture();
        c.tracks.insert(
            "z".repeat(22),
            Evidence {
                features: BTreeMap::from([("energy".into(), 0.8)]),
                ..Default::default()
            },
        );
        assert_eq!(
            select(&s, &c, &Criteria::default()).unwrap().total_matches,
            3
        );
    }
    #[test]
    fn invalid_identity_is_excluded() {
        let (mut s, c) = fixture();
        s.tracks.get_mut(&"a".repeat(22)).unwrap().id = "z".repeat(22);
        let r = select(&s, &c, &Criteria::default()).unwrap();
        assert_eq!(r.excluded_unknown, 1);
        assert_eq!(r.total_matches, 2);
    }
    #[test]
    fn explanations_include_observed_numeric_evidence() {
        let r = run(Criteria {
            features: BTreeMap::from([(Feature::Tempo, exact(120.0))]),
            ..Default::default()
        });
        assert!(
            r.tracks[0]
                .reasons
                .iter()
                .any(|s| s == "ReccoBeats tempo: 120")
        );
    }
    #[test]
    fn criteria_serialization_is_typed_and_rejects_unknown_filters() {
        assert!(serde_json::from_str::<Criteria>(r#"{"genre":"house"}"#).is_err());
        assert!(
            serde_json::from_str::<Criteria>(
                r#"{"features":{"happiness":{"min":0.5,"max":null}}}"#
            )
            .is_err()
        );
        let c = Criteria::default();
        assert_eq!(
            c,
            serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap()
        );
    }
    #[test]
    fn summary_exposes_utc_and_hard_unknown_policy() {
        let c = Criteria {
            saved_year: Some(exact(2026)),
            features: BTreeMap::from([(Feature::Tempo, exact(120.0))]),
            ..Default::default()
        };
        assert!(c.is_numeric());
        assert!(c.summary().iter().any(|s| s.contains("UTC")));
        assert!(
            c.summary()
                .iter()
                .any(|s| s.contains("missing required evidence"))
        );
    }
    #[test]
    fn bounded_sources_substrings_years_and_durations() {
        for c in [
            Criteria {
                source_ids: vec!["liked".into(); 51],
                ..Default::default()
            },
            Criteria {
                artist_contains: Some(" ".into()),
                ..Default::default()
            },
            Criteria {
                title_contains: Some("a".repeat(201)),
                ..Default::default()
            },
            Criteria {
                release_year: Some(exact(0)),
                ..Default::default()
            },
            Criteria {
                duration_ms: Some(exact(86_400_001)),
                ..Default::default()
            },
        ] {
            assert!(c.validate().is_err());
        }
    }
}
