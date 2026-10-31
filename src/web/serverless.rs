//! Lambda HTTP adapter and durable, single-account worker execution.
use super::*;
use crate::cloud::{Aws, Lease, decode, hash, item};
use axum::body::{Body, to_bytes};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Serialize, Deserialize)]
struct SavedSession {
    #[serde(default)]
    auto_sync_pending: bool,
    csrf: String,
    created: i64,
    pending: Option<SavedPending>,
    profile: Option<Profile>,
    tokens: Option<provider::Tokens>,
    generation: Option<String>,
    notice: Option<String>,
    build: Option<BuildForm>,
}
#[derive(Serialize, Deserialize)]
struct SavedPending {
    state: String,
    verifier: String,
    created: i64,
}

fn account_pk(account: &str) -> String {
    format!("account#{}", hash(account))
}
fn session_pk(sid: &str) -> String {
    format!("session#{}", hash(sid))
}
fn expired(item: &Value) -> bool {
    item["expires"]["N"]
        .as_str()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|v| v <= now())
}
fn cookie_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == "promptlist_session"
                && (20..=100).contains(&value.len())
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)))
            .then(|| value.into())
        })
}
fn csrf(body: &str) -> Option<String> {
    url::form_urlencoded::parse(body.as_bytes())
        .find(|(k, _)| k == "csrf")
        .map(|(_, v)| v.into_owned())
}
fn plain(status: u16, text: &str) -> Value {
    json!({"statusCode":status,"headers":{"content-type":"text/plain; charset=utf-8","cache-control":"no-store","strict-transport-security":"max-age=31536000","x-content-type-options":"nosniff"},"body":text,"isBase64Encoded":false})
}

fn load_session(aws: &Aws, sid: &str) -> Result<Option<SavedSession>> {
    let Some(raw) = aws.get(&session_pk(sid), "session")? else {
        return Ok(None);
    };
    if expired(&raw) {
        return Ok(None);
    };
    let session: SavedSession = serde_json::from_value(decode(&raw)?)?;
    if now() - session.created >= 86400 {
        return Ok(None);
    };
    if let Some(profile) = &session.profile {
        let control = aws
            .get(&account_pk(profile.account()), "control")?
            .map(|v| decode(&v))
            .transpose()?;
        if control.as_ref().and_then(|c| c["generation"].as_str()) != session.generation.as_deref()
        {
            return Ok(None);
        };
    }
    Ok(Some(session))
}

async fn restore_session(app: &App, sid: &str, saved: SavedSession) {
    let account = match (saved.profile, saved.tokens) {
        (Some(profile), Some(tokens)) => Some(Arc::new(Account {
            profile,
            api: Spotify::new(
                app.http.clone(),
                app.config.client_id.clone(),
                tokens,
                app.gate.clone(),
                app.quota.clone(),
            ),
        })),
        _ => None,
    };
    let pending = saved
        .pending
        .filter(|p| now() - p.created < 600)
        .map(|p| PendingAuth {
            state: p.state,
            verifier: oauth2::PkceCodeVerifier::new(p.verifier),
            created: Instant::now() - Duration::from_secs((now() - p.created).max(0) as u64),
        });
    app.runtime.lock().unwrap().sessions.insert(
        sid.into(),
        Session {
            auto_sync_pending: saved.auto_sync_pending,
            csrf: saved.csrf,
            created: saved.created,
            pending,
            account,
            job: Job::default(),
            notice: saved.notice,
            build: saved.build,
        },
    );
}

async fn save_session(app: &App, aws: &Aws, sid: &str, generation: Option<String>) -> Result<()> {
    let (mut saved, account) = {
        let rt = app.runtime.lock().unwrap();
        let Some(s) = rt.sessions.get(sid) else {
            aws.delete(&session_pk(sid), "session")?;
            return Ok(());
        };
        (
            SavedSession {
                auto_sync_pending: s.auto_sync_pending,
                csrf: s.csrf.clone(),
                created: s.created,
                pending: s.pending.as_ref().map(|p| SavedPending {
                    state: p.state.clone(),
                    verifier: p.verifier.secret().clone(),
                    created: now() - p.created.elapsed().as_secs() as i64,
                }),
                profile: s.account.as_ref().map(|a| a.profile.clone()),
                tokens: None,
                generation,
                notice: s.notice.clone(),
                build: s.build.clone(),
            },
            s.account.clone(),
        )
    };
    if let Some(account) = account {
        saved.tokens = Some(account.api.saved_tokens().await);
        aws.put(
            &account_pk(account.profile.account()),
            &session_pk(sid),
            &json!({"session_pk":session_pk(sid)}),
            Some(saved.created + 86400),
        )?;
    }
    aws.put(
        &session_pk(sid),
        "session",
        &serde_json::to_value(&saved)?,
        Some(saved.created + 86400),
    )
}

