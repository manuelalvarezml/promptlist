use crate::{
    Result,
    config::Config,
    history, message,
    model::{Coverage, Snapshot},
    now,
    preview::{Phase, Preview},
    provider::{self, Enrichment, PendingAuth, Profile, Spotify},
    store::Store,
    sync::{Draft, Stage},
};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Form, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maud::{DOCTYPE, Markup, html};
mod playback;
pub mod serverless;
mod views;
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use views::{Capabilities, render};

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    runtime: Arc<Mutex<Runtime>>,
    http: reqwest::Client,
    enrichment: Arc<Enrichment>,
    gate: Arc<tokio::sync::Mutex<Instant>>,
    quota: Arc<AtomicBool>,
}
struct Runtime {
    store: Store,
    sessions: HashMap<String, Session>,
    last_views: HashMap<String, String>,
}
struct Session {
    auto_sync_pending: bool,
    csrf: String,
    created: i64,
    pending: Option<PendingAuth>,
    account: Option<Arc<Account>>,
    job: Job,
    notice: Option<String>,
    build: Option<BuildForm>,
}
struct Account {
    profile: Profile,
    api: Spotify,
}
#[derive(Clone, Default)]
struct Job {
    id: String,
    running: bool,
    text: String,
    publishing: bool,
    abort: Option<tokio::task::AbortHandle>,
    kind: &'static str,
    started: Option<Instant>,
    completed: Option<usize>,
    total: Option<usize>,
}

fn expire_sessions(rt: &mut Runtime) {
    for session in rt
        .sessions
        .values()
        .filter(|s| now() - s.created >= 86400 && s.job.running && !s.job.publishing)
    {
        end_history_job(rt, session, "interrupted", None);
    }
    rt.sessions.retain(|_, s| {
        let keep = now() - s.created < 86400 || (s.job.running && s.job.publishing);
        if !keep && let Some(abort) = s.job.abort.take() {
            abort.abort();
        }
        keep
    });
    rt.last_views.retain(|id, _| rt.sessions.contains_key(id));
}

fn spawn_job(
    app: App,
    id: String,
    job: String,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let handle = tokio::spawn(future);
    let mut rt = app.runtime.lock().unwrap();
    if active(&rt, &id, &job) {
        rt.sessions.get_mut(&id).unwrap().job.abort = Some(handle.abort_handle());
    } else {
        handle.abort();
    }
}

impl App {
    pub fn new(config: Config, store: Store) -> Result<Self> {
        let http = provider::http_client()?;
        let enrichment = Arc::new(Enrichment::new(http.clone(), config.lastfm_key.clone()));
        Ok(Self {
            config: Arc::new(config),
            runtime: Arc::new(Mutex::new(Runtime {
                store,
                sessions: HashMap::new(),
                last_views: HashMap::new(),
            })),
            http,
            enrichment,
            gate: Arc::new(tokio::sync::Mutex::new(Instant::now())),
            quota: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn cleanup(&self) {
        let mut rt = self.runtime.lock().unwrap();
        expire_sessions(&mut rt);
        let _ = rt.store.purge_expired();
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/style.css", get(style))
        .route("/theme/{name}", get(theme_asset))
        .route("/app.js", get(browser_controller))
        .route(
            "/player.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../assets/player.js"),
                )
            }),
        )
        .route("/api/playback/token", post(playback::token))
        .route("/api/playback/play", post(playback::play))
        .route("/api/job", get(job_status))
        .route("/refine", post(refine))
        .route("/icons/{name}", get(icon_asset))
        .route("/health", get(|| async { "ok" }))
        .route("/auth/connect", post(connect))
        .route("/callback", get(callback))
        .route("/sync", post(start_sync))
        .route("/coverage", post(start_coverage))
        .route("/build", post(build))
        .route("/history/reuse", post(reuse_prompt))
        .route("/preview/edit", post(edit_preview))
        .route("/preview/abandon", post(abandon_preview))
        .route("/publish", post(publish))
        .route("/pause", post(pause))
        .route("/logout", post(disconnect))
        .route("/disconnect", post(delete_data))
        .route("/delete-data", post(delete_data))
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn_with_state(app.clone(), security))
        .with_state(app)
}

async fn security(State(app): State<App>, request: Request, next: Next) -> Response {
    let expected = app.config.host_header();
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(expected)
    {
        return (StatusCode::FORBIDDEN, "Invalid host").into_response();
    }
    if request.method() != axum::http::Method::GET
        && request.method() != axum::http::Method::HEAD
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != Some(app.config.origin().as_str())
    {
        return (StatusCode::FORBIDDEN, "Invalid origin").into_response();
    }
    let is_callback = request.uri().path() == "/callback";
    let event_kind = if request.method() == axum::http::Method::POST {
        match request.uri().path() {
            "/build" => Some("build_submit"),
            "/refine" => Some("refine_submit"),
            "/sync" => Some("sync_submit"),
            "/coverage" => Some("coverage_submit"),
            "/preview/edit" => Some("preview_edit_submit"),
            "/publish" => Some("export_submit"),
            "/preview/abandon" => Some("recovery_abandon_submit"),
            "/pause" => Some("pause_submit"),
            "/logout" => Some("sign_out"),
            "/history/reuse" => Some("history_reuse_submit"),
            "/api/playback/play" => Some("playback_start"),
            _ => None,
        }
    } else {
        None
    };
    let actor = event_kind.and_then(|_| {
        let rt = app.runtime.lock().unwrap();
        session_id(&rt, request.headers()).ok().and_then(|id| {
            rt.sessions[&id]
                .account
                .as_ref()
                .map(|a| a.profile.account().to_owned())
        })
    });
    let started = Instant::now();
    let mut response = next.run(request).await;
    if let (Some(account), Some(kind)) = (actor, event_kind) {
        let rt = app.runtime.lock().unwrap();
        audit_event(
            &rt,
            &account,
            kind,
            None,
            serde_json::json!({"http_status":response.status().as_u16(),"request_ms":started.elapsed().as_millis() as u64}),
        );
    }
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    // Native form POSTs need a non-null Origin; never forward callback codes.
    headers.insert(
        "referrer-policy",
        if is_callback {
            "no-referrer"
        } else {
            "same-origin"
        }
        .parse()
        .unwrap(),
    );
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("x-frame-options", "DENY".parse().unwrap());
    if app.config.hosted() {
        headers.insert(
            "strict-transport-security",
            "max-age=31536000".parse().unwrap(),
        );
    }
    headers.insert("content-security-policy","default-src 'none'; script-src 'self' https://sdk.scdn.co; connect-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' https://i.scdn.co https://*.scdn.co; frame-src https://sdk.scdn.co; form-action 'self' https://accounts.spotify.com; base-uri 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}

fn audit_event(
    rt: &Runtime,
    account: &str,
    kind: &str,
    run: Option<&str>,
    detail: serde_json::Value,
) {
    if history::event(&rt.store.db, account, kind, run, detail).is_err() {
        eprintln!("Promptlist: local audit event could not be stored.");
    }
}

fn end_history_job(rt: &Runtime, session: &Session, status: &str, error: Option<String>) {
    let Some(account) = &session.account else {
        return;
    };
    let elapsed = session
        .job
        .started
        .map(|at| at.elapsed().as_millis() as u64);
    audit_event(
        rt,
        account.profile.account(),
        "job_finished",
        Some(&session.job.id),
        serde_json::json!({"operation":session.job.kind,"status":status,"elapsed_ms":elapsed,"error":error}),
    );
    if let Ok(Some(mut run)) =
        history::get(&rt.store.db, account.profile.account(), &session.job.id)
    {
        run.status = status.into();
        run.error = error;
        run.elapsed_ms = elapsed;
        run.updated_at = now();
        if history::put(&rt.store.db, account.profile.account(), &run).is_err() {
            eprintln!("Promptlist: history status could not be stored.");
        }
    }
}

fn cookie_id(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        for cookie in cookie::Cookie::split_parse(value.to_str().ok()?).flatten() {
            if cookie.name() == "promptlist_session" {
                return Some(cookie.value().to_owned());
            }
        }
    }
    None
}

fn session_id(rt: &Runtime, headers: &HeaderMap) -> Result<String> {
    cookie_id(headers)
        .filter(|id| {
            rt.sessions
                .get(id)
                .is_some_and(|s| now() - s.created < 86400)
        })
        .ok_or_else(|| message("Session expired. Open the app and connect again."))
}

#[derive(Deserialize)]
struct Action {
    csrf: String,
    #[serde(default)]
    restart: bool,
    #[serde(default)]
    consent: Option<String>,
}

#[cfg(test)]
#[derive(Default, serde::Serialize, Deserialize)]
struct Settings {
    music_matching: bool,
}

fn authorize(rt: &Runtime, headers: &HeaderMap, action: &Action) -> Result<String> {
    let id = session_id(rt, headers)?;
    if rt.sessions[&id].csrf != action.csrf {
        return Err(message("Invalid form token. Reload the page."));
    }
    Ok(id)
}

fn error_response(error: crate::Error) -> Response {
    (StatusCode::BAD_REQUEST,Html(html!{(DOCTYPE) html {head {meta charset="utf-8";meta name="viewport" content="width=device-width, initial-scale=1";link rel="stylesheet" href="/style.css";title {"Promptlist"}} body {main {h1 {"Request stopped"} p {(error.to_string())} a href="/" {"Return to library"}}}}}.into_string())).into_response()
}
fn notice(app: &App, id: &str, text: String) {
    if let Some(s) = app.runtime.lock().unwrap().sessions.get_mut(id) {
        s.notice = Some(text);
    }
}

#[derive(Default, Deserialize)]
struct View {
    #[serde(default)]
    section: String,
    #[serde(default)]
    ui: String,
    #[serde(default)]
    tab: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: usize,
    #[serde(default)]
    excluded: bool,
}

async fn home(
    State(app): State<App>,
    headers: HeaderMap,
    Query(mut view): Query<View>,
) -> Response {
    let local = !app.config.hosted();
    let web_preview = view.ui == "web"
        || (view.ui != "local"
            && headers
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(';').any(|c| c.trim() == "promptlist_ui=web")));
    let diagnostics = local && !web_preview;
    if view.tab == "sources" {
        view.tab = "library".into();
        view.section = "sources".into();
    }
    if view.tab == "coverage" && !diagnostics {
        view.tab = "library".into();
    }
    if !matches!(
        view.tab.as_str(),
        "library" | "coverage" | "history" | "settings"
    ) {
        view.tab.clear();
    }
    let mut rt = app.runtime.lock().unwrap();
    expire_sessions(&mut rt);
    let (id, new_cookie) = match session_id(&rt, &headers) {
        Ok(id) => (id, false),
        Err(_) => {
            let capacity = if app.config.hosted() { 128 } else { 16 };
            if rt.sessions.len() >= capacity && app.config.hosted() {
                // Visitors cannot exhaust every slot and lock out signed-in accounts.
                let oldest = rt
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.account.is_none() && s.pending.is_none())
                    .min_by_key(|(_, s)| s.created)
                    .map(|(id, _)| id.clone());
                if let Some(oldest) = oldest {
                    rt.sessions.remove(&oldest);
                    rt.last_views.remove(&oldest);
                }
            }
            if rt.sessions.len() >= capacity {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many browser sessions. Please try again later.",
                )
                    .into_response();
            }
            let id = provider::random_secret();
            rt.sessions.insert(
                id.clone(),
                Session {
                    auto_sync_pending: false,
                    csrf: provider::random_secret(),
                    created: now(),
                    pending: None,
                    account: None,
                    job: Job::default(),
                    notice: None,
                    build: None,
                },
            );
            (id, true)
        }
    };
    if let Some(account) = rt.sessions[&id].account.clone() {
        if history::ensure_user(&rt.store.db, account.profile.account()).is_err() {
            return error_response(message("Cannot initialize private history."));
        }
        let tab = match view.tab.as_str() {
            "library" | "sources" | "coverage" | "settings" | "history" => view.tab.as_str(),
            _ => "build",
        };
        // No raw URLs or search text; repeated status refreshes are not user navigation.
        let fingerprint = serde_json::json!([
            tab,
            view.page,
            view.excluded,
            view.q.chars().take(200).collect::<String>()
        ])
        .to_string();
        if rt.last_views.get(&id) != Some(&fingerprint) {
            audit_event(
                &rt,
                account.profile.account(),
                "view",
                None,
                serde_json::json!({"tab":tab,"page":view.page,"excluded_only":view.excluded,"search_length":view.q.chars().count().min(200)}),
            );
            rt.last_views.insert(id.clone(), fingerprint);
        }
    }
    let s = &rt.sessions[&id];
    let account = s.account.clone();
    let mut data = if let Some(a) = &account {
        let key = a.profile.account();
        match (
            rt.store.get::<Snapshot>(key, "snapshot"),
            rt.store.get::<Draft>(key, "draft"),
            rt.store.get::<Coverage>(key, "coverage"),
        ) {
            (Ok(snapshot), Ok(draft), Ok(coverage)) => (
                snapshot.unwrap_or_default(),
                draft.is_some(),
                coverage.unwrap_or_default(),
            ),
            _ => return error_response(message("Cannot read local library cache.")),
        }
    } else {
        (Snapshot::default(), false, Coverage::default())
    };
    if let Some(account) = &account {
        let additional: Coverage = rt
            .store
            .get(account.profile.account(), "evidence")
            .ok()
            .flatten()
            .unwrap_or_default();
        for (id, e) in additional.tracks {
            if data.0.tracks.contains_key(&id)
                && data
                    .2
                    .tracks
                    .get(&id)
                    .is_none_or(|old| old.updated_at < e.updated_at)
            {
                data.2.tracks.insert(id, e);
            }
        }
        if let Ok(tags) = rt.store.tags(account.profile.account()) {
            for (id, track) in &data.0.tracks {
                let Some(artist) = track.artists.first() else {
                    continue;
                };
                let evidence = data.2.tracks.entry(id.clone()).or_default();
                if let Some(value) = tags.get(&crate::model::tag_key(artist, None)) {
                    evidence.artist_tags = value.tags.clone();
                }
                if let Some(value) = tags.get(&crate::model::tag_key(artist, Some(&track.name))) {
                    evidence.tags = value.tags.clone();
                    evidence.lastfm_expires_at = value.expires_at;
                }
            }
        }
    }
    for evidence in data.2.tracks.values_mut() {
        evidence.feature_count = evidence.feature_count.max(evidence.features.len());
        if evidence.features_expires_at <= now() {
            evidence.features.clear();
        }
        if evidence.lastfm_expires_at <= now() {
            evidence.tags.clear();
        }
    }
    let preview = account.as_ref().and_then(|a| {
        rt.store
            .get::<Preview>(a.profile.account(), "preview")
            .ok()
            .flatten()
    });
    let history_rows = match account
        .as_ref()
        .filter(|_| view.tab == "history")
        .map(|a| history::list(&rt.store.db, a.profile.account(), view.page))
        .transpose()
    {
        Ok(rows) => rows.unwrap_or_default(),
        Err(e) => return error_response(e),
    };
    if s.build.is_none() {
        let saved = account
            .as_ref()
            .zip(preview.as_ref())
            .and_then(|(a, p)| {
                p.run_id.as_deref().and_then(|run| {
                    history::get(&rt.store.db, a.profile.account(), run)
                        .ok()
                        .flatten()
                })
            })
            .and_then(|r| serde_json::from_value::<BuildForm>(r.controls).ok());
        if let Some(saved) = saved {
            rt.sessions.get_mut(&id).unwrap().build = Some(saved);
        }
    }
    let body = render(
        &rt.sessions[&id],
        &data.0,
        data.1,
        &data.2,
        &view,
        preview.as_ref(),
        Capabilities {
            local,
            diagnostics,
            lastfm: app.enrichment.has_lastfm(),
            openai: app.config.openai_key.is_some(),
            history: history_rows,
        },
    );
    let mut response = Html(body.into_string()).into_response();
    if new_cookie {
        response.headers_mut().insert(
            header::SET_COOKIE,
            app.config.session_cookie(&id).parse().unwrap(),
        );
    }
    if local && matches!(view.ui.as_str(), "web" | "local") {
        response.headers_mut().append(
            header::SET_COOKIE,
            format!("promptlist_ui={}; Path=/; SameSite=Lax; HttpOnly", view.ui)
                .parse()
                .unwrap(),
        );
    }
    response
}

