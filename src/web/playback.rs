//! Account-bound browser playback. No arbitrary Spotify API proxy or token persistence.
use super::*;

fn account(app: &App, headers: &HeaderMap, csrf: &str) -> Result<(String, Arc<Account>)> {
    let rt = app.runtime.lock().unwrap();
    let id = authorize(
        &rt,
        headers,
        &Action {
            csrf: csrf.into(),
            restart: false,
            consent: None,
        },
    )?;
    let account = rt.sessions[&id]
        .account
        .clone()
        .ok_or_else(|| message("Connect Spotify to play music."))?;
    Ok((id, account))
}

fn failure(error: crate::Error) -> Response {
    let (status, text) = match error {
        crate::Error::Http { status: 401, .. } => (StatusCode::UNAUTHORIZED, "Reconnect Spotify to authorize playback.".into()),
        crate::Error::Http { status: 403, .. } => (StatusCode::FORBIDDEN, "Spotify denied playback. Enable Web Playback SDK in your app, use Premium and reconnect to grant playback permissions.".into()),
        crate::Error::Http { status: 404, .. } => (StatusCode::NOT_FOUND, "This browser player is not available. Reload and try again.".into()),
        other => (StatusCode::BAD_REQUEST, other.to_string()),
    };
    (status, axum::Json(serde_json::json!({"error":text}))).into_response()
}

pub(super) async fn token(
    State(app): State<App>,
    headers: HeaderMap,
    Form(action): Form<Action>,
) -> Response {
    let (id, account) = match account(&app, &headers, &action.csrf) {
        Ok(value) => value,
        Err(e) => return failure(e),
    };
    match account.api.playback_access_token().await {
        Ok(token) => {
            let rt = app.runtime.lock().unwrap();
            if session_id(&rt, &headers).ok().as_deref() != Some(&id)
                || !rt
                    .sessions
                    .get(&id)
                    .and_then(|s| s.account.as_ref())
                    .is_some_and(|a| Arc::ptr_eq(a, &account))
            {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            axum::Json(serde_json::json!({"access_token":token})).into_response()
        }
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
pub(super) struct Play {
    csrf: String,
    preview_id: String,
    track_id: String,
    device_id: String,
}

pub(super) async fn play(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<Play>,
) -> Response {
    let (_, account) = match account(&app, &headers, &form.csrf) {
        Ok(value) => value,
        Err(e) => return failure(e),
    };
    let selected = (|| -> Result<Vec<String>> {
        let rt = app.runtime.lock().unwrap();
        let owner = account.profile.account();
        let preview: Preview = rt
            .store
            .get(owner, "preview")?
            .ok_or_else(|| message("Build a playlist before playing."))?;
        let snapshot: Snapshot = rt
            .store
            .get(owner, "snapshot")?
            .ok_or_else(|| message("Sync your library first."))?;
        if preview.id != form.preview_id {
            return Err(message(
                "Playlist changed. Select a track from the current results.",
            ));
        }
        preview.validate(&snapshot)?;
        let index = preview
            .track_ids
            .iter()
            .position(|id| id == &form.track_id)
            .ok_or_else(|| message("This track is not in the current playlist."))?;
        // Start at the clicked song; subsequent tracks retain the approved order.
        Ok(preview.track_ids[index..].to_vec())
    })();
    let ids = match selected {
        Ok(ids) => ids,
        Err(e) => return failure(e),
    };
    match account.api.start_playback(&form.device_id, &ids).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failure(e),
    }
}
