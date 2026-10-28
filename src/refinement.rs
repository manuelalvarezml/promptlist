//! Bounded prompt-only conversational state. Never contains library metadata.
use crate::{
    preferences::{self, Preferences},
    selection::Criteria,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Refinement {
    pub parent_run: String,
    pub prior: Preferences,
    pub message: String,
    #[serde(default)]
    pub recent_messages: Vec<String>,
}

impl Refinement {
    pub fn count_only(&self) -> Option<usize> {
        let text = self.message.to_lowercase();
        let words: Vec<_> = text
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .collect();
        if !words.iter().all(|w| {
            w.parse::<usize>().is_ok()
                || ["make", "it", "just", "only", "please", "tracks", "songs"].contains(w)
        }) {
            return None;
        }
        if words.iter().filter(|w| w.parse::<usize>().is_ok()).count() != 1 {
            return None;
        }
        words
            .windows(2)
            .find_map(|w| {
                matches!(w[1], "tracks" | "songs")
                    .then(|| w[0].parse::<usize>().ok())
                    .flatten()
            })
            .filter(|n| (1..=100).contains(n))
    }
    pub fn model_input(&self, original: &str) -> String {
        serde_json::json!({"original_request":original,"previous_preferences":self.prior,"latest_refinement":self.message}).to_string()
    }

    pub fn fallback(&self) -> Preferences {
        let mut plan = self.prior.clone();
        let text = self.message.to_lowercase();
        let exact = crate::intent::parse_local(&self.message).ok();
        let delta = preferences::fallback(&self.message, exact.as_ref());
        let count = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find_map(|w| {
                matches!(
                    w[1].trim_matches(|c: char| !c.is_alphabetic()),
                    "tracks" | "songs"
                )
                .then(|| w[0].parse::<usize>().ok())
                .flatten()
            });
        if let Some(count) = count {
            plan.limit = count.clamp(1, 100);
        }
        let less = [
            "less energetic",
            "less energy",
            "lower energy",
            "calmer",
            "more relaxed",
        ]
        .iter()
        .any(|s| text.contains(s));
        let more = ["more energetic", "more energy", "higher energy"]
            .iter()
            .any(|s| text.contains(s));
        if less || more {
            plan.energy_target = Some(
                (plan.energy_target.unwrap_or(0.5) + if less { -0.15 } else { 0.15 }).clamp(0., 1.),
            );
        } else if delta.energy_target.is_some() {
            plan.energy_target = delta.energy_target;
        }
        // Do not turn count-only or relative-energy requests into unrelated tags.
        if count.is_none() && !less && !more || exact.as_ref().is_some_and(Criteria::genre_needed) {
            if text.contains("instead") {
                plan.tag_groups.clear();
            }
            for group in delta.tag_groups {
                if !plan.tag_groups.contains(&group) {
                    plan.tag_groups.push(group);
                }
            }
            if !delta.title_terms.is_empty() {
                plan.title_terms = delta.title_terms;
            }
        }
        if plan.tag_groups.len() > 6 {
            plan.tag_groups.drain(..plan.tag_groups.len() - 6);
        }
        plan.description = "Refined selection using your previous preferences".into();
        plan.caveats = vec!["Local refinement fallback: prior preferences retained; unrecognized changes are approximate, not guaranteed.".into()];
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(message: &str) -> Refinement {
        Refinement {
            parent_run: "private-run-id".into(),
            prior: preferences::fallback("10 high energy metal tracks", None),
            message: message.into(),
            recent_messages: vec![],
        }
    }
    #[test]
    fn relative_energy_and_counts_preserve_previous_intent() {
        let c = context("less energetic, 6 tracks");
        let plan = c.fallback();
        assert_eq!(plan.limit, 6);
        assert!(plan.energy_target.unwrap() < c.prior.energy_target.unwrap());
        assert_eq!(plan.tag_groups, c.prior.tag_groups);
        let mut next = context("more energetic");
        next.prior = plan;
        assert_eq!(next.fallback().limit, 6);
    }
    #[test]
    fn model_context_excludes_internal_ids_and_full_history() {
        let c = context("more energetic");
        let input = c.model_input("metal for a workout");
        assert!(!input.contains(&c.parent_run));
        assert!(!input.contains("recent_messages"));
        assert!(input.contains("previous_preferences"));
        assert!(input.contains("latest_refinement"));
    }
    #[test]
    fn count_shortcut_does_not_drop_additional_clauses() {
        assert_eq!(context("make it 6 tracks please").count_only(), Some(6));
        assert_eq!(context("6 tracks but more energetic").count_only(), None);
        assert_eq!(context("0 tracks").count_only(), None);
        assert_eq!(context("6 tracks instead of 5 tracks").count_only(), None);
    }
}