async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../assets/style.css"),
    )
}

async fn theme_asset(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    // An explicit allowlist keeps optional, locally licensed assets out of Git
    // without exposing arbitrary files or requiring them at compile time.
    let (file, mime) = match name.as_str() {
        "background.jpg" => ("island-background.jpg", "image/jpeg"),
        "satoshi-regular.woff2" => ("fonts/satoshi-regular.woff2", "font/woff2"),
        "satoshi-medium.woff2" => ("fonts/satoshi-medium.woff2", "font/woff2"),
        "satoshi-bold.woff2" => ("fonts/satoshi-bold.woff2", "font/woff2"),
        "satoshi-black.woff2" => ("fonts/satoshi-black.woff2", "font/woff2"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match tokio::fs::read(std::path::Path::new("assets").join(file)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn browser_controller() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/app.js"),
    )
}

async fn job_status(State(app): State<App>, headers: HeaderMap) -> Response {
    let rt = app.runtime.lock().unwrap();
    let id = match session_id(&rt, &headers) {
        Ok(id) => id,
        Err(_) => return (StatusCode::UNAUTHORIZED, "Session expired").into_response(),
    };
    let s = &rt.sessions[&id];
    if s.account.is_none() {
        return (StatusCode::UNAUTHORIZED, "Connect Spotify first").into_response();
    }
    axum::Json(serde_json::json!({"id":s.job.id,"running":s.job.running,"status_html":crate::ui::job_status(&s.job.text,s.job.running,s.job.publishing,s.job.completed,s.job.total,&s.csrf).into_string()})).into_response()
}

async fn icon_asset(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    let asset = match name.as_str() {
        "arrow-up" => include_str!("../assets/arrow-up.svg"),
        "arrow-down" => include_str!("../assets/arrow-down.svg"),
        "x" => include_str!("../assets/x.svg"),
        "play" => include_str!("../assets/play.svg"),
        "pause" => include_str!("../assets/pause.svg"),
        "skip-back" => include_str!("../assets/skip-back.svg"),
        "skip-forward" => include_str!("../assets/skip-forward.svg"),
        "volume-2" => include_str!("../assets/volume-2.svg"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "image/svg+xml")], asset).into_response()
}

async fn connect(
    State(app): State<App>,
    headers: HeaderMap,
    Form(action): Form<Action>,
) -> Response {
    let mut rt = app.runtime.lock().unwrap();
    let id = match authorize(&rt, &headers, &action) {
        Ok(id) => id,
        Err(e) => return error_response(e),
    };
    let s = rt.sessions.get_mut(&id).unwrap();
    if s.job.running || s.account.is_some() {
        return error_response(message(
            "Disconnect the current account before reconnecting.",
        ));
    }
    let (url, pending) = provider::authorization(&app.config.client_id, &app.config.redirect);
    s.csrf = provider::random_secret();
    s.pending = Some(pending);
    Redirect::to(url.as_str()).into_response()
}

#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let (id, pending, auth_epoch) = {
        let mut rt = app.runtime.lock().unwrap();
        let id = match session_id(&rt, &headers) {
            Ok(id) => id,
            Err(e) => return error_response(e),
        };
        let Some(pending) = rt.sessions.get_mut(&id).unwrap().pending.take() else {
            return error_response(message("Login already used or expired. Connect again."));
        };
        if query.state.as_deref() != Some(&pending.state)
            || pending.created.elapsed() > Duration::from_secs(600)
        {
            return error_response(message("Login verification failed. Connect again."));
        }
        let auth_epoch = rt.sessions[&id].csrf.clone();
        (id, pending, auth_epoch)
    };
    if query.error.is_some() {
        notice(&app, &id, "Spotify authorization was not completed.".into());
        return Redirect::to("/").into_response();
    }
    let Some(code) = query.code else {
        return error_response(message("Spotify callback is missing its code."));
    };
    let result = async {
        let tokens = provider::exchange(
            &app.http,
            &app.config.client_id,
            &app.config.redirect,
            &code,
            pending.verifier,
        )
        .await?;
        let api = Spotify::new(
            app.http.clone(),
            app.config.client_id.clone(),
            tokens,
            app.gate.clone(),
            app.quota.clone(),
        );
        let profile: Profile = serde_json::from_value(api.get("/v1/me").await?)?;
        if profile.id.is_empty() || profile.account().is_empty() {
            return Err(message("Spotify did not identify the account."));
        }
        if !app.config.allows_user(profile.account()) {
            return Err(message(
                "This Spotify account is not enabled for this private beta.",
            ));
        }
        Ok::<_, crate::Error>(Arc::new(Account { profile, api }))
    }
    .await;
    match result {
        Ok(account) => {
            let mut rt = app.runtime.lock().unwrap();
            if let Some(s) = rt.sessions.get_mut(&id)
                && s.csrf == auth_epoch
            {
                s.account = Some(account);
                s.auto_sync_pending = true;
                s.csrf = provider::random_secret();
                s.notice = Some("Spotify connected.".into());
                let account = s.account.as_ref().unwrap().profile.account().to_owned();
                if history::ensure_user(&rt.store.db, &account).is_ok() {
                    audit_event(&rt, &account, "signed_in", None, serde_json::json!({}));
                }
            }
        }
        Err(e) => notice(&app, &id, e.to_string()),
    }
    Redirect::to("/").into_response()
}

fn begin_job(
    rt: &mut Runtime,
    headers: &HeaderMap,
    action: &Action,
    text: &str,
) -> Result<(String, String, Arc<Account>)> {
    let id = authorize(rt, headers, action)?;
    let s = &rt.sessions[&id];
    let account = s
        .account
        .clone()
        .ok_or_else(|| message("Connect Spotify first."))?;
    if rt.sessions.values().any(|s| {
        s.job.running
            && s.account
                .as_ref()
                .is_some_and(|a| a.profile.account() == account.profile.account())
    }) {
        return Err(message("An operation is already running for this account."));
    }
    let job_id = provider::random_secret();
    history::ensure_user(&rt.store.db, account.profile.account())?;
    let kind = match text {
        "Building playlist preview" => "build",
        "Starting library sync" => "sync",
        "Saving private playlist" => "export",
        _ => "coverage",
    };
    audit_event(
        rt,
        account.profile.account(),
        "job_started",
        Some(&job_id),
        serde_json::json!({"operation":kind}),
    );
    let s = rt.sessions.get_mut(&id).unwrap();
    s.job = Job {
        id: job_id.clone(),
        running: true,
        text: text.into(),
        publishing: false,
        abort: None,
        kind,
        started: Some(Instant::now()),
        completed: None,
        total: None,
    };
    s.notice = None;
    Ok((id, job_id, account))
}

fn active(rt: &Runtime, id: &str, job: &str) -> bool {
    rt.sessions.get(id).is_some_and(|s| {
        s.account.is_some()
            && s.job.id == job
            && s.job.running
            && (now() - s.created < 86400 || s.job.publishing)
    })
}

fn finish(app: &App, id: &str, job: &str, result: Result<()>, success: &str) {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return;
    }
    end_history_job(
        &rt,
        &rt.sessions[id],
        if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
        result.as_ref().err().map(ToString::to_string),
    );
    let s = rt.sessions.get_mut(id).unwrap();
    s.job.running = false;
    s.job.text = match result {
        Ok(()) => success.into(),
        Err(e) => format!("Paused: {e}"),
    };
}

async fn start_sync(
    State(app): State<App>,
    headers: HeaderMap,
    Form(action): Form<Action>,
) -> Response {
    if action.consent.as_deref() == Some("automatic") {
        let mut rt = app.runtime.lock().unwrap();
        let id = match authorize(&rt, &headers, &action) {
            Ok(id) => id,
            Err(e) => return error_response(e),
        };
        if !std::mem::take(&mut rt.sessions.get_mut(&id).unwrap().auto_sync_pending) {
            return Redirect::to("/").into_response();
        }
    }
    let setup = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &action)?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        let draft = if action.restart {
            Draft::incremental(
                account.profile.id.clone(),
                rt.store.get(account.profile.account(), "snapshot")?,
            )
        } else {
            rt.store
                .get(account.profile.account(), "draft")?
                .filter(|d: &Draft| now() - d.started_at < 3600)
                .unwrap_or(Draft::incremental(
                    account.profile.id.clone(),
                    rt.store.get(account.profile.account(), "snapshot")?,
                ))
        };
        if rt
            .store
            .get::<Preview>(account.profile.account(), "preview")?
            .is_some_and(|p| matches!(p.phase, Phase::Creating | Phase::Created))
        {
            return Err(message(
                "Finish or explicitly abandon the pending Spotify save before syncing a new library.",
            ));
        }
        let (id, job, account) = begin_job(&mut rt, &headers, &action, "Starting library sync")?;
        rt.sessions.get_mut(&id).unwrap().auto_sync_pending = false;
        if let Err(error) = rt.store.put(account.profile.account(), "draft", &draft) {
            rt.sessions.get_mut(&id).unwrap().job.running = false;
            return Err(error);
        }
        account.api.acknowledge_quota();
        Ok::<_, crate::Error>((id, job, account, draft))
    })();
    let (id, job, account, mut draft) = match setup {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    spawn_job(app.clone(), id.clone(), job.clone(), async move {
        let result = async {
            for _ in 0..2000 {
                if !active(&app.runtime.lock().unwrap(), &id, &job) {
                    return Ok(());
                }
                // Mutate a copy: a failure after ingest must not duplicate a page on resume.
                let mut next = draft.clone();
                next.advance(&account.api).await?;
                let mut rt = app.runtime.lock().unwrap();
                if !active(&rt, &id, &job) {
                    return Ok(());
                }
                if next.stage == Stage::Finished {
                    rt.store
                        .promote(account.profile.account(), &next.snapshot)?;
                    return Ok(());
                }
                rt.store.put(account.profile.account(), "draft", &next)?;
                let job = &mut rt.sessions.get_mut(&id).unwrap().job;
                job.text = next.progress();
                (job.completed, job.total) = next.progress_counts();
                draft = next;
            }
            Err(message(
                "Request safety cap reached. Resume to continue from the checkpoint.",
            ))
        }
        .await;
        finish(&app, &id, &job, result, "Library sync complete");
    });
    Redirect::to("/").into_response()
}

#[derive(Deserialize)]
struct CoverageForm {
    csrf: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    tags: Option<String>,
}

fn coverage_pool(snapshot: &Snapshot, source: &str, limit: usize) -> Result<Vec<String>> {
    if limit > 200 && limit != 0 {
        return Err(message(
            "Choose a sample of up to 200 tracks or all tracks.",
        ));
    }
    if !source.is_empty()
        && !snapshot
            .sources
            .iter()
            .any(|s| s.id == source && s.accessible)
    {
        return Err(message(
            "Selected source is unavailable. Sync the library and choose again.",
        ));
    }
    let mut selected = snapshot.clone();
    selected.tracks.retain(|_, t| {
        t.memberships.iter().any(|m| {
            (source.is_empty() || m.source == source)
                && snapshot
                    .sources
                    .iter()
                    .any(|s| s.id == m.source && s.accessible)
        })
    });
    let size = if limit == 0 {
        selected.tracks.len()
    } else {
        limit
    };
    Ok(crate::sync::sample(&selected, size))
}