fn load_job(aws: &Aws, pk: &str) -> Result<Option<Value>> {
    let Some(raw) = aws.get(pk, "job")? else {
        return Ok(None);
    };
    if expired(&raw) {
        return Ok(None);
    };
    Ok(Some(decode(&raw)?))
}
fn running(job: &Value) -> bool {
    matches!(job["status"].as_str(), Some("queued" | "running"))
        && job["deadline"].as_i64().unwrap_or(0) > now()
}
fn overlay_job(app: &App, sid: &str, job: &Value) {
    let mut rt = app.runtime.lock().unwrap();
    let Some(s) = rt.sessions.get_mut(sid) else {
        return;
    };
    s.job = Job {
        id: job["id"].as_str().unwrap_or_default().into(),
        running: running(job),
        text: if !running(job) && matches!(job["status"].as_str(), Some("queued" | "running")) {
            "Operation interrupted. Saved checkpoints are preserved; review before retrying.".into()
        } else {
            job["text"].as_str().unwrap_or("Queued").into()
        },
        publishing: job["path"] == "/publish",
        completed: job["completed"].as_u64().map(|n| n as usize),
        total: job["total"].as_u64().map(|n| n as usize),
        ..Default::default()
    };
}

fn write_job(aws: &Aws, pk: &str, job: &Value, lease: Option<&Lease>) -> Result<()> {
    let mut data = item(pk, "job", job, Some(now() + 86400))?;
    data["status"] = json!({"S":job["status"].as_str().unwrap_or("failed")});
    let write = json!({"Put":{"TableName":aws.table,"Item":data}});
    let writes = match lease {
        Some(lease) => vec![lease.check(), write],
        None => vec![write],
    };
    aws.ddb("TransactWriteItems", json!({"TransactItems":writes}))?;
    Ok(())
}

fn config() -> Result<Config> {
    Config::load(std::path::Path::new("."))
}
fn headers(event: &Value) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(map) = event["headers"].as_object() {
        for (k, v) in map {
            if let (Ok(k), Some(v)) = (axum::http::HeaderName::from_bytes(k.as_bytes()), v.as_str())
                && let Ok(v) = v.parse()
            {
                headers.insert(k, v);
            }
        }
    }
    if let Some(cookies) = event["cookies"].as_array() {
        let raw = cookies
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        if let Ok(v) = raw.parse() {
            headers.insert(header::COOKIE, v);
        }
    }
    headers
}
fn request(method: &str, uri: &str, headers: &HeaderMap, body: Vec<u8>) -> Result<Request> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(body))
        .map_err(|_| message("Invalid request."))?;
    *request.headers_mut() = headers.clone();
    Ok(request)
}
async fn response(response: Response) -> Result<Value> {
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 5_500_000)
        .await
        .map_err(|_| message("Response too large."))?;
    let mut headers = serde_json::Map::new();
    let mut cookies = vec![];
    for (key, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            if key == header::SET_COOKIE {
                cookies.push(value.to_owned());
            } else {
                headers.insert(key.to_string(), json!(value));
            }
        }
    }
    Ok(
        json!({"statusCode":parts.status.as_u16(),"headers":headers,"cookies":cookies,"body":STANDARD.encode(bytes),"isBase64Encoded":true}),
    )
}

