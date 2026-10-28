//! Shared visual primitives. Page composition lives in web/views.rs; tokens in assets/style.css.
use crate::model::Snapshot;
use maud::{Markup, html};

pub fn icon(name: &str) -> Markup {
    html! {img src=(format!("/icons/{name}")) alt="" width="16" height="16";}
}

pub fn navigation(active: &str) -> Markup {
    html! {nav aria-label="Library views" {@for (key,label) in [("","Build a playlist"),("library","Library"),("sources","Sources"),("coverage","Coverage"),("history","History")] {
        a href=(format!("/?tab={key}")) aria-current=[(active==label).then_some("page")] {(label)}
    }}}
}

pub fn library_stats(snapshot: &Snapshot) -> Markup {
    html! {section.metrics aria-label="Library summary" {
        div {strong {(snapshot.tracks.len())} span {"tracks in your orbit"}}
        div {strong {(snapshot.sources.iter().filter(|s|s.accessible).count())} span {"sources"}}
        div {a.metric-link href="/?tab=sources&excluded=true" {strong {(snapshot.sources.iter().filter(|s|!s.accessible).count())} span {"Excluded sources / view"}}}
        div {strong {(snapshot.skipped_items)} span {"unavailable items"}}
    }}
}

pub fn job_status(
    text: &str,
    running: bool,
    publishing: bool,
    completed: Option<usize>,
    total: Option<usize>,
    csrf: &str,
) -> Markup {
    html! {section.job id="job-status" role="status" aria-live="polite" aria-busy=(running.to_string()) data-running=(running.to_string()) hidden[text.is_empty()] {
        div.job-copy {div.job-label {@if running {span.activity-mark aria-hidden="true" {}} span {(text)}}
            @if running {
                @if let (Some(done),Some(total))=(completed,total.filter(|n|*n>0)) {
                    progress value=(done) max=(total) aria-label="Current stage progress" {}
                    small {(done) " / " (total)}
                } @else {progress aria-label="Working" {}}
            }
        }
        @if running && !publishing {form method="post" action="/pause" {input type="hidden" name="csrf" value=(csrf);button.quiet {"Pause"}}}
        noscript {a href="" {"Check progress"}}
    }}
}

pub fn scroll_region(label: &str, content: Markup) -> Markup {
    html! {div.scroll-region role="region" aria-label=(label) tabindex="0" {(content)}}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_real_or_indeterminate_and_escaped() {
        let counted = job_status("<script>", true, false, Some(3), Some(10), "csrf").into_string();
        assert!(counted.contains("value=\"3\""));
        assert!(counted.contains("max=\"10\""));
        assert!(counted.contains("&lt;script&gt;"));
        assert!(!counted.contains("<script>"));
        let unknown = job_status("Thinking", true, false, None, None, "csrf").into_string();
        assert!(!unknown.contains("<progress value="));
        assert!(unknown.contains("data-running=\"true\""));
    }
}