async fn start_coverage(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<CoverageForm>,
) -> Response {
    if app.config.hosted() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let action = basic_action(form.csrf);
    let limit = form.limit.unwrap_or(40);
    let setup = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &action)?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        let snapshot: Snapshot = rt
            .store
            .get(account.profile.account(), "snapshot")?
            .ok_or_else(|| message("Complete a library sync first."))?;
        if snapshot.tracks.is_empty() {
            return Err(message("The library is empty."));
        }
        let mut coverage: Coverage = rt
            .store
            .get(account.profile.account(), "coverage")?
            .unwrap_or_default();
        let sample = coverage_pool(&snapshot, &form.source, limit)?;
        if sample.is_empty() {
            return Err(message("The selected source has no accessible tracks."));
        }
        let include_tags = form.tags.as_deref() == Some("yes");
        // A diagnostic resume reuses completed counts, never expired feature values.
        if coverage.completed_at.is_some()
            || coverage.sample != sample
            || coverage.include_tags != include_tags
        {
            coverage = Coverage {
                sample,
                include_tags,
                ..Default::default()
            };
        }
        let (id, job, account) = begin_job(&mut rt, &headers, &action, "Checking source coverage")?;
        if let Err(error) = rt
            .store
            .put(account.profile.account(), "coverage", &coverage)
        {
            rt.sessions.get_mut(&id).unwrap().job.running = false;
            return Err(error);
        }
        Ok::<_, crate::Error>(Some((id, job, account, snapshot, coverage)))
    })();
    let (id, job, account, snapshot, mut coverage) = match setup {
        Ok(Some(v)) => v,
        Ok(None) => return Redirect::to("/?tab=settings").into_response(),
        Err(e) => return error_response(e),
    };
    spawn_job(app.clone(), id.clone(), job.clone(), async move {
        let result = async {
            let pending: Vec<_> = coverage
                .sample
                .iter()
                .filter(|id| !coverage.tracks.get(*id).is_some_and(|e| e.recco_checked))
                .cloned()
                .collect();
            for batch in pending.chunks(40) {
                if !active(&app.runtime.lock().unwrap(), &id, &job) {
                    return Ok(());
                }
                let response = app.enrichment.features_with_policy(batch).await?;
                let mut features = response.features;
                for track in batch {
                    let e = coverage.tracks.entry(track.clone()).or_default();
                    e.recco_checked = true;
                    e.features = features.remove(track).unwrap_or_default();
                    e.feature_count = e.features.len();
                    e.features_expires_at = if response.retention_allowed {
                        response.expires_at
                    } else {
                        0
                    };
                    e.updated_at = now();
                }
                save_coverage(&app, &id, &job, &account, &coverage)?;
            }
            if coverage.include_tags && app.enrichment.has_lastfm() {
                for track_id in coverage.sample.clone() {
                    if !active(&app.runtime.lock().unwrap(), &id, &job) {
                        return Ok(());
                    }
                    let existing = coverage.tracks.get(&track_id);
                    if existing.is_some_and(|e| e.lastfm_checked) {
                        continue;
                    }
                    let track = &snapshot.tracks[&track_id];
                    let Some(artist) = track.artists.first() else {
                        continue;
                    };
                    let (tags, url, expires) = app.enrichment.tags(artist, &track.name).await?;
                    let e = coverage.tracks.entry(track_id).or_default();
                    e.lastfm_checked = true;
                    e.tag_count = tags.len();
                    e.tags = tags;
                    e.lastfm_url = url;
                    e.lastfm_expires_at = expires;
                    save_coverage(&app, &id, &job, &account, &coverage)?;
                }
            }
            coverage.completed_at = Some(now());
            save_coverage(&app, &id, &job, &account, &coverage)?;
            Ok(())
        }
        .await;
        finish(&app, &id, &job, result, "Coverage check complete");
    });
    Redirect::to("/?tab=coverage").into_response()
}

fn save_coverage(
    app: &App,
    id: &str,
    job: &str,
    account: &Account,
    coverage: &Coverage,
) -> Result<()> {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return Err(message("Operation cancelled."));
    }
    let mut persisted = coverage.clone();
    for e in persisted.tracks.values_mut() {
        e.feature_count = e.feature_count.max(e.features.len());
        if e.features_expires_at <= now() {
            e.features.clear();
        }
        if e.lastfm_expires_at <= now() {
            e.tags.clear();
            e.lastfm_url = None;
        }
        e.artist_tags.clear();
    }
    rt.store
        .put(account.profile.account(), "coverage", &persisted)?;
    let checked = coverage
        .sample
        .iter()
        .filter(|id| coverage.tracks.get(*id).is_some_and(|e| e.recco_checked))
        .count();
    let tags = coverage
        .sample
        .iter()
        .filter(|id| coverage.tracks.get(*id).is_some_and(|e| e.lastfm_checked))
        .count();
    rt.sessions.get_mut(id).unwrap().job.text = format!(
        "Coverage: {checked} / {} numeric checks{}",
        coverage.sample.len(),
        if coverage.include_tags {
            format!("; {tags} tag checks")
        } else {
            String::new()
        }
    );
    Ok(())
}

async fn pause(State(app): State<App>, headers: HeaderMap, Form(action): Form<Action>) -> Response {
    let mut rt = app.runtime.lock().unwrap();
    match authorize(&rt, &headers, &action) {
        Ok(id) => {
            if rt.sessions[&id].job.running && !rt.sessions[&id].job.publishing {
                end_history_job(&rt, &rt.sessions[&id], "cancelled", None);
            }
            let s = rt.sessions.get_mut(&id).unwrap();
            if s.job.running && s.job.publishing {
                return error_response(message(
                    "Spotify save is in flight. Wait for its receipt before another operation.",
                ));
            }
            s.job.running = false;
            if let Some(abort) = s.job.abort.take() {
                abort.abort();
            }
            s.job.text = "Paused. Completed checkpoints retained.".into();
        }
        Err(e) => return error_response(e),
    }
    Redirect::to("/").into_response()
}

async fn disconnect(
    State(app): State<App>,
    headers: HeaderMap,
    Form(action): Form<Action>,
) -> Response {
    end_connection(app, headers, action, false)
}

async fn delete_data(
    State(app): State<App>,
    headers: HeaderMap,
    Form(action): Form<Action>,
) -> Response {
    if action.consent.as_deref() != Some("yes") {
        return error_response(message("Confirm deletion of this account's local data."));
    }
    end_connection(app, headers, action, true)
}

fn end_connection(app: App, headers: HeaderMap, action: Action, delete: bool) -> Response {
    let mut rt = app.runtime.lock().unwrap();
    let id = match authorize(&rt, &headers, &action) {
        Ok(id) => id,
        Err(e) => return error_response(e),
    };
    let account = rt.sessions[&id]
        .account
        .as_ref()
        .map(|a| a.profile.account().to_owned());
    if let Some(account) = account {
        if rt.sessions.values().any(|s| {
            s.job.running
                && s.job.publishing
                && s.account
                    .as_ref()
                    .is_some_and(|a| a.profile.account() == account)
        }) {
            return error_response(message(
                "Spotify save is in flight. Wait for its receipt before disconnecting.",
            ));
        }
        if delete && let Err(e) = rt.store.disconnect(&account) {
            return error_response(e);
        }
        if !delete {
            for session in rt.sessions.values().filter(|s| {
                s.job.running
                    && s.account
                        .as_ref()
                        .is_some_and(|a| a.profile.account() == account)
            }) {
                end_history_job(&rt, session, "cancelled", None);
            }
        }
        for session in rt.sessions.values_mut() {
            if session
                .account
                .as_ref()
                .is_some_and(|a| a.profile.account() == account)
            {
                if let Some(abort) = session.job.abort.take() {
                    abort.abort();
                }
                session.account = None;
                session.build = None;
                session.pending = None;
                session.job = Job::default();
                session.csrf = provider::random_secret();
                session.notice = Some(
                    if delete {
                        "Disconnected. Local account data deleted; Spotify playlists are unchanged."
                    } else {
                        "Signed out. Your library is retained; previews expire after 24 hours."
                    }
                    .into(),
                );
            }
        }
    }
    let s = rt.sessions.get_mut(&id).unwrap();
    s.pending = None;
    s.csrf = provider::random_secret();
    Redirect::to("/").into_response()
}

fn hidden(csrf: &str) -> Markup {
    html! {input type="hidden" name="csrf" value=(csrf);}
}

#[derive(Deserialize)]
struct PreviewAction {
    csrf: String,
    preview_id: String,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    command: String,
    #[serde(default)]
    confirm: Option<String>,
}

fn basic_action(csrf: String) -> Action {
    Action {
        csrf,
        restart: false,
        consent: None,
    }
}

#[derive(Clone, Default, Deserialize, serde::Serialize)]
struct BuildForm {
    #[serde(default, skip_serializing)]
    csrf: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    title_match: String,
    #[serde(default)]
    release_year: String,
    #[serde(default)]
    saved_year: String,
    #[serde(default)]
    bpm_min: String,
    #[serde(default)]
    bpm_max: String,
    #[serde(default)]
    energy_min: String,
    #[serde(default)]
    limit: String,
    #[serde(default)]
    refinement: Option<crate::refinement::Refinement>,
}

#[derive(Deserialize)]
struct RefineForm {
    csrf: String,
    preview_id: String,
    prompt: String,
}

async fn refine(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<RefineForm>,
) -> Response {
    if form.prompt.trim().is_empty() || form.prompt.len() > 2000 {
        return error_response(message("Enter a refinement of up to 2,000 bytes."));
    }
    let build = BuildForm {
        csrf: form.csrf.clone(),
        ..Default::default()
    };
    start_build(app, headers, build, Some(form)).await
}

fn optional_number<T: std::str::FromStr>(value: &str) -> Result<Option<T>> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        value
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| message("Invalid numeric filter."))
    }
}

fn apply_controls(
    mut criteria: crate::selection::Criteria,
    form: &BuildForm,
) -> Result<crate::selection::Criteria> {
    use crate::selection::{Feature, Range};
    // Form fields are explicit user overrides of the interpreted request.
    if !form.source.is_empty() {
        criteria.source_ids = vec![form.source.clone()];
    }
    if !form.artist.trim().is_empty() {
        criteria.artist_contains = Some(form.artist.trim().into());
    }
    if !form.title_match.trim().is_empty() {
        criteria.title_contains = Some(form.title_match.trim().into());
    }
    if let Some(year) = optional_number::<u16>(&form.release_year)? {
        criteria.release_year = Some(Range {
            min: Some(year),
            max: Some(year),
        });
    }
    if let Some(year) = optional_number::<u16>(&form.saved_year)? {
        criteria.saved_year = Some(Range {
            min: Some(year),
            max: Some(year),
        });
    }
    let min = optional_number::<f64>(&form.bpm_min)?;
    let max = optional_number::<f64>(&form.bpm_max)?;
    if min.is_some() || max.is_some() {
        criteria.features.insert(Feature::Tempo, Range { min, max });
    }
    if let Some(min) = optional_number::<f64>(&form.energy_min)? {
        criteria.energy_preset = None;
        criteria.features.insert(
            Feature::Energy,
            Range {
                min: Some(min),
                max: None,
            },
        );
    }
    if let Some(limit) = optional_number::<usize>(&form.limit)? {
        criteria.limit = limit;
    }
    criteria.validate()?;
    Ok(criteria)
}

async fn build(
    State(app): State<App>,
    headers: HeaderMap,
    Form(mut form): Form<BuildForm>,
) -> Response {
    // Conversational state is resolved from this account's stored preview only.
    form.refinement = None;
    start_build(app, headers, form, None).await
}

