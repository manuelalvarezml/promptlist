//! Page-level composition. Reusable visual primitives live in crate::ui.
use super::*;
use crate::ui;
pub(super) fn action_form(
    csrf: &str,
    path: &str,
    label: &str,
    class: &str,
    disabled: bool,
) -> Markup {
    html! {form method="post" action=(path) {(hidden(csrf)) button class=(class) disabled[disabled] {(label)}}}
}

pub(super) fn edit_button(
    csrf: &str,
    preview: &Preview,
    index: usize,
    command: &str,
    icon: &str,
    label: &str,
    disabled: bool,
) -> Markup {
    html! {form method="post" action="/preview/edit" {(hidden(csrf)) input type="hidden" name="preview_id" value=(preview.id);input type="hidden" name="index" value=(index);input type="hidden" name="command" value=(command);
        button.icon-button type="submit" title=(label) aria-label=(label) disabled[disabled] {(ui::icon(icon))}
    }}
}

pub(super) fn composer(s: &Session, snapshot: &Snapshot, preview: Option<&Preview>) -> Markup {
    let unavailable = s.account.is_none() || s.job.running || snapshot.tracks.is_empty();
    let form = s.build.clone().unwrap_or_else(|| BuildForm {
        name: "Promptlist selection".into(),
        ..Default::default()
    });
    html! {
        div.build-layout {
            section.composer {
                form.composer-form method="post" action="/build" {(hidden(&s.csrf))
                    label.sr-only for="prompt" {"Playlist prompt"}
                    textarea.floating-prompt id="prompt" name="prompt" rows="4" maxlength="2000" placeholder="Describe your playlist" {(form.prompt)}
                    div.composer-actions {
                        label.field {"Playlist name" input name="name" maxlength="100" value=(form.name);}
                        button.primary.build-button type="submit" disabled[unavailable] {"Build preview" span.forward-icon {(ui::icon("arrow-up"))}}
                    }
                    details.advanced-config {
                        summary {"+ advanced config"}
                        div.filter-grid {
                            label.field {"Source" select name="source" {option value="" selected[form.source.is_empty()] {"All accessible sources"} @for source in &snapshot.sources {option value=(source.id) selected[form.source==source.id] disabled[!source.accessible] {(source.name) @if !source.accessible {" / unavailable"}}}}}
                            label.field {"Track limit" input name="limit" type="number" min="1" max="100" value=(form.limit) placeholder="From request / 30";}
                        }
                        h3 {"Exact filters"}
                        div.filter-grid {
                            label.field {"Artist contains" input name="artist" maxlength="200" value=(form.artist);}
                            label.field {"Title contains" input name="title_match" maxlength="200" value=(form.title_match);}
                            label.field {"Released in year" input name="release_year" type="number" min="1" max="9999" value=(form.release_year);}
                            label.field {"Liked in year (UTC)" input name="saved_year" type="number" min="1" max="9999" value=(form.saved_year);}
                            label.field {"Minimum BPM" input name="bpm_min" type="number" min="1" max="500" step="0.1" value=(form.bpm_min);}
                            label.field {"Maximum BPM" input name="bpm_max" type="number" min="1" max="500" step="0.1" value=(form.bpm_max);}
                            label.field {"Minimum energy" input name="energy_min" type="number" min="0" max="1" step="0.01" value=(form.energy_min);}
                        }
                    }
                }
                @if let Some(p)=preview {@if p.run_id.is_some() {
                    section.refinement {
                        @if let Some(refinement)=&form.refinement {
                            ol.conversation aria-label="Recent refinements" {@for message in &refinement.recent_messages {li {(message)}}}
                        }
                        form.refinement-form method="post" action="/refine" {(hidden(&s.csrf))
                            input type="hidden" name="preview_id" value=(p.id);
                            label for="refinement" {"Refine playlist"}
                            textarea id="refinement" name="prompt" maxlength="2000" rows="3" placeholder="Less intense, more unexpected..." required {}
                            button.primary type="submit" disabled[unavailable||matches!(p.phase,Phase::Creating|Phase::Created)] {"Refine playlist" span.forward-icon {(ui::icon("arrow-up"))}}
                        }
                    }
                }}
                @if s.account.is_some() && snapshot.tracks.is_empty() {div.toolbar {(action_form(&s.csrf,"/sync","Sync library","quiet",s.job.running))}}
            }
            section.preview id="selection" data-preview-id=(preview.map(|p|p.id.as_str()).unwrap_or("")) {
                div.preview-header {h2 {"Your mix"} span.result-count {(preview.map(|p|p.track_ids.len()).unwrap_or(0)) " tracks"}}
                @if let Some(p)=preview {
                    h3 {(p.name)}
                    @if p.warnings.iter().any(|w|w.starts_with("Best-effort selection:")) {p.muted {"Best-effort selection: closest available matches; some preferences are approximate."}}
                    details.criteria-summary {summary {"Selection criteria & matching details"} ul {@for line in &p.criteria {li {(line)}}} @for warning in &p.warnings {p.muted {(warning)}}}
                    @if p.track_ids.is_empty() {p.result-empty {"No matches for these criteria"}}
                    (ui::scroll_region("Playlist tracks",html!{ol.track-list {@for (index,id) in p.track_ids.iter().enumerate() {@if let Some(track)=snapshot.tracks.get(id) {
                        li.track-row {
                            span.track-number {(format!("{:02}",index+1))}
                            @if let Some(image)=&track.image {img.cover src=(image) alt="" loading="lazy" width="40" height="40";}
                            div.track-info {a href=(format!("https://open.spotify.com/track/{id}")) rel="noreferrer" target="_blank" {(track.name)} small {(track.artists.join(", "))}}
                            div.actions {
                                (ui::preview_button(id,&track.name))
                            @if p.phase==Phase::Draft {
                                (edit_button(&s.csrf,p,index,"up","arrow-up","Move track up",s.job.running||index==0))
                                (edit_button(&s.csrf,p,index,"down","arrow-down","Move track down",s.job.running||index+1==p.track_ids.len()))
                                (edit_button(&s.csrf,p,index,"remove","x","Remove track",s.job.running))
                            }}
                        }
                    }}}}))
                    @if p.phase==Phase::Saved {
                        @if let Some(id)=&p.playlist_id {p.notice {"Saved as a private playlist. " a href=(format!("https://open.spotify.com/playlist/{id}")) target="_blank" rel="noreferrer" {"Open in Spotify"}}}
                    } @else if p.phase==Phase::Creating {
                        p.notice {"Save started. If interrupted, check Spotify before another creation attempt."}
                    } @else {
                        form.save-form method="post" action="/publish" {(hidden(&s.csrf)) input type="hidden" name="preview_id" value=(p.id);
                            button.primary disabled[s.job.running||p.track_ids.is_empty()] {(if p.phase==Phase::Created {"Resume export"} else {"Export to Spotify"})}
                        }
                    }
                    @if !s.job.running && matches!(p.phase,Phase::Creating|Phase::Created) {
                        @if let Some(id)=&p.playlist_id {p {a href=(format!("https://open.spotify.com/playlist/{id}")) target="_blank" rel="noreferrer" {"Inspect unfinished playlist in Spotify"}}}
                        details {summary {"Abandon pending save"}
                            form.save-form method="post" action="/preview/abandon" {(hidden(&s.csrf)) input type="hidden" name="preview_id" value=(p.id);
                                label.consent {input type="checkbox" name="confirm" value="yes" required;span {"I checked Spotify for an unfinished playlist. Leave it unchanged and discard local recovery."}}
                                button.quiet {"Abandon recovery"}
                            }
                        }
                    }
                } @else {
                    div.result-empty {
                        @let covers: Vec<_> = snapshot.tracks.values().filter_map(|t|t.image.as_deref()).take(5).collect();
                        @if !covers.is_empty() {div.cover-stack aria-hidden="true" {@for image in covers {img src=(image) alt="" width="120" height="120";}}}
                        h3 {"No selection yet"}
                    }
                }
            }
        }
    }
}