pub async fn handle(event: Value) -> Result<Value> {
    if event.get("Records").is_some() {
        worker(event).await?;
        return Ok(json!({"ok":true}));
    }
    let config = config()?;
    let headers = headers(&event);
    let path = event["rawPath"].as_str().unwrap_or("/");
    if path == "/coverage" {
        return Ok(plain(404, "Not found"));
    }
    let method = event["requestContext"]["http"]["method"]
        .as_str()
        .unwrap_or("GET");
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(config.host_header()) {
        if let Some(location) = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .and_then(|host| config.redirect_location(host, method))
        {
            return Ok(
                json!({"statusCode":308,"headers":{"location":location,"cache-control":"no-store","strict-transport-security":"max-age=31536000","referrer-policy":"no-referrer"},"body":""}),
            );
        }
        return Ok(plain(403, "Invalid host"));
    }
    if !matches!(method, "GET" | "HEAD")
        && headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
            != Some(config.origin().as_str())
    {
        return Ok(plain(403, "Invalid origin"));
    }
    let raw = event["body"].as_str().unwrap_or("");
    if raw.len() > 12_000 {
        return Ok(plain(413, "Request too large"));
    }
    let bytes = if event["isBase64Encoded"] == true {
        STANDARD
            .decode(raw)
            .map_err(|_| message("Invalid request body."))?
    } else {
        raw.as_bytes().to_vec()
    };
    if bytes.len() > 8192 {
        return Ok(plain(413, "Request too large"));
    }
    let query = event["rawQueryString"].as_str().unwrap_or("");
    let uri = if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    };
    let public = path == "/health"
        || path == "/style.css"
        || path == "/app.js"
        || path == "/player.js"
        || path.starts_with("/theme/")
        || path.starts_with("/icons/");
    if public {
        let app = App::new(config, Store::memory()?)?;
        let mut reply = router(app)
            .oneshot(request(method, &uri, &headers, bytes)?)
            .await
            .unwrap();
        if path != "/health" && reply.status().is_success() {
            reply.headers_mut().insert(
                header::CACHE_CONTROL,
                "public, max-age=3600".parse().unwrap(),
            );
        }
        return response(reply).await;
    }
    let aws = Aws::env()?;
    let sid = cookie_id(&headers);
    let saved = if let Some(id) = &sid {
        load_session(&aws, id)?
    } else {
        None
    };
    if saved
        .as_ref()
        .and_then(|s| s.profile.as_ref())
        .is_some_and(|p| !config.allows_user(p.account()))
    {
        return Ok(plain(403, "Account not enabled for this private beta."));
    }
    if method != "GET" && saved.is_none() {
        return Ok(plain(401, "Session expired. Reload and connect Spotify."));
    }
    let account = saved
        .as_ref()
        .and_then(|s| s.profile.as_ref())
        .map(|p| p.account().to_owned());
    let mut generation = saved.as_ref().and_then(|s| s.generation.clone());
    let pk = account
        .as_deref()
        .map(account_pk)
        .unwrap_or_else(|| session_pk(sid.as_deref().unwrap_or("new")));
    let current = if account.is_some() {
        load_job(&aws, &pk)?
    } else {
        None
    };
    let busy = current.as_ref().is_some_and(running);
    if path == "/api/job" && method == "GET" {
        let Some(saved) = &saved else {
            return Ok(plain(401, "Session expired"));
        };
        if account.is_none() {
            return Ok(plain(401, "Connect Spotify first"));
        }
        let text = current
            .as_ref()
            .and_then(|v| v["text"].as_str())
            .unwrap_or("");
        let stale = current.as_ref().is_some_and(|v| {
            matches!(v["status"].as_str(), Some("queued" | "running")) && !running(v)
        });
        let text = if stale {
            "Operation interrupted. Checkpoints retained; review before retrying."
        } else {
            text
        };
        let html = crate::ui::job_status(
            text,
            busy,
            current.as_ref().is_some_and(|v| v["path"] == "/publish"),
            current
                .as_ref()
                .and_then(|v| v["completed"].as_u64())
                .map(|v| v as usize),
            current
                .as_ref()
                .and_then(|v| v["total"].as_u64())
                .map(|v| v as usize),
            &saved.csrf,
        )
        .into_string();
        let mut reply = plain(
            200,
            &serde_json::to_string(
                &json!({"id":current.as_ref().map(|j|&j["id"]),"running":busy,"status_html":html}),
            )?,
        );
        reply["headers"]["content-type"] = json!("application/json");
        return Ok(reply);
    }
    let body = std::str::from_utf8(&bytes).map_err(|_| message("Invalid form encoding."))?;
    if method == "POST"
        && saved
            .as_ref()
            .is_none_or(|s| csrf(body).as_deref() != Some(s.csrf.as_str()))
    {
        return Ok(plain(403, "Invalid request verification."));
    }
    if method == "POST" && path == "/pause" && busy {
        let job = current.as_ref().unwrap();
        if job["path"] == "/publish" {
            return Ok(plain(409, "A Spotify save cannot be cancelled safely."));
        }
        aws.put(&pk, "cancel", &json!({"id":job["id"]}), Some(now() + 900))?;
        return Ok(
            json!({"statusCode":303,"headers":{"location":"/","cache-control":"no-store"},"body":""}),
        );
    }
    if busy && method == "POST" && !path.starts_with("/api/playback/") {
        return Ok(plain(
            409,
            "An operation is already running. Its progress and saved state are preserved.",
        ));
    }
    if busy
        && path.starts_with("/api/playback/")
        && saved
            .as_ref()
            .and_then(|s| s.tokens.as_ref())
            .is_none_or(|t| !t.is_fresh())
    {
        return Ok(plain(
            409,
            "Playback authorization is refreshing. Try again after the current operation.",
        ));
    }
    let needs_lease = path != "/api/job" && !busy;
    let lease = if needs_lease {
        match aws.lock(&pk, 45) {
            Ok(lease) => Some(Arc::new(lease)),
            Err(_) => {
                return Ok(plain(
                    409,
                    "Account update in progress. Please try again shortly.",
                ));
            }
        }
    } else {
        None
    };
    let mut store = Store::memory()?;
    if let Some(account) = &account {
        store.attach_cloud(aws.clone(), account, lease.clone())?;
    }
    let app = App::new(config, store)?;
    if let (Some(sid), Some(saved)) = (&sid, saved) {
        restore_session(&app, sid, saved).await;
    }
    if let (Some(sid), Some(job)) = (&sid, &current) {
        overlay_job(&app, sid, job);
    }
    let queued = method == "POST"
        && matches!(
            path,
            "/build" | "/refine" | "/sync" | "/coverage" | "/publish"
        );
    if queued {
        if path == "/sync"
            && url::form_urlencoded::parse(body.as_bytes())
                .any(|(k, v)| k == "consent" && v == "automatic")
            && !sid
                .as_ref()
                .is_some_and(|id| app.runtime.lock().unwrap().sessions[id].auto_sync_pending)
        {
            return Ok(
                json!({"statusCode":303,"headers":{"location":"/","cache-control":"no-store"},"body":""}),
            );
        }
        if account.is_none() {
            return Ok(plain(401, "Connect Spotify first."));
        }
        // Debit worst-case worker time before durable dispatch. Failed submissions
        // consume their slot too, so retries cannot evade the spending allowance.
        let month = chrono::Utc::now().format("%Y-%m").to_string();
        aws.admit(&format!("jobs#{month}"), 200, now() + 62 * 86400)?;
        let job = json!({"id":provider::random_secret(),"sid":sid,"generation":generation,"path":path,"body":body,"status":"queued","text":"Queued","created":now(),"deadline":now()+700});
        write_job(&aws, &pk, &job, lease.as_deref())?;
        if let Some(lease) = lease {
            lease.release()?;
        }
        return Ok(
            json!({"statusCode":303,"headers":{"location":"/","cache-control":"no-store"},"body":""}),
        );
    }
    let reply = router(app.clone())
        .oneshot(request(method, &uri, &headers, bytes)?)
        .await
        .unwrap();
    if lease.is_some() {
        let sessions: Vec<String> = app
            .runtime
            .lock()
            .unwrap()
            .sessions
            .keys()
            .cloned()
            .collect();
        for id in sessions {
            let after = app.runtime.lock().unwrap().sessions[&id].account.clone();
            let mut account_lease = None;
            if account.is_none()
                && let Some(after) = &after
            {
                let account = after.profile.account();
                let pk = account_pk(account);
                let acquired = Arc::new(aws.lock(&pk, 45)?);
                generation = Some(match aws.get(&pk, "control")? {
                    Some(raw) => decode(&raw)?["generation"]
                        .as_str()
                        .ok_or_else(|| message("Invalid account generation."))?
                        .into(),
                    None => provider::random_secret(),
                });
                aws.put(&pk, "control", &json!({"generation":generation}), None)?;
                app.runtime.lock().unwrap().store.attach_cloud(
                    aws.clone(),
                    account,
                    Some(acquired.clone()),
                )?;
                history::ensure_user(&app.runtime.lock().unwrap().store.db, account)?;
                history::event(
                    &app.runtime.lock().unwrap().store.db,
                    account,
                    "signed_in",
                    None,
                    json!({}),
                )?;
                account_lease = Some(acquired);
            }
            if matches!(path, "/delete-data" | "/disconnect")
                && reply.status().is_redirection()
                && account.is_some()
            {
                aws.put(
                    &pk,
                    "control",
                    &json!({"generation":provider::random_secret()}),
                    None,
                )?;
                aws.delete(&pk, "job")?;
                aws.delete(&pk, "cancel")?;
                aws.purge_account(&pk)?;
                for row in aws.query(&pk, "session#")? {
                    if let Some(target) = decode(&row)?["session_pk"].as_str() {
                        aws.delete(target, "session")?;
                    }
                    if let Some(sk) = row["sk"]["S"].as_str() {
                        aws.delete(&pk, sk)?;
                    }
                }
            }
            app.runtime.lock().unwrap().store.flush()?;
            if sid.is_none() {
                aws.admit(
                    &format!("anonymous#{}", now() / 86400),
                    100,
                    now() + 2 * 86400,
                )?;
            }
            save_session(&app, &aws, &id, generation.clone()).await?;
            if let Some(lease) = account_lease {
                lease.release()?;
            }
        }
        if let Some(lease) = lease {
            lease.release()?;
        }
    }
    response(reply).await
}