async fn start_build(
    app: App,
    headers: HeaderMap,
    mut form: BuildForm,
    refine: Option<RefineForm>,
) -> Response {
    if form.prompt.len() > 2000 {
        return error_response(message("Request is too long (maximum 2,000 bytes)."));
    }
    let setup = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let action = basic_action(form.csrf.clone());
        let id = authorize(&rt, &headers, &action)?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        let previous = rt
            .store
            .get::<Preview>(account.profile.account(), "preview")?;
        if previous
            .as_ref()
            .is_some_and(|p| matches!(p.phase, Phase::Creating | Phase::Created))
        {
            return Err(message(
                "A previous Spotify save is unresolved. Finish that save before replacing the preview.",
            ));
        }
        if let Some(refine) = &refine {
            let previous = previous
                .as_ref()
                .filter(|p| p.id == refine.preview_id)
                .ok_or_else(|| message("This selection changed. Refine the latest preview."))?;
            let run = previous
                .run_id
                .as_deref()
                .map(|run| history::get(&rt.store.db, account.profile.account(), run))
                .transpose()?
                .flatten()
                .ok_or_else(|| message("Build a new preview to begin a conversation."))?;
            let mut saved: BuildForm = serde_json::from_value(run.controls)?;
            let prior = run
                .interpretation
                .as_ref()
                .and_then(|v| v.get("preferences"))
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_else(|| {
                    crate::preferences::fallback(
                        &saved.prompt,
                        crate::intent::parse_local(&saved.prompt).ok().as_ref(),
                    )
                });
            let mut recent = saved
                .refinement
                .take()
                .map(|r| r.recent_messages)
                .unwrap_or_default();
            recent.push(refine.prompt.trim().to_owned());
            if recent.len() > 6 {
                recent.drain(..recent.len() - 6);
            }
            saved.refinement = Some(crate::refinement::Refinement {
                parent_run: run.id,
                prior,
                message: refine.prompt.trim().into(),
                recent_messages: recent,
            });
            saved.csrf = form.csrf.clone();
            form = saved;
        }
        let snapshot: Snapshot = rt
            .store
            .get(account.profile.account(), "snapshot")?
            .ok_or_else(|| message("Sync your library first."))?;
        if snapshot.completed_at.is_none() || snapshot.tracks.is_empty() {
            return Err(message("Complete a nonempty library sync first."));
        }
        if refine.is_some() {
            previous.as_ref().unwrap().validate(&snapshot)?;
        }
        let (id, job, account) =
            begin_job(&mut rt, &headers, &action, "Building playlist preview")?;
        let run = history::Run {
            schema_version: 1,
            id: job.clone(),
            created_at: now(),
            updated_at: now(),
            status: "running".into(),
            prompt: form
                .refinement
                .as_ref()
                .map(|r| r.message.clone())
                .unwrap_or_else(|| form.prompt.clone()),
            controls: serde_json::to_value(&form)?,
            library_count: snapshot.tracks.len(),
            snapshot_at: snapshot.completed_at,
            interpretation: None,
            initial_result: None,
            latest_result: None,
            elapsed_ms: None,
            error: None,
        };
        if let Err(e) = history::put(&rt.store.db, account.profile.account(), &run) {
            rt.sessions.get_mut(&id).unwrap().job.running = false;
            return Err(e);
        }
        rt.sessions.get_mut(&id).unwrap().build = Some(form.clone());
        Ok::<_, crate::Error>((id, job, account, snapshot, previous))
    })();
    let (id, job, account, snapshot, previous) = match setup {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    spawn_job(app.clone(), id.clone(), job.clone(), async move {
        let result=async {
            build_progress(&app,&id,&job,"Interpreting request".into())?;
            if let Some(refinement)=&form.refinement
                && let Some(count)=refinement.count_only()
                && form.limit.trim().is_empty()
                && let Some(previous)=&previous
                && count<=previous.track_ids.len() {
                let mut plan=refinement.prior.clone();plan.limit=count;
                record_interpretation(&app,&account,&job,serde_json::json!({"mode":"refinement","engine":"local-count-v1","preferences":plan}))?;
                let mut criteria:Vec<_>=previous.criteria.iter().filter(|s|!s.starts_with("Up to ")).cloned().collect();
                criteria.insert(0,format!("Up to {count} tracks; previous selection order retained"));
                let mut preview=Preview::new(previous.name.clone(),previous.track_ids.iter().take(count).cloned().collect(),criteria,vec!["Reduced the existing selection without another provider lookup. Previous matching limitations still apply.".into()],&snapshot)?;
                preview.run_id=Some(job.clone());
                return save_preview(&app,&id,&job,&account,&preview);
            }
            if form.refinement.is_some() {return build_soft_preview(&app,&id,&job,&account,&snapshot,&form,None).await;}
            let criteria=if form.prompt.trim().is_empty() {crate::selection::Criteria::default()}
            else {match crate::intent::parse_local(&form.prompt) {
                Ok(criteria) => criteria,
                Err(_) => return build_soft_preview(&app,&id,&job,&account,&snapshot,&form,None).await,
            }};
            let criteria=apply_controls(criteria,&form)?;
            record_interpretation(&app,&account,&job,serde_json::json!({"mode":"exact","criteria":criteria,"engine":"local-v1"}))?;
            let mut warnings=vec![];
            let mut evidence:Coverage=app.runtime.lock().unwrap().store.get(account.profile.account(),"evidence")?.unwrap_or_default();
            evidence.tracks.retain(|id,_|snapshot.tracks.contains_key(id));
            for e in evidence.tracks.values_mut() {e.tags.clear();e.artist_tags.clear();if e.features_expires_at<=now() {e.features.clear();}}
            let mut metadata=criteria.clone();
            metadata.features.clear();
            metadata.energy_preset=None;
            metadata.genre_groups.clear();
            let ids=crate::selection::all_candidate_ids(&snapshot,&Coverage::default(),&metadata)?;
            let started=Instant::now();
            if criteria.genre_needed() {
                let lookup=genre_evidence(&app,&id,&job,&account,&snapshot,&ids,&criteria,&mut evidence).await;
                let (cached,fetched)=match lookup {Ok(counts)=>counts,Err(_)=>return build_soft_preview(&app,&id,&job,&account,&snapshot,&form,Some(&criteria)).await};
                warnings.push(format!("Genre considered across all {} eligible library tracks before energy; {cached} cached lookups, {fetched} fresh lookups. No candidate cap.",ids.len()));
                if criteria.genre_match_scope==crate::selection::GenreMatchScope::TrackOrArtist {
                    warnings.push("Fast genre matching uses primary-artist tags and any fresh cached track tags. Artist tags are broad priors, not verified track genre; tracks without a matching prior can be missed.".into());
                }
            }
            let mut before_numeric=criteria.clone();
            before_numeric.features.clear();
            before_numeric.energy_preset=None;
            let numeric_ids=crate::selection::all_candidate_ids(&snapshot,&evidence,&before_numeric)?;
            if criteria.is_numeric() {
                let pending:Vec<_>=numeric_ids.iter().filter(|id|!evidence.tracks.get(*id).is_some_and(|e|e.recco_checked && e.features_expires_at>now())).cloned().collect();
                warnings.push(format!("Numeric check narrowed to {} genre/metadata matches from {} tracks; {} reusable numeric records.",numeric_ids.len(),ids.len(),numeric_ids.len()-pending.len()));
                for (index,batch) in pending.chunks(40).enumerate() {
                    counted_progress(&app,&id,&job,"Finding the right energy",index*40,pending.len())?;
                    let response=match app.enrichment.features_with_policy(batch).await {Ok(response)=>response,Err(_)=>return build_soft_preview(&app,&id,&job,&account,&snapshot,&form,Some(&criteria)).await};
                    let mut values=response.features;
                    for track in batch {
                        let e=evidence.tracks.entry(track.clone()).or_default();
                        e.features=values.remove(track).unwrap_or_default();
                        e.feature_count=e.features.len();e.recco_checked=true;e.updated_at=now();
                        e.features_expires_at=if response.retention_allowed {response.expires_at} else {0};
                    }
                    save_build_evidence(&app,&id,&job,&account,&evidence)?;
                }
            }
            let selection=crate::selection::select(&snapshot,&evidence,&criteria)?;
            if selection.tracks.is_empty() && !form.prompt.trim().is_empty() {
                return build_soft_preview(&app,&id,&job,&account,&snapshot,&form,Some(&criteria)).await;
            }
            let artist_matches=selection.tracks.iter().filter(|t|t.reasons.iter().any(|r|r.starts_with("Last.fm artist-level"))).count();
            if artist_matches>0 {warnings.push(format!("{artist_matches} selected tracks use artist-level genre priors, not verified track-level genre tags."));}
            if selection.excluded_unknown>0 {warnings.push(format!("{} tracks lack required evidence.",selection.excluded_unknown));}
            warnings.push(format!("{} matches before the {}-track limit.",selection.total_matches,criteria.limit));
            warnings.push(format!("Evidence lookup completed in {:.1}s.",started.elapsed().as_secs_f64()));
            let name=if form.name.trim().is_empty() {"Promptlist selection".into()}else{form.name.trim().into()};
            let mut preview=Preview::new(name,selection.tracks.into_iter().map(|t|t.id).collect(),criteria.summary(),warnings,&snapshot)?;
            preview.run_id=Some(job.clone());
            save_preview(&app,&id,&job,&account,&preview)
        }.await;
        finish(&app, &id, &job, result, "Preview ready for review");
    });
    Redirect::to("/").into_response()
}

fn record_interpretation(
    app: &App,
    account: &Account,
    job: &str,
    value: serde_json::Value,
) -> Result<()> {
    let rt = app.runtime.lock().unwrap();
    if let Some(mut run) = history::get(&rt.store.db, account.profile.account(), job)? {
        run.interpretation = Some(value);
        run.updated_at = now();
        history::put(&rt.store.db, account.profile.account(), &run)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn build_soft_preview(
    app: &App,
    id: &str,
    job: &str,
    account: &Account,
    snapshot: &Snapshot,
    form: &BuildForm,
    exact: Option<&crate::selection::Criteria>,
) -> Result<()> {
    let mut warnings=vec!["Best-effort selection: subjective preferences are ranked, not strict requirements. Mood, lyrics and dedication suitability are not verified.".into()];
    let mut engine = "local-preferences-v1";
    let local_plan = || {
        form.refinement
            .as_ref()
            .map(|r| r.fallback())
            .unwrap_or_else(|| crate::preferences::fallback(&form.prompt, None))
    };
    let prompt = form
        .refinement
        .as_ref()
        .map(|r| r.model_input(&form.prompt))
        .unwrap_or_else(|| form.prompt.clone());
    let plan = if exact.is_none() {
        if let Some(key) = app.config.openai_key.as_deref() {
            build_progress(app, id, job, "Interpreting musical preferences".into())?;
            match crate::preferences::interpret_with_model(
                &app.http,
                key,
                &prompt,
                &app.config.openai_model,
            )
            .await
            {
                Ok(plan) => {
                    engine = &app.config.openai_model;
                    plan
                }
                Err(_) => {
                    warnings.push(
                        "AI interpretation was unavailable; used local preference matching.".into(),
                    );
                    local_plan()
                }
            }
        } else {
            warnings.push("Used local preference matching; AI is not configured.".into());
            local_plan()
        }
    } else {
        warnings.push("Exact prompt matching returned no result or evidence was unavailable. Prompt constraints were relaxed to preferences; advanced filters are unchanged.".into());
        crate::preferences::fallback(&form.prompt, exact)
    };
    let mut base = if form.refinement.is_some() {
        crate::intent::parse_local(&form.prompt).unwrap_or_default()
    } else {
        crate::selection::Criteria::default()
    };
    base.features.clear();
    base.genre_groups.clear();
    base.energy_preset = None;
    base.limit = plan.limit;
    let controls = apply_controls(base, form)?;
    record_interpretation(
        app,
        account,
        job,
        serde_json::json!({"mode":"best_effort","engine":engine,"preferences":plan,"advanced_filters":controls}),
    )?;
    let mut metadata = controls.clone();
    metadata.features.clear();
    metadata.energy_preset = None;
    let ids = crate::selection::all_candidate_ids(snapshot, &Coverage::default(), &metadata)?;
    let mut evidence: Coverage = app
        .runtime
        .lock()
        .unwrap()
        .store
        .get(account.profile.account(), "evidence")?
        .unwrap_or_default();
    evidence
        .tracks
        .retain(|id, _| snapshot.tracks.contains_key(id));
    for e in evidence.tracks.values_mut() {
        e.tags.clear();
        e.artist_tags.clear();
        if e.features_expires_at <= now() {
            e.features.clear();
        }
    }
    if !plan.tag_groups.is_empty() && app.enrichment.has_lastfm() {
        let lookup = tokio::time::timeout(
            Duration::from_secs(12),
            genre_evidence(
                app,
                id,
                job,
                account,
                snapshot,
                &ids,
                &metadata,
                &mut evidence,
            ),
        )
        .await;
        if !matches!(lookup, Ok(Ok(_))) {
            warnings.push("Genre lookup was partial or unavailable. Used completed cached evidence; uncached tracks remain eligible.".into());
            let rt = app.runtime.lock().unwrap();
            let cache = rt.store.tags(account.profile.account())?;
            for track_id in &ids {
                let track = &snapshot.tracks[track_id];
                if let Some(artist) = track.artists.first() {
                    let e = evidence.tracks.entry(track_id.clone()).or_default();
                    if let Some(tags) = cache.get(&crate::model::tag_key(artist, None)) {
                        e.artist_tags = tags.tags.clone();
                    }
                    if let Some(tags) = cache.get(&crate::model::tag_key(artist, Some(&track.name)))
                    {
                        e.tags = tags.tags.clone();
                    }
                }
            }
        }
    }
    if plan.energy_target.is_some() || controls.is_numeric() {
        build_progress(
            app,
            id,
            job,
            "Ranking candidates with available energy evidence".into(),
        )?;
        let ordered =
            crate::preferences::rank(snapshot, &evidence, &ids, &plan, ids.len(), &form.prompt);
        let pending: Vec<_> = ordered
            .into_iter()
            .filter(|id| {
                !evidence
                    .tracks
                    .get(id)
                    .is_some_and(|e| e.recco_checked && e.features_expires_at > now())
            })
            .collect();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        for batch in pending.chunks(40) {
            let fetched =
                tokio::time::timeout_at(deadline, app.enrichment.features_with_policy(batch)).await;
            let response = match fetched {
                Ok(Ok(response)) => response,
                _ => {
                    warnings.push("Numeric lookup was partial or unavailable. Unknown energy is not treated as low energy; selection uses the evidence available.".into());
                    break;
                }
            };
            let mut values = response.features;
            for track in batch {
                let e = evidence.tracks.entry(track.clone()).or_default();
                e.features = values.remove(track).unwrap_or_default();
                e.feature_count = e.features.len();
                e.recco_checked = true;
                e.updated_at = now();
                e.features_expires_at = if response.retention_allowed {
                    response.expires_at
                } else {
                    0
                };
            }
            save_build_evidence(app, id, job, account, &evidence)?;
        }
    }
    let eligible = crate::selection::all_candidate_ids(snapshot, &evidence, &controls)?;
    let selected = crate::preferences::rank(
        snapshot,
        &evidence,
        &eligible,
        &plan,
        controls.limit,
        &form.prompt,
    );
    warnings.extend(plan.caveats.clone());
    warnings.push(format!("Ranked {} eligible tracks across the library. Artist tags are weaker than recording tags; title words are weak thematic clues. Unscored tracks may fill remaining places.",eligible.len()));
    if selected.is_empty() {
        warnings.push("No library tracks satisfy the advanced filters. Clear those filters to broaden the result.".into());
    }
    let mut summary = vec![plan.description.clone()];
    if !plan.tag_groups.is_empty() {
        summary.push(format!(
            "Soft tag preferences: {}",
            plan.tag_groups
                .iter()
                .map(|g| g.join(" / "))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(target) = plan.energy_target {
        summary.push(format!(
            "Approximate energy preference: {target:.2} (ranking target, not a threshold)"
        ));
    }
    summary.extend(controls.summary());
    let name = if form.name.trim().is_empty() {
        "Promptlist selection".into()
    } else {
        form.name.trim().into()
    };
    let mut preview = Preview::new(name, selected, summary, warnings, snapshot)?;
    preview.run_id = Some(job.into());
    save_preview(app, id, job, account, &preview)
}

fn build_progress(app: &App, id: &str, job: &str, text: String) -> Result<()> {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return Err(message("Build cancelled."));
    }
    let job = &mut rt.sessions.get_mut(id).unwrap().job;
    job.text = text;
    job.completed = None;
    job.total = None;
    Ok(())
}

fn counted_progress(
    app: &App,
    id: &str,
    job: &str,
    label: &str,
    completed: usize,
    total: usize,
) -> Result<()> {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return Err(message("Build cancelled."));
    }
    let job = &mut rt.sessions.get_mut(id).unwrap().job;
    job.text = label.into();
    job.completed = Some(completed.min(total));
    job.total = Some(total);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn genre_evidence(
    app: &App,
    id: &str,
    job: &str,
    account: &Account,
    snapshot: &Snapshot,
    ids: &[String],
    criteria: &crate::selection::Criteria,
    evidence: &mut Coverage,
) -> Result<(usize, usize)> {
    use crate::model::{CachedTags, tag_key};
    use std::collections::BTreeMap;
    let mut cache = app
        .runtime
        .lock()
        .unwrap()
        .store
        .tags(account.profile.account())?;
    let track_only = criteria.genre_match_scope == crate::selection::GenreMatchScope::TrackOnly;
    let mut keys = BTreeMap::new();
    for track_id in ids {
        let track = &snapshot.tracks[track_id];
        if let Some(artist) = track.artists.first() {
            let title = track_only.then(|| track.name.clone());
            keys.entry(tag_key(artist, title.as_deref()))
                .or_insert((artist.clone(), title));
        }
    }
    let total = keys.len();
    let mut pending = keys
        .into_iter()
        .filter(|(key, _)| !cache.contains_key(key))
        .collect::<Vec<_>>()
        .into_iter();
    let cached = total - pending.len();
    let mut jobs = tokio::task::JoinSet::new();
    let mut fetched = 0;
    loop {
        while jobs.len() < 4 {
            let Some((key, (artist, title))) = pending.next() else {
                break;
            };
            counted_progress(
                app,
                id,
                job,
                if track_only {
                    "Exploring your tracks"
                } else {
                    "Finding your sound"
                },
                cached + fetched,
                total,
            )?;
            let api = app.enrichment.clone();
            jobs.spawn(async move {
                let result = if let Some(title) = title {
                    api.tags(&artist, &title)
                        .await
                        .map(|(tags, _, expires)| (tags, expires))
                } else {
                    api.artist_tags_with_policy(&artist).await
                };
                (key, result)
            });
        }
        let Some(result) = jobs.join_next().await else {
            break;
        };
        let (key, result) = result.map_err(|_| {
            message("Genre lookup was interrupted. Retry to resume cached progress.")
        })?;
        let (tags, expires_at) = result?;
        let value = CachedTags {
            tags,
            expires_at,
            checked_at: now(),
        };
        let mut rt = app.runtime.lock().unwrap();
        if !active(&rt, id, job) {
            return Err(message("Build cancelled."));
        }
        rt.store.put_tags(account.profile.account(), &key, &value)?;
        // Even no-store responses remain usable within this build, never another.
        cache.insert(key, value);
        fetched += 1;
    }
    for track_id in ids {
        let track = &snapshot.tracks[track_id];
        let Some(artist) = track.artists.first() else {
            continue;
        };
        let e = evidence.tracks.entry(track_id.clone()).or_default();
        if let Some(value) = cache.get(&tag_key(artist, Some(&track.name))) {
            e.tags = value.tags.clone();
        }
        if !track_only && let Some(value) = cache.get(&tag_key(artist, None)) {
            e.artist_tags = value.tags.clone();
        }
    }
    Ok((total - fetched, fetched))
}

fn save_build_evidence(
    app: &App,
    id: &str,
    job: &str,
    account: &Account,
    evidence: &Coverage,
) -> Result<()> {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return Err(message("Build cancelled."));
    }
    let mut saved: Coverage = rt
        .store
        .get(account.profile.account(), "evidence")?
        .unwrap_or_default();
    for (id, value) in &evidence.tracks {
        if !value.recco_checked {
            continue;
        }
        let mut value = value.clone();
        if value.features_expires_at <= now() {
            value.features.clear();
        }
        value.tags.clear();
        value.artist_tags.clear();
        saved.tracks.insert(id.clone(), value);
    }
    rt.store.put(account.profile.account(), "evidence", &saved)
}

async fn edit_preview(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<PreviewAction>,
) -> Response {
    let result = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &basic_action(form.csrf))?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        if rt.sessions.values().any(|s| {
            s.job.running
                && s.account
                    .as_ref()
                    .is_some_and(|a| a.profile.account() == account.profile.account())
        }) {
            return Err(message("Wait for the current operation."));
        }
        let mut preview: Preview = rt
            .store
            .get(account.profile.account(), "preview")?
            .ok_or_else(|| message("Build a preview first."))?;
        if preview.id != form.preview_id {
            return Err(message("Preview changed. Reload before editing."));
        }
        preview.edit(form.index, &form.command)?;
        rt.store
            .put(account.profile.account(), "preview", &preview)?;
        record_result(&rt, account.profile.account(), &preview)?;
        audit_event(
            &rt,
            account.profile.account(),
            "preview_edited",
            preview.run_id.as_deref(),
            serde_json::json!({"command":form.command,"index":form.index,"remaining":preview.track_ids.len()}),
        );
        Ok::<_, crate::Error>(())
    })();
    match result {
        Ok(()) => Redirect::to("/").into_response(),
        Err(e) => error_response(e),
    }
}