pub(super) fn track_details(
    track: &crate::model::Track,
    snapshot: &Snapshot,
    evidence: Option<&crate::model::Evidence>,
) -> Markup {
    let lastfm_artist = track.artists.first().map(|a| {
        format!(
            "https://www.last.fm/music/{}",
            url::form_urlencoded::byte_serialize(a.as_bytes()).collect::<String>()
        )
    });
    html! {details.track-details {summary {"Metadata & evidence"}
        dl {dt {"ISRC"} dd {(track.isrc.as_deref().unwrap_or("Not provided"))}
            dt {"Explicit"} dd {(track.explicit.map(|v|if v {"Yes"}else{"No"}).unwrap_or("Unknown"))}
            dt {"Release precision"} dd {(track.release_precision.as_deref().unwrap_or("Unknown"))}
            dt {"Sources"} dd {@for m in &track.memberships {p {(snapshot.sources.iter().find(|s|s.id==m.source).map(|s|s.name.as_str()).unwrap_or("Unavailable source")) @if let Some(at)=&m.added_at {small {"Added " (at)}}}}}
        }
        @if let Some(e)=evidence {
            @if !e.tags.is_empty() {h3 {"Track tags / Last.fm"} div.tag-list {@for tag in &e.tags {span {(tag)}}}}
            @if !e.artist_tags.is_empty() {h3 {"Artist tags / weaker evidence"} div.tag-list {@for tag in &e.artist_tags {span {(tag)}}}}
            @if e.tags.is_empty() && e.artist_tags.is_empty() {p.muted {"No current genre tags"}}
            @if !e.features.is_empty() {h3 {"Numeric features / ReccoBeats"} dl {@for (key,value) in &e.features {dt {(key)} dd {(format!("{value:.2}"))}}}}
            @else if e.recco_checked {p.muted {(e.feature_count) " numeric fields returned at last lookup. Values are not retained or have expired."}}
            @else {p.muted {"Numeric features not checked"}}
        } @else {p.muted {"No enrichment yet"}}
        @if let Some(url)=lastfm_artist {p.attribution {a href=(url) target="_blank" rel="noreferrer" {"Last.fm artist"} " / " a href="https://reccobeats.com" target="_blank" rel="noreferrer" {"ReccoBeats"}}}
    }}
}