async fn worker(event: Value) -> Result<()> {
    let aws = Aws::env()?;
    let records = event["Records"]
        .as_array()
        .ok_or_else(|| message("Invalid worker event."))?;
    for record in records {
        let pk = record["dynamodb"]["Keys"]["pk"]["S"]
            .as_str()
            .ok_or_else(|| message("Invalid worker key."))?;
        if record["dynamodb"]["Keys"]["sk"]["S"] != "job"
            || record["dynamodb"]["NewImage"]["status"]["S"] != "queued"
        {
            continue;
        }
        let expected = decode(&record["dynamodb"]["NewImage"])?;
        let mut acquired = None;
        for _ in 0..10 {
            if let Ok(lease) = aws.lock(pk, 650) {
                acquired = Some(Arc::new(lease));
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let lease = acquired.ok_or_else(|| message("Worker account is busy."))?;
        let Some(mut job) = load_job(&aws, pk)? else {
            lease.release()?;
            continue;
        };
        if job["status"] != "queued" || job["id"] != expected["id"] || !running(&job) {
            lease.release()?;
            continue;
        }
        let sid = job["sid"]
            .as_str()
            .ok_or_else(|| message("Invalid worker session."))?
            .to_owned();
        let Some(saved) = load_session(&aws, &sid)? else {
            job["status"] = json!("failed");
            job["text"] = json!("Session expired. Reconnect before trying again.");
            write_job(&aws, pk, &job, Some(&lease))?;
            lease.release()?;
            continue;
        };
        if saved.generation.as_ref().map(|v| json!(v)) != job.get("generation").cloned()
            || saved
                .profile
                .as_ref()
                .map(|p| account_pk(p.account()))
                .as_deref()
                != Some(pk)
        {
            lease.release()?;
            continue;
        }
        let account = saved.profile.as_ref().unwrap().account().to_owned();
        if !config()?.allows_user(&account) {
            lease.release()?;
            continue;
        }
        let generation = saved.generation.clone();
        let csrf = saved.csrf.clone();
        let mut store = Store::memory()?;
        store.attach_cloud(aws.clone(), &account, Some(lease.clone()))?;
        let app = App::new(config()?, store)?;
        restore_session(&app, &sid, saved).await;
        job["status"] = json!("running");
        job["text"] = json!("Starting");
        job["deadline"] = json!(now() + 620);
        write_job(&aws, pk, &job, Some(&lease))?;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, app.config.host_header().parse().unwrap());
        headers.insert(header::ORIGIN, app.config.origin().parse().unwrap());
        headers.insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        headers.insert(
            header::COOKIE,
            format!("promptlist_session={sid}").parse().unwrap(),
        );
        let request = request(
            "POST",
            job["path"].as_str().unwrap(),
            &headers,
            job["body"].as_str().unwrap_or_default().as_bytes().to_vec(),
        )?;
        let reply = router(app.clone()).oneshot(request).await.unwrap();
        let success = reply.status().is_redirection() || reply.status().is_success();
        if !success {
            let text = to_bytes(reply.into_body(), 8192).await.unwrap_or_default();
            job["text"] = json!(String::from_utf8_lossy(&text).to_string());
        }
        let started = Instant::now();
        loop {
            let live = {
                app.runtime
                    .lock()
                    .unwrap()
                    .sessions
                    .get(&sid)
                    .map(|s| s.job.clone())
                    .unwrap_or_default()
            };
            if success {
                job["text"] = json!(live.text);
                job["completed"] = json!(live.completed);
                job["total"] = json!(live.total);
            }
            app.runtime.lock().unwrap().store.flush()?;
            save_session(&app, &aws, &sid, generation.clone()).await?;
            if !live.running {
                job["status"] = json!(if success { "completed" } else { "failed" });
                write_job(&aws, pk, &job, Some(&lease))?;
                break;
            }
            if started.elapsed() > Duration::from_secs(480) {
                if let Some(abort) = live.abort {
                    abort.abort();
                }
                job["status"] = json!("failed");
                job["text"] =
                    json!("Time limit reached. Checkpoints are saved. Review before resuming.");
                write_job(&aws, pk, &job, Some(&lease))?;
                // The process must not retain an untracked Spotify writer.
                if live.publishing {
                    std::process::exit(1);
                }
                break;
            }
            if let Some(cancel) = aws.get(pk, "cancel")?
                && decode(&cancel)?["id"] == job["id"]
                && !live.publishing
            {
                let _ = pause(
                    State(app.clone()),
                    headers.clone(),
                    Form(basic_action(csrf.clone())),
                )
                .await;
                aws.delete(pk, "cancel")?;
            }
            write_job(&aws, pk, &job, Some(&lease))?;
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        job.as_object_mut().unwrap().remove("body");
        write_job(&aws, pk, &job, Some(&lease))?;
        lease.release()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookies_and_csrf_are_bounded_and_exact() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=x; promptlist_session=abcdefghijklmnopqrstuv"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            cookie_id(&headers).as_deref(),
            Some("abcdefghijklmnopqrstuv")
        );
        headers.insert(
            header::COOKIE,
            "promptlist_session=../../something".parse().unwrap(),
        );
        assert!(cookie_id(&headers).is_none());
        assert_eq!(
            csrf("prompt=csrf%3Dx&csrf=correct").as_deref(),
            Some("correct")
        );
        assert!(csrf("prompt=hello").is_none());
    }
    #[test]
    fn expired_claimed_jobs_never_look_running() {
        assert!(running(&json!({"status":"queued","deadline":now()+100})));
        assert!(!running(&json!({"status":"running","deadline":now()-1})));
        assert!(!running(
            &json!({"status":"completed","deadline":now()+100})
        ));
        assert!(expired(&json!({"expires":{"N":(now()-1).to_string()}})));
    }
    #[tokio::test]
    async fn lambda_response_preserves_secure_cookies_and_binary_content() {
        let reply = (
            [
                (header::SET_COOKIE, "id=synthetic; Secure; HttpOnly"),
                (header::CONTENT_TYPE, "image/png"),
            ],
            vec![0_u8, 255, 12],
        )
            .into_response();
        let output = response(reply).await.unwrap();
        assert_eq!(output["cookies"][0], "id=synthetic; Secure; HttpOnly");
        assert_eq!(
            STANDARD.decode(output["body"].as_str().unwrap()).unwrap(),
            vec![0, 255, 12]
        );
        assert_eq!(output["isBase64Encoded"], true);
    }
}