async fn publish(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<PreviewAction>,
) -> Response {
    let setup = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let action = basic_action(form.csrf);
        let id = authorize(&rt, &headers, &action)?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        let mut preview: Preview = rt
            .store
            .get(account.profile.account(), "preview")?
            .ok_or_else(|| message("Build a preview first."))?;
        if preview.id != form.preview_id {
            return Err(message("Preview changed. Review it again before saving."));
        }
        let snapshot: Snapshot = rt
            .store
            .get(account.profile.account(), "snapshot")?
            .ok_or_else(|| message("Sync library first."))?;
        preview.begin_save(&snapshot)?;
        let (id, job, account) = begin_job(&mut rt, &headers, &action, "Saving private playlist")?;
        rt.sessions.get_mut(&id).unwrap().job.publishing = true;
        if let Err(e) = rt.store.put(account.profile.account(), "preview", &preview) {
            rt.sessions.get_mut(&id).unwrap().job.running = false;
            return Err(e);
        }
        // Analytics must not strand a prepared Spotify write before its worker starts.
        if record_result(&rt, account.profile.account(), &preview).is_err() {
            eprintln!("Promptlist: export history checkpoint could not be stored.");
        }
        audit_event(
            &rt,
            account.profile.account(),
            "export_started",
            preview.run_id.as_deref(),
            serde_json::json!({"tracks":preview.track_ids.len()}),
        );
        Ok::<_, crate::Error>((id, job, account, preview))
    })();
    let (id, job, account, mut preview) = match setup {
        Ok(v) => v,
        Err(e) => return error_response(e),
    };
    account.api.acknowledge_quota();
    tokio::spawn(async move {
        let result = async {
            if preview.phase == Phase::Saved {
                return Ok(());
            }
            if preview.phase == Phase::Creating {
                match account.api.create_private_playlist(&preview.name).await {
                    Ok(playlist_id) => {
                        preview.playlist_id = Some(playlist_id);
                        preview.phase = Phase::Created;
                    }
                    Err(e) => {
                        if matches!(
                            e,
                            crate::Error::Http {
                                status: 400 | 401 | 403 | 422,
                                ..
                            } | crate::Error::RateLimit { .. }
                                | crate::Error::Quota
                        ) {
                            preview.phase = Phase::Draft;
                            save_preview(&app, &id, &job, &account, &preview)?;
                        }
                        return Err(e);
                    }
                }
                save_preview(&app, &id, &job, &account, &preview)?;
            }
            if !active(&app.runtime.lock().unwrap(), &id, &job) {
                return Err(message("Save cancelled."));
            }
            let playlist_id = preview
                .playlist_id
                .as_deref()
                .ok_or_else(|| message("Missing save receipt. Check Spotify before retrying."))?;
            account
                .api
                .fill_private_playlist(playlist_id, &preview.track_ids)
                .await?;
            account
                .api
                .verify_playlist_items(playlist_id, &preview.track_ids)
                .await?;
            preview.phase = Phase::Saved;
            save_preview(&app, &id, &job, &account, &preview)
        }
        .await;
        finish(
            &app,
            &id,
            &job,
            result,
            "Private playlist saved and verified",
        );
    });
    Redirect::to("/").into_response()
}

async fn abandon_preview(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<PreviewAction>,
) -> Response {
    let result = (|| {
        if form.confirm.as_deref() != Some("yes") {
            return Err(message(
                "Confirm that you checked Spotify before abandoning recovery.",
            ));
        }
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &basic_action(form.csrf))?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        if rt.sessions.values().any(|s| {
            s.job.running
                && s.account
                    .as_ref()
                    .is_some_and(|a| a.profile.account() == account.profile.account())
        }) {
            return Err(message("Wait for the current operation."));
        }
        let preview: Preview = rt
            .store
            .get(account.profile.account(), "preview")?
            .ok_or_else(|| message("No preview to abandon."))?;
        if preview.id != form.preview_id
            || !matches!(preview.phase, Phase::Creating | Phase::Created)
        {
            return Err(message("Recovery state changed. Reload the page."));
        }
        rt.store.put(
            account.profile.account(),
            &format!("save-history:{}", preview.id),
            &preview,
        )?;
        rt.store.remove(account.profile.account(), "preview")?;
        if let Some(run_id) = &preview.run_id
            && let Some(mut run) = history::get(&rt.store.db, account.profile.account(), run_id)?
        {
            run.status = "recovery_abandoned".into();
            run.updated_at = now();
            history::put(&rt.store.db, account.profile.account(), &run)?;
        }
        rt.sessions.get_mut(&id).unwrap().notice=Some("Pending save abandoned locally. Any playlist already created in Spotify was left unchanged.".into());
        Ok::<_, crate::Error>(())
    })();
    match result {
        Ok(()) => Redirect::to("/").into_response(),
        Err(e) => error_response(e),
    }
}

fn save_preview(
    app: &App,
    id: &str,
    job: &str,
    account: &Account,
    preview: &Preview,
) -> Result<()> {
    let mut rt = app.runtime.lock().unwrap();
    if !active(&rt, id, job) {
        return Err(message("Operation cancelled."));
    }
    rt.store
        .put(account.profile.account(), "preview", preview)?;
    record_result(&rt, account.profile.account(), preview)
}