pub(super) struct Capabilities {
    pub(super) lastfm: bool,
    pub(super) openai: bool,
    pub(super) history: Vec<history::Run>,
}

pub(super) fn history_view(
    runs: &[history::Run],
    page: usize,
    csrf: &str,
    running: bool,
) -> Markup {
    html! {
        @if runs.is_empty() {p.empty-line {"No prompt history yet"}}
        @for run in runs.iter().take(25) {
            article.history-entry {
                div.history-heading {time {(chrono::DateTime::from_timestamp(run.created_at,0).map(|d|d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default())} span.badge {(run.status)}}
                @if run.controls.get("refinement").is_some_and(|r|!r.is_null()) {small {"Refinement"}}
                h2 {(if run.prompt.is_empty() {"Manual filters"}else{&run.prompt})}
                p.muted {(run.latest_result.as_ref().map(|r|r.tracks.len()).unwrap_or(0)) " tracks" @if let Some(ms)=run.elapsed_ms {" / " (format!("{:.1}s",ms as f64/1000.))}}
                @if let Some(error)=&run.error {p.notice {(error)}}
                form method="post" action="/history/reuse" {(hidden(csrf)) input type="hidden" name="run_id" value=(run.id);button.quiet disabled[running] {(if run.controls.get("refinement").is_some_and(|r|!r.is_null()) {"Use original prompt"}else{"Use prompt again"})}}
                @for (label,result) in [("Original result",run.initial_result.as_ref()),("Latest result",run.latest_result.as_ref())] {
                    @if let Some(result)=result {
                        details.history-result {summary {(label) " / " (result.tracks.len())}
                            h3 {(result.name)}
                            @for line in &result.criteria {p.muted {(line)}}
                            @for line in &result.warnings {p.muted {(line)}}
                            @if let Some(id)=&result.playlist_id {a href=(format!("https://open.spotify.com/playlist/{id}")) target="_blank" rel="noreferrer" {"Open Spotify playlist"}}
                            (ui::scroll_region("Saved playlist tracks",html!{ol {@for track in &result.tracks {li {a href=(format!("https://open.spotify.com/track/{}",track.id)) target="_blank" rel="noreferrer" {(track.name)} small {(track.artists.join(", "))}}}}}))
                        }
                    }
                }
            }
        }
        div.pagination {@if page>0 {a href=(format!("/?tab=history&page={}",page-1)) {"Previous"}} @if runs.len()>25 {a href=(format!("/?tab=history&page={}",page+1)) {"Next"}}}
    }
}

pub(super) fn render(
    s: &Session,
    snapshot: &Snapshot,
    has_draft: bool,
    coverage: &Coverage,
    view: &View,
    preview: Option<&Preview>,
    capabilities: Capabilities,
) -> Markup {
    let connected = s.account.is_some();
    let running = s.job.running;
    let account_name = s
        .account
        .as_ref()
        .and_then(|a| a.profile.display_name.as_deref())
        .unwrap_or("Spotify account");
    let title = match view.tab.as_str() {
        "sources" => "Sources",
        "coverage" => "Coverage",
        "library" => "Library",
        "settings" => "Settings",
        "history" => "History",
        _ => "Build a playlist",
    };
    let q = view.q.trim().to_lowercase();
    let tracks: Vec<_> = snapshot
        .tracks
        .values()
        .filter(|t| {
            q.is_empty()
                || t.name.to_lowercase().contains(&q)
                || t.artists.iter().any(|a| a.to_lowercase().contains(&q))
        })
        .collect();
    let page = view.page.min(tracks.len().saturating_sub(1) / 50);
    let numeric = coverage
        .sample
        .iter()
        .filter(|id| {
            coverage
                .tracks
                .get(*id)
                .is_some_and(|e| e.feature_count > 0 || !e.features.is_empty())
        })
        .count();
    let tags = coverage
        .sample
        .iter()
        .filter(|id| coverage.tracks.get(*id).is_some_and(|e| e.tag_count > 0))
        .count();
    html! {(DOCTYPE) html lang="en" {head {
            meta charset="utf-8";meta name="viewport" content="width=device-width, initial-scale=1";
            title {"Promptlist / " (title)} link rel="stylesheet" href="/style.css";
            meta name="csrf-token" content=(s.csrf);
            script src="/app.js" defer {}
            script src="/player.js" defer {}
        } body {
            header {
                div.brand-cluster {a.brand.wordmark href="/" {"promptlist"}
                    div.header-status id="header-status" {
                        @if let Some(notice)=&s.notice {span.connection-notice title=(notice) role="status" {(notice)}}
                        (ui::job_status(&s.job.text,running,s.job.publishing,s.job.completed,s.job.total,&s.csrf))
                        span id="playback-status" role="status" aria-live="polite" {}
                    }
                }
                (ui::player(connected))
                div.account {
                @if connected {
                    button.quiet.account-trigger popovertarget="account-menu" aria-label="Account menu" {span.dot.online {} (account_name) img src="/icons/arrow-down" alt="" width="14" height="14";}
                    div.account-popover id="account-menu" popover="auto" {
                        a href="/?tab=settings" {"Settings & privacy"}
                        (action_form(&s.csrf,"/sync","Sync library","quiet",running))
                        (action_form(&s.csrf,"/logout","Sign out","quiet",false))
                        a.danger href="/?tab=settings#delete-data" {"Delete local data..."}
                    }
                } @else {(action_form(&s.csrf,"/auth/connect","Connect Spotify","quiet account-trigger",false))}
            }}
            main data-view=(view.tab) {
                @if title!="Build a playlist" {section.heading {h1 {(title)}}}
                (ui::navigation(title))
                @if title!="Build a playlist" {(ui::library_stats(snapshot))}
                @if title=="Build a playlist" {
                    (composer(s,snapshot,preview))
                } @else if !connected {
                    p.empty-line {"No connected library"}
                } @else if view.tab=="history" {
                    (history_view(&capabilities.history,view.page,&s.csrf,running))
                } @else if view.tab=="settings" {
                    section.settings-section {
                        h2 {"Music matching"}
                        p.muted {"Always active. Genre matching uses Last.fm artist names across the eligible library before numeric filtering. Track-only genre requests also send artist/title pairs. ReccoBeats receives IDs only for candidates requiring numeric checks. No account identity or playlist grouping is sent to these providers."}
                        p.muted {"Last.fm tags use a seven-day freshness policy when response headers do not specify one; restrictive headers take precedence. ReccoBeats no-store values stay within the build. Your library snapshot stays in Promptlist's private storage until you delete it; sync runs only when requested."}
                        h2 {"History & product analytics"}
                        p.muted {"Submitted prompts, interpretations, original and edited results, and export outcomes are saved in Promptlist's private storage for your history and developer review. Detailed app actions are kept for 90 days; daily action counts, outcomes and timing summaries for 365 days. Prompt history remains until account deletion. No keystrokes, passwords, OAuth tokens or raw provider responses are recorded. No third-party analytics service is used."}
                        h2 {"Request interpretation"}
                        p.muted {"Developer exports are separate private files and must be deleted separately. Result lists and activity logs are never sent to AI. No model training is enabled."}
                        p.muted {(if capabilities.openai {"Requests use local parsing first, then the configured OpenAI model to interpret subjective preferences. Refinements send your original request, previous preference interpretation and latest message, not your library or result tracks. If AI or provider evidence is unavailable, matching uses a disclosed best-effort local fallback. Do not put private account details in a prompt."} else {"Local request and preference parsing is active. OpenAI is not configured."})}
                        h2 {"Spotify playback"}
                        p.muted {"The Web Playback SDK uses your linked Spotify Premium account. A short-lived access token is supplied to Spotify's SDK in browser memory; the refresh token stays on the server. Connecting prepares a player but does not start music. Playback can move audio from another Spotify device. No audio is downloaded or analyzed by Promptlist."}
                    }
                    section.settings-section id="delete-data" {
                        h2 {"Local account data"}
                        p.muted {"Signing out ends this browser session and preserves your library. Disconnecting and deleting removes this account's local library and evidence; Spotify playlists stay unchanged. Revoke Promptlist access in your Spotify account's Apps page to remove the provider authorization."}
                        details {summary.danger {"Delete local data"} form.save-form method="post" action="/delete-data" {(hidden(&s.csrf)) label.consent {input type="checkbox" name="consent" value="yes" required;span {"Delete this account's local library, evidence, previews, prompt history and activity records."}} button.danger disabled[running] {"Delete local data and disconnect"}}}
                    }
                } @else if view.tab=="sources" {
                    div.toolbar {h2 {(if view.excluded {"Excluded sources"} else {"Library sources"})} (action_form(&s.csrf,"/sync",if has_draft {"Resume sync"} else {"Sync library"},"primary",running))}
                    div.source-tabs {a href="/?tab=sources" aria-current=[(!view.excluded).then_some("page")] {"All sources"} a href="/?tab=sources&excluded=true" aria-current=[view.excluded.then_some("page")] {"Excluded only"}}
                    p.muted {"Liked Songs, owned, collaborative and saved playlists. Spotify may deny contents of playlists you only follow."}
                    div.table-wrap {table {thead {tr {th {"Source"} th {"Items"} th {"Access"}}} tbody {@for source in snapshot.sources.iter().filter(|s|!view.excluded||!s.accessible) {tr {td {@if source.id=="liked" {"Liked Songs"} @else {a href=(format!("https://open.spotify.com/playlist/{}",source.id)) rel="noreferrer" target="_blank" {(source.name)}}} td {(source.count)} td {(source.reason.as_deref().unwrap_or("Accessible"))}}}}}
                    }
                    @if snapshot.sources.is_empty() {p.empty-line {"No completed sync"}}
                    @if view.excluded && snapshot.sources.iter().all(|s|s.accessible) {p.empty-line {"No excluded sources"}}
                } @else if view.tab=="coverage" {
                    div.toolbar {h2 {"Feature coverage"} span.muted {(numeric) " numeric / " (tags) " tagged / " (coverage.sample.len()) " selected"}}
                    form.coverage-form method="post" action="/coverage" {(hidden(&s.csrf))
                        label {"Source" select name="source" {option value="" {"Entire library"} @for source in &snapshot.sources {@if source.accessible {option value=(source.id) {(source.name)}}}}}
                        label {"Scope" select name="limit" {option value="0" selected {"All tracks"} option value="20" {"20-track sample"} option value="40" {"40-track sample"} option value="100" {"100-track sample"} option value="200" {"200-track sample"}}}
                        label.consent {input type="checkbox" name="tags" value="yes" disabled[!capabilities.lastfm];span {"Include track tags (slower)"}}
                        button.primary disabled[running||snapshot.tracks.is_empty()] {"Run coverage"}
                    }
                    @if !capabilities.lastfm {p.notice {"Last.fm key missing. Numeric features only."}}
                    p.muted {"Optional diagnostic, not a prerequisite for building playlists. Counts survive a check; non-cacheable values do not. Builds fetch fresh evidence as needed."}
                    @let coverage_page = view.page.min(coverage.sample.len().saturating_sub(1)/50);
                    div.table-wrap {table {thead {tr {th {"Track"} th {"Numeric evidence"} th {"Tags returned"}}} tbody {@for id in coverage.sample.iter().skip(coverage_page*50).take(50) {@if let Some(track)=snapshot.tracks.get(id) {tr {td {a href=(format!("https://open.spotify.com/track/{id}")) target="_blank" rel="noreferrer" {(track.name)} small {(track.artists.join(", "))}} @if let Some(e)=coverage.tracks.get(id) {td {(if !e.recco_checked {"Not checked"} else if e.feature_count>0 {"Found at last check"} else {"Not found"})} td {@if e.lastfm_checked {(e.tag_count)} @else {"Not checked"}}} @else {td {"Not checked"} td {"Not checked"}}}}}}}
                    }
                    div.pagination {span.muted {(coverage.sample.len()) " tracks"} @if coverage_page>0 {a href=(format!("/?tab=coverage&page={}",coverage_page-1)) {"Previous"}} @if (coverage_page+1)*50<coverage.sample.len() {a href=(format!("/?tab=coverage&page={}",coverage_page+1)) {"Next"}}}
                    p.attribution {a href="https://reccobeats.com/" target="_blank" rel="noreferrer" {"ReccoBeats"} " / " a href="https://www.last.fm/" target="_blank" rel="noreferrer" {"Last.fm"}}
                } @else {
                    div.toolbar {form.search method="get" action="/" {input type="hidden" name="tab" value="library";label.sr-only for="q" {"Search library"} input id="q" name="q" value=(view.q) placeholder="Track or artist" maxlength="200";button.quiet {"Search"}} div.actions {(action_form(&s.csrf,"/sync",if has_draft {"Resume sync"} else {"Sync library"},"primary",running)) @if has_draft {form method="post" action="/sync" {(hidden(&s.csrf)) input type="hidden" name="restart" value="true";button.quiet disabled[running] {"Start new sync"}}}}}
    div.table-wrap {table.library-table {thead {tr {th {"Track / evidence"} th {"Release"} th {"Sources"} th {"Duration"}}} tbody {@for track in tracks.iter().skip(page*50).take(50) {tr {td {div.track {@if let Some(image)=&track.image {img src=(image) alt="" loading="lazy" width="40" height="40";} div {a href=(format!("https://open.spotify.com/track/{}",track.id)) target="_blank" rel="noreferrer" {(track.name)} small {(track.artists.join(", "))}}} (track_details(track,snapshot,coverage.tracks.get(&track.id)))} td data-label="Release" {(track.release_date.as_deref().unwrap_or("Unknown"))} td data-label="Sources" {(track.memberships.len())} td data-label="Duration" {(track.duration_ms.map(|ms|format!("{}:{:02}",ms/60000,(ms/1000)%60)).unwrap_or_else(||"Unknown".into()))}}}}}
                    }
                    @if tracks.is_empty() {p.empty-line {(if snapshot.completed_at.is_some() {"No matching tracks"} else {"No completed library sync"})}}
                    div.pagination {span.muted {(tracks.len()) " tracks"} @if page>0 {a href=(format!("/?tab=library&page={}&q={}",page-1,url::form_urlencoded::byte_serialize(view.q.as_bytes()).collect::<String>())) {"Previous"}} @if (page+1)*50<tracks.len() {a href=(format!("/?tab=library&page={}&q={}",page+1,url::form_urlencoded::byte_serialize(view.q.as_bytes()).collect::<String>())) {"Next"}}}
                }
            }
            footer {span {"Promptlist / 0.2"} @if let Some(at)=snapshot.completed_at {span {"Last sync " (chrono::DateTime::from_timestamp(at,0).map(|d|d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default())}} a href="https://www.spotify.com/" rel="noreferrer" target="_blank" {"Music metadata by Spotify"}}
        }}}
}