fn record_result(rt: &Runtime, account: &str, preview: &Preview) -> Result<()> {
    if let Some(run_id) = &preview.run_id
        && let Some(mut run) = history::get(&rt.store.db, account, run_id)?
    {
        let snapshot = rt.store.get(account, "snapshot")?.unwrap_or_default();
        let result = history::SavedResult::capture(preview, &snapshot);
        if run.initial_result.is_none() {
            run.initial_result = Some(result.clone());
        }
        let was_exported = run
            .latest_result
            .as_ref()
            .is_some_and(|r| r.phase == Phase::Saved);
        run.latest_result = Some(result);
        run.updated_at = now();
        if preview.phase == Phase::Saved && !was_exported {
            run.status = "exported".into();
            audit_event(
                rt,
                account,
                "export_completed",
                Some(run_id),
                serde_json::json!({"tracks":preview.track_ids.len()}),
            );
        }
        history::put(&rt.store.db, account, &run)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct HistoryAction {
    csrf: String,
    run_id: String,
}
async fn reuse_prompt(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<HistoryAction>,
) -> Response {
    let result = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &basic_action(form.csrf))?;
        let account = rt.sessions[&id]
            .account
            .clone()
            .ok_or_else(|| message("Connect Spotify first."))?;
        if rt.sessions[&id].job.running {
            return Err(message("Wait for the current operation."));
        }
        let run = history::get(&rt.store.db, account.profile.account(), &form.run_id)?
            .ok_or_else(|| message("History entry not found."))?;
        let mut build: BuildForm = serde_json::from_value(run.controls)?;
        build.refinement = None;
        build.csrf = rt.sessions[&id].csrf.clone();
        rt.sessions.get_mut(&id).unwrap().build = Some(build);
        audit_event(
            &rt,
            account.profile.account(),
            "history_reused",
            Some(&run.id),
            serde_json::json!({}),
        );
        Ok::<_, crate::Error>(())
    })();
    match result {
        Ok(()) => Redirect::to("/").into_response(),
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use tower::ServiceExt;

    async fn seeded_app() -> (
        App,
        tempfile::TempDir,
        tokio::task::JoinHandle<()>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use axum::Json;
        use serde_json::json;
        let created = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = created.clone();
        let items = Arc::new(Mutex::new(Vec::<String>::new()));
        let read_items = items.clone();
        let mock=Router::new()
            .route("/v1/me/player/play",axum::routing::put(||async {StatusCode::NO_CONTENT}))
            .route("/v1/me/playlists",post(move |Json(body):Json<serde_json::Value>| {let c=c.clone();async move {
                assert_eq!(body["public"],false);c.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                (StatusCode::CREATED,Json(json!({"id":"pppppppppppppppppppppp","public":false})))
            }}))
            .route("/v1/playlists/pppppppppppppppppppppp",get(||async {Json(json!({"snapshot_id":"mock"}))}))
            .route("/v1/playlists/pppppppppppppppppppppp/items",get(move ||{let items=read_items.clone();async move {
                let ids=items.lock().unwrap().clone();
                Json(json!({"items":ids.iter().map(|id|json!({"item":{"id":id,"type":"track"}})).collect::<Vec<_>>(),"total":ids.len(),"next":null}))
            }}).put(move |Json(body):Json<serde_json::Value>|{let items=items.clone();async move {
                *items.lock().unwrap()=body["uris"].as_array().unwrap().iter().map(|v|v.as_str().unwrap().trim_start_matches("spotify:track:").to_owned()).collect();
                Json(json!({"snapshot_id":"mock"}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let (mut app, dir) = test_app();
        app.enrichment = Arc::new(Enrichment::fixture(&base));
        let mut snapshot = Snapshot {
            completed_at: Some(now()),
            ..Default::default()
        };
        snapshot.sources.push(crate::model::Source {
            id: "liked".into(),
            name: "Liked Songs".into(),
            accessible: true,
            count: 12,
            ..Default::default()
        });
        snapshot.sources.push(crate::model::Source {
            id: "x".repeat(22),
            name: "Unavailable test playlist".into(),
            accessible: false,
            reason: Some("Spotify denied access (403)".into()),
            ..Default::default()
        });
        let titles = [
            "Open Circuit",
            "Soft Signal",
            "After Hours",
            "Low Frequency",
            "Glass Surface",
            "Parallel Lines",
            "Night Transit",
            "Slow Motion",
            "Static Memory",
            "Blue Room",
            "First Light",
            "Closing Time",
        ];
        for (i, title) in titles.iter().enumerate() {
            let id = format!("{i:022}");
            snapshot.tracks.insert(
                id.clone(),
                crate::model::Track {
                    id,
                    name: (*title).into(),
                    artists: vec![if i % 2 == 0 {
                        "Studio North".into()
                    } else {
                        "Signal Forms".into()
                    }],
                    duration_ms: Some(210000),
                    explicit: Some(false),
                    release_date: Some("2020".into()),
                    release_precision: Some("year".into()),
                    memberships: vec![crate::model::Membership {
                        source: "liked".into(),
                        added_at: Some("2026-01-01T00:00:00Z".into()),
                        position: i,
                    }],
                    ..Default::default()
                },
            );
        }
        let mut rt = app.runtime.lock().unwrap();
        rt.store.promote("fixture", &snapshot).unwrap();
        rt.sessions.insert(
            "fixture".into(),
            Session {
                csrf: "fixture-token".into(),
                auto_sync_pending: false,
                created: now(),
                pending: None,
                account: Some(Arc::new(Account {
                    profile: Profile {
                        id: "fixture".into(),
                        account_id: None,
                        display_name: Some("Local test library".into()),
                    },
                    api: Spotify::fixture(base),
                })),
                job: Job::default(),
                notice: None,
                build: None,
            },
        );
        drop(rt);
        (app, dir, task, created)
    }

    async fn fixture_post(app: &App, path: &str, body: String) -> Response {
        router(app.clone())
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", "127.0.0.1:8787")
                    .header("origin", "http://127.0.0.1:8787")
                    .header("cookie", "promptlist_session=fixture")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn fixture_html(app: &App, path: &str, connected: bool) -> String {
        let mut request = HttpRequest::builder()
            .uri(path)
            .header("host", app.config.host_header());
        if connected {
            request = request.header("cookie", "promptlist_session=fixture");
        }
        let response = router(app.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        String::from_utf8(
            to_bytes(response.into_body(), 5_000_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn connection_gate_and_local_web_diagnostics_are_separate() {
        let (mut app, _dir, server, _) = seeded_app().await;
        let html = fixture_html(&app, "/?tab=library", false).await;
        assert!(html.contains("connect-button"));
        assert!(!html.contains("Library views"));
        assert!(!html.contains("id=\"radio-player\""));
        assert!(!html.contains("id=\"prompt\""));
        let local = fixture_html(&app, "/?tab=library", true).await;
        assert!(local.contains("Metadata &amp; evidence"));
        assert!(local.contains("Local diagnostics"));
        assert!(local.contains("Resync library"));
        assert!(!local.contains("<h1"));
        let web = fixture_html(&app, "/?tab=library&ui=web", true).await;
        assert!(!web.contains("track-details"));
        assert!(!web.contains("href=\"/?tab=coverage\""));
        let sources = fixture_html(&app, "/?tab=sources&excluded=true", true).await;
        assert!(sources.contains("data-view=\"library\""));
        assert!(sources.contains("Unavailable test playlist"));
        assert!(sources.contains("Library tracks"));
        let config = Arc::get_mut(&mut app.config).unwrap();
        config.redirect = url::Url::parse("https://promptlist.example/callback").unwrap();
        config.allowed_users = vec!["fixture".into()];
        let hosted = fixture_html(&app, "/?tab=coverage&ui=local", true).await;
        assert!(hosted.contains("data-view=\"library\""));
        assert!(!hosted.contains("track-details"));
        assert!(!hosted.contains("Display mode"));
        let result = start_coverage(
            State(app),
            HeaderMap::new(),
            Form(CoverageForm {
                csrf: "unused".into(),
                limit: None,
                source: String::new(),
                tags: None,
            }),
        )
        .await;
        assert_eq!(result.status(), StatusCode::NOT_FOUND);
        server.abort();
    }

    #[tokio::test]
    async fn automatic_sync_is_one_shot_and_failure_keeps_saved_library() {
        let (app, _dir, server, _) = seeded_app().await;
        app.runtime
            .lock()
            .unwrap()
            .sessions
            .get_mut("fixture")
            .unwrap()
            .auto_sync_pending = true;
        assert!(
            fixture_html(&app, "/", true)
                .await
                .contains("id=\"auto-sync\"")
        );
        let body = "csrf=fixture-token&consent=automatic".to_owned();
        assert_eq!(
            fixture_post(&app, "/sync", body.clone()).await.status(),
            StatusCode::SEE_OTHER
        );
        let first = app.runtime.lock().unwrap().sessions["fixture"]
            .job
            .id
            .clone();
        assert!(!first.is_empty());
        assert_eq!(
            fixture_post(&app, "/sync", body).await.status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            app.runtime.lock().unwrap().sessions["fixture"].job.id,
            first
        );
        assert!(!app.runtime.lock().unwrap().sessions["fixture"].auto_sync_pending);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            app.runtime
                .lock()
                .unwrap()
                .store
                .get::<Snapshot>("fixture", "snapshot")
                .unwrap()
                .unwrap()
                .tracks
                .len(),
            12
        );
        assert!(
            !fixture_html(&app, "/", true)
                .await
                .contains("id=\"auto-sync\"")
        );
        server.abort();
    }

    #[tokio::test]
    async fn hosted_origin_cookies_and_anonymous_capacity_are_safe() {
        let (mut app, _dir, server, _) = seeded_app().await;
        let config = Arc::get_mut(&mut app.config).unwrap();
        config.redirect = url::Url::parse("https://promptlist.example/callback").unwrap();
        config.allowed_users = vec!["fixture".into()];
        for (host, origin, method, path, expected) in [
            ("promptlist.example", "", "GET", "/", StatusCode::OK),
            ("attacker.example", "", "GET", "/", StatusCode::FORBIDDEN),
            (
                "promptlist.example",
                "https://attacker.example",
                "POST",
                "/api/playback/token",
                StatusCode::FORBIDDEN,
            ),
            (
                "promptlist.example",
                "https://promptlist.example",
                "POST",
                "/api/playback/token",
                StatusCode::OK,
            ),
        ] {
            let response = router(app.clone())
                .oneshot(
                    HttpRequest::builder()
                        .method(method)
                        .uri(path)
                        .header("host", host)
                        .header("origin", origin)
                        .header("x-forwarded-host", "promptlist.example")
                        .header("cookie", "promptlist_session=fixture")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from("csrf=fixture-token"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        for _ in 0..130 {
            let response = router(app.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/")
                        .header("host", "promptlist.example")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers()[header::SET_COOKIE]
                    .to_str()
                    .unwrap()
                    .contains("; Secure")
            );
            assert_eq!(
                response.headers()["strict-transport-security"],
                "max-age=31536000"
            );
        }
        let rt = app.runtime.lock().unwrap();
        assert_eq!(rt.sessions.len(), 128);
        assert!(rt.sessions["fixture"].account.is_some());
        server.abort();
    }
    async fn idle(app: &App) {
        for _ in 0..3000 {
            if !app.runtime.lock().unwrap().sessions["fixture"].job.running {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Mock job did not finish");
    }

    #[tokio::test]
    async fn playback_is_account_preview_and_csrf_bound() {
        let (app, _dir, server, _) = seeded_app().await;
        let response = fixture_post(&app, "/api/playback/token", "csrf=fixture-token".into()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 10000).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["access_token"].is_string());
        assert!(value.get("refresh_token").is_none());
        assert!(
            !fixture_post(&app, "/api/playback/token", "csrf=wrong".into())
                .await
                .status()
                .is_success()
        );
        fixture_post(&app, "/build", "csrf=fixture-token&limit=5".into()).await;
        idle(&app).await;
        let preview: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        let body = format!(
            "csrf=fixture-token&preview_id={}&track_id={}&device_id=browser123",
            preview.id, preview.track_ids[1]
        );
        assert_eq!(
            fixture_post(&app, "/api/playback/play", body.clone())
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        for invalid in [
            body.replace("fixture-token", "wrong"),
            body.replace(&preview.id, "stale"),
            body.replace(&preview.track_ids[1], &"z".repeat(22)),
            body.replace("browser123", "bad%26device"),
        ] {
            assert!(
                !fixture_post(&app, "/api/playback/play", invalid)
                    .await
                    .status()
                    .is_success()
            );
        }
        app.runtime
            .lock()
            .unwrap()
            .store
            .remove("fixture", "preview")
            .unwrap();
        assert!(
            !fixture_post(&app, "/api/playback/play", body)
                .await
                .status()
                .is_success()
        );
        fixture_post(&app, "/logout", "csrf=fixture-token".into()).await;
        assert!(
            !fixture_post(&app, "/api/playback/token", "csrf=fixture-token".into())
                .await
                .status()
                .is_success()
        );
        server.abort();
    }
    #[tokio::test]
    async fn refinement_preserves_context_links_history_and_rejects_stale_or_forged_actions() {
        let (app, _dir, server, created) = seeded_app().await;
        assert_eq!(
            fixture_post(
                &app,
                "/build",
                "csrf=fixture-token&prompt=5+tracks&artist=Studio".into()
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        let original: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        let body = format!(
            "csrf=fixture-token&preview_id={}&prompt=3+tracks",
            original.id
        );
        assert_eq!(
            fixture_post(&app, "/refine", body.replace("fixture-token", "wrong"))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture_post(&app, "/refine", body.clone()).await.status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        let reduced: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        assert_eq!(reduced.track_ids, original.track_ids[..3]);
        assert_ne!(reduced.id, original.id);
        assert_eq!(
            fixture_post(&app, "/refine", body).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture_post(
                &app,
                "/refine",
                format!(
                    "csrf=fixture-token&preview_id={}&prompt=more+energetic",
                    reduced.id
                )
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        {
            let mut rt = app.runtime.lock().unwrap();
            let current: Preview = rt.store.get("fixture", "preview").unwrap().unwrap();
            assert_eq!(current.track_ids.len(), 3);
            let snapshot: Snapshot = rt.store.get("fixture", "snapshot").unwrap().unwrap();
            assert!(
                current
                    .track_ids
                    .iter()
                    .all(|id| snapshot.tracks[id].artists[0].contains("Studio"))
            );
            let run = history::get(&rt.store.db, "fixture", current.run_id.as_ref().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                run.controls["refinement"]["parent_run"],
                reduced.run_id.unwrap()
            );
            assert_eq!(
                run.interpretation.as_ref().unwrap()["preferences"]["limit"],
                3
            );
            assert_eq!(
                run.interpretation.as_ref().unwrap()["preferences"]["energy_target"],
                0.65
            );
            assert_eq!(history::list(&rt.store.db, "fixture", 0).unwrap().len(), 3);
            assert!(
                history::list(&rt.store.db, "other-account", 0)
                    .unwrap()
                    .is_empty()
            );
            let mut foreign = current.clone();
            foreign.id = "other-account-preview".into();
            rt.store.put("other-account", "preview", &foreign).unwrap();
            rt.sessions.get_mut("fixture").unwrap().build = None;
        }
        assert_eq!(
            fixture_post(
                &app,
                "/refine",
                "csrf=fixture-token&preview_id=other-account-preview&prompt=2+tracks".into()
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let response = router(app.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header("host", "127.0.0.1:8787")
                    .header("cookie", "promptlist_session=fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let rt = app.runtime.lock().unwrap();
        assert_eq!(
            rt.sessions["fixture"]
                .build
                .as_ref()
                .unwrap()
                .refinement
                .as_ref()
                .unwrap()
                .recent_messages,
            vec!["3 tracks", "more energetic"]
        );
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn build_edit_confirm_save_and_duplicate_submission() {
        let (app, _dir, task, created) = seeded_app().await;
        assert_eq!(
            fixture_post(
                &app,
                "/build",
                "csrf=fixture-token&limit=5&name=Test&artist=Studio".into()
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        let p: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        assert_eq!(p.track_ids.len(), 5);
        assert_eq!(
            fixture_post(
                &app,
                "/preview/edit",
                format!(
                    "csrf=fixture-token&preview_id={}&command=remove&index=0",
                    p.id
                )
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            fixture_post(
                &app,
                "/publish",
                format!("csrf=fixture-token&preview_id={}&confirm=yes", p.id)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        let p: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        assert_eq!(p.track_ids.len(), 4);
        for _ in 0..2 {
            assert_eq!(
                fixture_post(
                    &app,
                    "/publish",
                    format!("csrf=fixture-token&preview_id={}", p.id)
                )
                .await
                .status(),
                StatusCode::SEE_OTHER
            );
            idle(&app).await;
        }
        let rt = app.runtime.lock().unwrap();
        let saved: Preview = rt.store.get("fixture", "preview").unwrap().unwrap();
        assert_eq!(
            saved.phase,
            Phase::Saved,
            "{}",
            rt.sessions["fixture"].job.text
        );
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 1);
        let run = history::get(&rt.store.db, "fixture", saved.run_id.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(run.status, "exported");
        assert_eq!(run.initial_result.unwrap().tracks.len(), 5);
        assert_eq!(run.latest_result.unwrap().tracks.len(), 4);
        assert_eq!(
            rt.store
                .db
                .query_row(
                    "SELECT count(*) FROM app_events WHERE kind='export_completed'",
                    [],
                    |r| r.get::<_, usize>(0)
                )
                .unwrap(),
            1
        );
        task.abort();
    }

    #[tokio::test]
    async fn numeric_build_is_always_enabled_but_does_not_publish() {
        let (mut app, _dir, task, created) = seeded_app().await;
        let (enrichment, provider, _) = enrichment_mock().await;
        app.enrichment = Arc::new(enrichment);
        fixture_post(&app, "/build", "csrf=fixture-token&bpm_min=110".into()).await;
        idle(&app).await;
        assert!(
            app.runtime.lock().unwrap().sessions["fixture"]
                .job
                .text
                .contains("Preview ready")
        );
        assert!(
            app.runtime
                .lock()
                .unwrap()
                .store
                .get::<Preview>("fixture", "preview")
                .unwrap()
                .is_some()
        );
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 0);
        task.abort();
        provider.abort();
    }

    #[tokio::test]
    async fn subjective_prompt_returns_result_and_private_history_even_without_providers() {
        let (app, _dir, server, created) = seeded_app().await;
        let prompt = "weird kinda energetic music to dedicate to a girl you love";
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", "fixture-token")
            .append_pair("prompt", prompt)
            .finish();
        assert_eq!(
            fixture_post(&app, "/build", body).await.status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        let run_id = {
            let mut rt = app.runtime.lock().unwrap();
            assert!(rt.sessions["fixture"].job.text.contains("Preview ready"));
            let p: Preview = rt.store.get("fixture", "preview").unwrap().unwrap();
            assert_eq!(p.track_ids.len(), 12);
            let run = history::get(&rt.store.db, "fixture", p.run_id.as_ref().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(run.prompt, prompt);
            assert_eq!(run.status, "completed");
            assert_eq!(run.interpretation.as_ref().unwrap()["mode"], "best_effort");
            assert_eq!(run.initial_result.as_ref().unwrap().tracks.len(), 12);
            let exported = history::export(&mut rt.store.db).unwrap().to_string();
            assert!(!exported.contains("fixture-token"));
            assert!(!exported.contains("csrf"));
            assert!(!exported.contains("features_expires_at"));
            assert!(exported.contains("job_finished"));
            assert!(exported.contains("build_submit"));
            run.id
        };
        assert_eq!(
            fixture_post(
                &app,
                "/history/reuse",
                format!("csrf=fixture-token&run_id={run_id}")
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            app.runtime.lock().unwrap().sessions["fixture"]
                .build
                .as_ref()
                .unwrap()
                .prompt,
            prompt
        );
        assert_eq!(
            fixture_post(&app, "/history/reuse", "csrf=wrong&run_id=anything".into())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn history_isolation_views_are_deduplicated_and_rendering_escapes_prompts() {
        let (app, _dir, server, _) = seeded_app().await;
        fixture_post(
            &app,
            "/build",
            "csrf=fixture-token&prompt=%3Cscript%3Ehello%3C%2Fscript%3E".into(),
        )
        .await;
        idle(&app).await;
        for _ in 0..3 {
            let response = router(app.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/?tab=history")
                        .header("host", "127.0.0.1:8787")
                        .header("cookie", "promptlist_session=fixture")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let html = String::from_utf8(
                to_bytes(response.into_body(), 1_000_000)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(html.contains("&lt;script&gt;"));
            assert!(!html.contains("<script>"));
        }
        let id = {
            let rt = app.runtime.lock().unwrap();
            assert_eq!(
                rt.store
                    .db
                    .query_row(
                        "SELECT count(*) FROM app_events WHERE kind='view'",
                        [],
                        |r| r.get::<_, usize>(0)
                    )
                    .unwrap(),
                1
            );
            let run = history::list(&rt.store.db, "fixture", 0).unwrap().remove(0);
            history::ensure_user(&rt.store.db, "another-account").unwrap();
            history::put(
                &rt.store.db,
                "another-account",
                &history::Run {
                    id: "other-run".into(),
                    ..run
                },
            )
            .unwrap();
            "other-run"
        };
        assert_eq!(
            fixture_post(
                &app,
                "/history/reuse",
                format!("csrf=fixture-token&run_id={id}")
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        fixture_post(
            &app,
            "/delete-data",
            "csrf=fixture-token&consent=yes".into(),
        )
        .await;
        let rt = app.runtime.lock().unwrap();
        assert!(
            history::list(&rt.store.db, "fixture", 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            history::list(&rt.store.db, "another-account", 0)
                .unwrap()
                .len(),
            1
        );
        server.abort();
    }

    async fn enrichment_mock() -> (
        Enrichment,
        tokio::task::JoinHandle<()>,
        Arc<Mutex<Vec<usize>>>,
    ) {
        enrichment_mock_with_delay(Duration::ZERO).await
    }

    async fn enrichment_mock_with_delay(
        delay: Duration,
    ) -> (
        Enrichment,
        tokio::task::JoinHandle<()>,
        Arc<Mutex<Vec<usize>>>,
    ) {
        use axum::Json;
        use serde_json::json;
        let calls = Arc::new(Mutex::new(vec![]));
        let counts = calls.clone();
        let tag_counts = calls.clone();
        let mock=Router::new()
            .route("/features",get(move |Query(query):Query<HashMap<String,String>>| {
                let ids:Vec<_>=query["ids"].split(',').map(str::to_owned).collect();
                counts.lock().unwrap().push(ids.len());
                async move { ([(header::CACHE_CONTROL,"no-store")],Json(json!({"content":ids.iter().map(|id|json!({"href":format!("https://open.spotify.com/track/{id}"),"energy":if id.parse::<usize>().unwrap()<3 || id.parse::<usize>().unwrap()==200 {0.9} else {0.4},"tempo":130.0})).collect::<Vec<_>>()}))) }
            }))
            .route("/2.0/",get(move |Query(query):Query<HashMap<String,String>>|{tag_counts.lock().unwrap().push(0);async move {
                tokio::time::sleep(delay).await;
                let artist=&query["artist"];
                let track=query.get("track");
                let name=if track.is_some_and(|t|t=="Open Circuit") || (track.is_none() && artist=="Studio North") {"metalcore"} else if track.is_none() {"house"} else {""};
                let tags=if name.is_empty() {vec![]} else {vec![json!({"name":name,"count":20})]};
                let mut attr=json!({"artist":artist});
                if let Some(track)=track {attr["track"]=json!(track);}
                Json(json!({"toptags":{"@attr":attr,"tag":tags}}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        (Enrichment::fixture(base), server, calls)
    }

    #[tokio::test]
    async fn owner_workout_prompt_builds_without_coverage_and_keeps_provider_values_transient() {
        let (mut app, _dir, spotify, created) = seeded_app().await;
        let (enrichment, provider, calls) = enrichment_mock().await;
        app.enrichment = Arc::new(enrichment);
        app.runtime
            .lock()
            .unwrap()
            .store
            .put(
                "fixture",
                "settings",
                &Settings {
                    music_matching: true,
                },
            )
            .unwrap();
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", "fixture-token")
            .append_pair(
                "prompt",
                "high energy harcore metal to workout to and reach failure",
            )
            .finish();
        assert_eq!(
            fixture_post(&app, "/build", body).await.status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        let rt = app.runtime.lock().unwrap();
        let preview: Preview = rt
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap_or_else(|| panic!("{}", rt.sessions["fixture"].job.text));
        assert_eq!(
            preview.track_ids,
            vec![format!("{:022}", 0), format!("{:022}", 2)]
        );
        assert!(
            preview
                .warnings
                .iter()
                .any(|w| w.contains("2 selected tracks use artist-level"))
        );
        assert!(preview.criteria.iter().any(|s| s.contains("0.7")));
        assert!(
            rt.store
                .get::<Coverage>("fixture", "coverage")
                .unwrap()
                .is_none()
        );
        assert_eq!(*calls.lock().unwrap(), vec![0, 0, 6]);
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 0);
        spotify.abort();
        provider.abort();
    }

    #[tokio::test]
    async fn whole_library_genre_precedes_energy_and_warm_build_skips_tag_calls() {
        let (mut app, _dir, spotify, created) = seeded_app().await;
        let (enrichment, provider, calls) = enrichment_mock().await;
        app.enrichment = Arc::new(enrichment);
        {
            let mut rt = app.runtime.lock().unwrap();
            let mut snapshot: Snapshot = rt.store.get("fixture", "snapshot").unwrap().unwrap();
            let template = snapshot.tracks.values().next().unwrap().clone();
            for i in 12..201 {
                let id = format!("{i:022}");
                let mut track = template.clone();
                track.id = id.clone();
                track.name = "Beyond previous cutoff".into();
                track.artists = vec![
                    if i == 200 {
                        "Studio North"
                    } else {
                        "Signal Forms"
                    }
                    .into(),
                ];
                snapshot.tracks.insert(id, track);
            }
            rt.store.promote("fixture", &snapshot).unwrap();
            rt.store
                .put(
                    "fixture",
                    "settings",
                    &Settings {
                        music_matching: false,
                    },
                )
                .unwrap();
        }
        for expected in [vec![0, 0, 7], vec![0, 0, 7, 7]] {
            assert_eq!(
                fixture_post(
                    &app,
                    "/build",
                    "csrf=fixture-token&prompt=high+energy+metal".into()
                )
                .await
                .status(),
                StatusCode::SEE_OTHER
            );
            idle(&app).await;
            let rt = app.runtime.lock().unwrap();
            let p: Preview = rt.store.get("fixture", "preview").unwrap().unwrap();
            assert!(
                p.track_ids.contains(&format!("{:022}", 200)),
                "last track was capped out"
            );
            assert_eq!(p.track_ids.len(), 3);
            assert!(p.warnings.iter().any(|w| w.contains("all 201 eligible")));
            assert_eq!(*calls.lock().unwrap(), expected);
            assert_eq!(rt.store.tags("fixture").unwrap().len(), 2);
        }
        assert_eq!(created.load(std::sync::atomic::Ordering::SeqCst), 0);
        spotify.abort();
        provider.abort();
    }

    #[tokio::test]
    async fn full_coverage_batches_more_than_200_and_resumes_counts_without_reusing_values() {
        let (mut app, _dir, spotify, _) = seeded_app().await;
        let (enrichment, provider, calls) = enrichment_mock().await;
        app.enrichment = Arc::new(enrichment);
        {
            let mut rt = app.runtime.lock().unwrap();
            let mut snapshot: Snapshot = rt.store.get("fixture", "snapshot").unwrap().unwrap();
            let template = snapshot.tracks.values().next().unwrap().clone();
            for i in 12..201 {
                let id = format!("{i:022}");
                let mut track = template.clone();
                track.id = id.clone();
                snapshot.tracks.insert(id, track);
            }
            rt.store.promote("fixture", &snapshot).unwrap();
            rt.store
                .put(
                    "fixture",
                    "settings",
                    &Settings {
                        music_matching: true,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            fixture_post(&app, "/coverage", "csrf=fixture-token&limit=0".into())
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        {
            let mut rt = app.runtime.lock().unwrap();
            let mut coverage: Coverage = rt.store.get("fixture", "coverage").unwrap().unwrap();
            assert!(
                coverage.completed_at.is_some(),
                "{}",
                rt.sessions["fixture"].job.text
            );
            assert_eq!(coverage.sample.len(), 201);
            assert!(coverage.tracks.values().all(|e| e.recco_checked
                && e.feature_count == 2
                && e.features.is_empty()
                && e.artist_tags.is_empty()));
            assert!(!coverage.include_tags);
            coverage.completed_at = None;
            rt.store.put("fixture", "coverage", &coverage).unwrap();
        }
        assert_eq!(
            fixture_post(&app, "/coverage", "csrf=fixture-token&limit=0".into())
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        idle(&app).await;
        assert_eq!(*calls.lock().unwrap(), vec![40, 40, 40, 40, 40, 1]);
        spotify.abort();
        provider.abort();
    }

    #[tokio::test]
    async fn publishing_cannot_be_paused_disconnected_or_invalidated_by_sync() {
        let (app, _dir, task, _) = seeded_app().await;
        fixture_post(&app, "/build", "csrf=fixture-token&limit=5".into()).await;
        idle(&app).await;
        {
            let mut rt = app.runtime.lock().unwrap();
            let mut p: Preview = rt.store.get("fixture", "preview").unwrap().unwrap();
            p.phase = Phase::Created;
            p.playlist_id = Some("p".repeat(22));
            rt.store.put("fixture", "preview", &p).unwrap();
            let s = rt.sessions.get_mut("fixture").unwrap();
            s.job.running = true;
            s.job.publishing = true;
        }
        for path in ["/pause", "/disconnect", "/sync"] {
            assert_eq!(
                fixture_post(&app, path, "csrf=fixture-token".into())
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        {
            let mut rt = app.runtime.lock().unwrap();
            rt.sessions.get_mut("fixture").unwrap().job.running = false;
        }
        assert_eq!(
            fixture_post(&app, "/sync", "csrf=fixture-token".into())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        let p: Preview = app
            .runtime
            .lock()
            .unwrap()
            .store
            .get("fixture", "preview")
            .unwrap()
            .unwrap();
        assert_eq!(
            fixture_post(
                &app,
                "/preview/abandon",
                format!("csrf=fixture-token&preview_id={}", p.id)
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture_post(
                &app,
                "/preview/abandon",
                format!("csrf=fixture-token&preview_id={}&confirm=yes", p.id)
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        let rt = app.runtime.lock().unwrap();
        assert!(
            rt.store
                .get::<Preview>("fixture", "preview")
                .unwrap()
                .is_none()
        );
        assert!(
            rt.store
                .get::<Preview>("fixture", &format!("save-history:{}", p.id))
                .unwrap()
                .is_some()
        );
        task.abort();
    }

    #[tokio::test]
    async fn disconnect_preserves_cache_but_confirmed_delete_is_account_scoped() {
        let (app, _dir, task, _) = seeded_app().await;
        let account = app.runtime.lock().unwrap().sessions["fixture"]
            .account
            .clone();
        assert_eq!(
            fixture_post(&app, "/delete-data", "csrf=fixture-token".into())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture_post(&app, "/logout", "csrf=fixture-token".into())
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let csrf = {
            let mut rt = app.runtime.lock().unwrap();
            assert!(
                rt.store
                    .get::<Snapshot>("fixture", "snapshot")
                    .unwrap()
                    .is_some()
            );
            assert!(rt.sessions["fixture"].account.is_none());
            rt.store
                .put("other", "snapshot", &Snapshot::default())
                .unwrap();
            let s = rt.sessions.get_mut("fixture").unwrap();
            s.account = account;
            s.csrf.clone()
        };
        assert_eq!(
            fixture_post(&app, "/delete-data", format!("csrf={csrf}&consent=yes"))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
        let rt = app.runtime.lock().unwrap();
        assert!(
            rt.store
                .get::<Snapshot>("fixture", "snapshot")
                .unwrap()
                .is_none()
        );
        assert!(
            rt.store
                .get::<Snapshot>("other", "snapshot")
                .unwrap()
                .is_some()
        );
        task.abort();
    }

    #[tokio::test]
    async fn coverage_ignores_legacy_disabled_setting_and_needs_no_checkbox() {
        let (mut app, _dir, task, _) = seeded_app().await;
        let (enrichment, provider, calls) = enrichment_mock().await;
        app.enrichment = Arc::new(enrichment);
        app.runtime
            .lock()
            .unwrap()
            .store
            .put(
                "fixture",
                "settings",
                &Settings {
                    music_matching: false,
                },
            )
            .unwrap();
        let response = fixture_post(&app, "/coverage", "csrf=fixture-token&limit=0".into()).await;
        assert_eq!(response.headers()[header::LOCATION], "/?tab=coverage");
        idle(&app).await;
        assert_eq!(*calls.lock().unwrap(), vec![12]);
        assert!(
            app.runtime
                .lock()
                .unwrap()
                .store
                .get::<Coverage>("fixture", "coverage")
                .unwrap()
                .unwrap()
                .completed_at
                .is_some()
        );
        task.abort();
        provider.abort();
    }

    #[test]
    fn full_coverage_selects_all_accessible_tracks_or_one_source() {
        use crate::model::{Membership, Source, Track};
        let mut snapshot = Snapshot {
            sources: vec![
                Source {
                    id: "liked".into(),
                    accessible: true,
                    ..Default::default()
                },
                Source {
                    id: "p".repeat(22),
                    accessible: true,
                    ..Default::default()
                },
                Source {
                    id: "x".repeat(22),
                    accessible: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        for i in 0..2430 {
            let id = format!("{i:022}");
            snapshot.tracks.insert(
                id.clone(),
                Track {
                    id,
                    memberships: vec![Membership {
                        source: if i < 1000 {
                            "liked".into()
                        } else {
                            "p".repeat(22)
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            );
        }
        assert_eq!(coverage_pool(&snapshot, "", 0).unwrap().len(), 2430);
        assert_eq!(coverage_pool(&snapshot, "liked", 0).unwrap().len(), 1000);
        assert_eq!(coverage_pool(&snapshot, "", 200).unwrap().len(), 200);
        assert!(coverage_pool(&snapshot, &"x".repeat(22), 0).is_err());
        assert!(coverage_pool(&snapshot, "missing", 0).is_err());
        assert!(coverage_pool(&snapshot, "", 201).is_err());
    }

    #[tokio::test]
    async fn coverage_start_cannot_overwrite_checkpoint_while_another_job_runs() {
        let (app, _dir, task, _) = seeded_app().await;
        {
            let mut rt = app.runtime.lock().unwrap();
            rt.store
                .put(
                    "fixture",
                    "settings",
                    &Settings {
                        music_matching: true,
                    },
                )
                .unwrap();
            rt.store
                .put(
                    "fixture",
                    "coverage",
                    &Coverage {
                        sample: vec!["sentinel".into()],
                        ..Default::default()
                    },
                )
                .unwrap();
            rt.sessions.get_mut("fixture").unwrap().job.running = true;
        }
        assert_eq!(
            fixture_post(&app, "/coverage", "csrf=fixture-token&limit=0".into())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            app.runtime
                .lock()
                .unwrap()
                .store
                .get::<Coverage>("fixture", "coverage")
                .unwrap()
                .unwrap()
                .sample,
            ["sentinel"]
        );
        task.abort();
    }

    #[tokio::test]
    async fn pause_disconnect_delete_and_expiry_abort_queued_workers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for path in ["/pause", "/disconnect", "/delete-data", "expiry"] {
            let (app, _dir, task, _) = seeded_app().await;
            let gate = Arc::new(tokio::sync::Notify::new());
            let calls = Arc::new(AtomicUsize::new(0));
            let g = gate.clone();
            let c = calls.clone();
            let job = "queued-job".to_owned();
            {
                let mut rt = app.runtime.lock().unwrap();
                rt.sessions.get_mut("fixture").unwrap().job = Job {
                    id: job.clone(),
                    running: true,
                    ..Default::default()
                };
            }
            spawn_job(app.clone(), "fixture".into(), job, async move {
                g.notified().await;
                c.fetch_add(1, Ordering::SeqCst);
            });
            tokio::task::yield_now().await;
            if path == "expiry" {
                app.runtime
                    .lock()
                    .unwrap()
                    .sessions
                    .get_mut("fixture")
                    .unwrap()
                    .created = now() - 86401;
                app.cleanup();
            } else {
                assert_eq!(
                    fixture_post(&app, path, "csrf=fixture-token&consent=yes".into())
                        .await
                        .status(),
                    StatusCode::SEE_OTHER
                );
            }
            gate.notify_one();
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{path}");
            task.abort();
        }
    }

    #[tokio::test]
    #[ignore = "Local synthetic browser fixture; no real provider calls"]
    async fn browser_fixture() {
        let (mut app, _dir, _mock, _created) = seeded_app().await;
        let (enrichment, _provider, _calls) =
            enrichment_mock_with_delay(Duration::from_secs(2)).await;
        app.enrichment = Arc::new(enrichment);
        Arc::get_mut(&mut app.config)
            .unwrap()
            .redirect
            .set_port(Some(8788))
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8788")
            .await
            .unwrap();
        println!(
            "Synthetic browser fixture http://127.0.0.1:8788; cookie promptlist_session=fixture"
        );
        let fixture = router(app)
            .layer(axum::middleware::from_fn(
                |request: axum::extract::Request, next: axum::middleware::Next| async move {
                    if request.uri().path() == "/player.js" {
                        return (
                            [(header::CONTENT_TYPE, "text/javascript")],
                            include_str!("../assets/player.js").replace(
                                "https://sdk.scdn.co/spotify-player.js",
                                "/fixture-sdk.js",
                            ),
                        )
                            .into_response();
                    }
                    next.run(request).await
                },
            ))
            .route(
                "/fixture-sdk.js",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/javascript")],
                        include_str!("../tests/spotify-sdk-fixture.js"),
                    )
                }),
            )
            .route(
                "/fixture-login",
                get(|| async {
                    (
                        [(
                            header::SET_COOKIE,
                            "promptlist_session=fixture; Path=/; HttpOnly; SameSite=Lax",
                        )],
                        Redirect::to("/"),
                    )
                }),
            );
        axum::serve(listener, fixture).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Explicit opt-in: one bounded AI interpretation of synthetic intent, no library data"]
    async fn live_intent_smoke() {
        let root = std::env::current_dir().unwrap();
        let config = Config::load(&root).unwrap();
        let key = config.openai_key.as_deref().expect("OpenAI key required");
        let client = provider::http_client().unwrap();
        let preflight = client
            .get(format!(
                "https://api.openai.com/v1/models/{}",
                crate::config::DEFAULT_MODEL
            ))
            .bearer_auth(key)
            .send()
            .await
            .map_err(provider::network_error)
            .unwrap();
        assert_eq!(
            preflight.status(),
            reqwest::StatusCode::OK,
            "Model access/transport preflight failed"
        );
        let criteria =
            crate::intent::interpret(&client, key, "5 non-explicit tracks released in 2020")
                .await
                .unwrap();
        assert_eq!(criteria.limit, 5);
        assert_eq!(criteria.explicit, crate::selection::Explicit::CleanOnly);
        assert_eq!(criteria.release_year.unwrap().min, Some(2020));
        println!(
            "PASS: model transport/access and one synthetic intent interpretation. No library input or web-search tools."
        );
    }
    fn test_app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::from_values(
            HashMap::from([
                ("SPOTIFY_CLIENT_ID".into(), "a".repeat(32)),
                (
                    "SPOTIFY_REDIRECT_URI".into(),
                    "http://127.0.0.1:8787/callback".into(),
                ),
            ]),
            dir.path().into(),
        )
        .unwrap();
        let store = Store::open(dir.path()).unwrap();
        (App::new(config, store).unwrap(), dir)
    }
    #[tokio::test]
    async fn theme_assets_reject_unlisted_paths_and_keep_fonts_same_origin() {
        let (app, _d) = test_app();
        for path in ["/theme/.env.local", "/theme/style.css", "/fixture-login"] {
            let response = router(app.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .header("host", "127.0.0.1:8787")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let response = router(app)
            .oneshot(
                HttpRequest::builder()
                    .uri("/style.css")
                    .header("host", "127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("font-src 'self'")
        );
    }

    #[tokio::test]
    async fn rejects_dns_rebinding_host() {
        let (app, _d) = test_app();
        let response = router(app)
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header("host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn rejects_cross_origin_posts() {
        let (app, _d) = test_app();
        let response = router(app)
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/sync")
                    .header("host", "127.0.0.1:8787")
                    .header("origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn home_escapes_input_and_has_no_inline_script_refresh_or_secret() {
        let (app, _d) = test_app();
        let response = router(app)
            .oneshot(
                HttpRequest::builder()
                    .uri("/?q=%3Cscript%3E")
                    .header("host", "127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["referrer-policy"], "same-origin");
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("HttpOnly")
        );
        let html = String::from_utf8(
            to_bytes(response.into_body(), 1_000_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!html.contains("<script>"));
        assert!(!html.contains("http-equiv=\"refresh\""));
        assert!(html.contains("src=\"/app.js\""));
        assert!(!html.contains(&"a".repeat(32)));
        assert!(html.contains("Connect Spotify"));
    }
    #[tokio::test]
    async fn progress_requires_session_and_exposes_only_its_job() {
        let (app, _dir, server, _created) = seeded_app().await;
        {
            let mut rt = app.runtime.lock().unwrap();
            let session = rt.sessions.get_mut("fixture").unwrap();
            session.job.text = "Finding <your> sound".into();
            session.job.running = true;
            session.job.completed = Some(2);
            session.job.total = Some(12);
        }
        for (cookie, expected) in [
            ("", StatusCode::UNAUTHORIZED),
            ("promptlist_session=unknown", StatusCode::UNAUTHORIZED),
            ("promptlist_session=fixture", StatusCode::OK),
        ] {
            let response = router(app.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/api/job")
                        .header("host", "127.0.0.1:8787")
                        .header("cookie", cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            if expected == StatusCode::OK {
                let body = to_bytes(response.into_body(), 100_000).await.unwrap();
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(value.as_object().unwrap().len(), 3);
                assert_eq!(value["running"], true);
                let html = value["status_html"].as_str().unwrap();
                assert!(html.contains("Finding &lt;your&gt; sound"));
                assert!(html.contains("value=\"2\" max=\"12\""));
                assert!(!html.contains("Open Circuit"));
            }
        }
        server.abort();
    }
    #[tokio::test]
    async fn callback_without_browser_session_is_rejected() {
        let (app, _d) = test_app();
        let response = router(app)
            .oneshot(
                HttpRequest::builder()
                    .uri("/callback?state=bad&code=secret")
                    .header("host", "127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        let html = String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!html.contains("code=secret"));
    }
    #[test]
    fn csrf_form_token_is_required() {
        let (app, _d) = test_app();
        let mut rt = app.runtime.lock().unwrap();
        rt.sessions.insert(
            "session".into(),
            Session {
                csrf: "right".into(),
                auto_sync_pending: false,
                created: now(),
                pending: None,
                account: None,
                job: Job::default(),
                notice: None,
                build: None,
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "promptlist_session=session".parse().unwrap());
        assert!(
            authorize(
                &rt,
                &headers,
                &Action {
                    csrf: "wrong".into(),
                    restart: false,
                    consent: None,
                }
            )
            .is_err()
        );
    }

    fn pending_session(app: &App) -> (HeaderMap, String) {
        let (_, pending) = provider::authorization(&app.config.client_id, &app.config.redirect);
        let state = pending.state.clone();
        app.runtime.lock().unwrap().sessions.insert(
            "session".into(),
            Session {
                csrf: "form-token".into(),
                auto_sync_pending: false,
                created: now(),
                pending: Some(pending),
                account: None,
                job: Job::default(),
                notice: None,
                build: None,
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "promptlist_session=session".parse().unwrap());
        (headers, state)
    }

    #[tokio::test]
    async fn login_form_requires_exact_origin_and_redirects_with_valid_csrf() {
        for (origin, expected) in [
            ("null", StatusCode::FORBIDDEN),
            ("https://evil.example", StatusCode::FORBIDDEN),
            ("http://127.0.0.1:8787", StatusCode::SEE_OTHER),
        ] {
            let (app, _d) = test_app();
            pending_session(&app);
            let response = router(app)
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri("/auth/connect")
                        .header("host", "127.0.0.1:8787")
                        .header("origin", origin)
                        .header("cookie", "promptlist_session=session")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from("csrf=form-token"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::SEE_OTHER {
                assert!(
                    response.headers()["location"]
                        .to_str()
                        .unwrap()
                        .starts_with("https://accounts.spotify.com/authorize?")
                );
            }
        }
    }

    #[tokio::test]
    async fn callback_state_is_single_use_even_when_authorization_is_denied() {
        let (app, _d) = test_app();
        let (headers, state) = pending_session(&app);
        let first = callback(
            State(app.clone()),
            headers.clone(),
            Query(Callback {
                state: Some(state.clone()),
                code: None,
                error: Some("access_denied".into()),
            }),
        )
        .await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER);
        let replay = callback(
            State(app),
            headers,
            Query(Callback {
                state: Some(state),
                code: None,
                error: Some("access_denied".into()),
            }),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn disconnect_cancels_pending_login_and_rotates_form_token() {
        let (app, _d) = test_app();
        let (headers, _) = pending_session(&app);
        let response = disconnect(
            State(app.clone()),
            headers,
            Form(Action {
                csrf: "form-token".into(),
                restart: false,
                consent: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let rt = app.runtime.lock().unwrap();
        let session = &rt.sessions["session"];
        assert!(session.pending.is_none());
        assert!(session.account.is_none());
        assert_ne!(session.csrf, "form-token");
    }
}
