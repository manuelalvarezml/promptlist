//! First-party account security records; no fingerprinting or advertising audiences.
use super::*;
use serde::{Deserialize, Serialize};

pub(super) async fn notice() -> Html<String> {
    Html(maud::html! { (maud::DOCTYPE) html lang="en" { head {
        meta charset="utf-8"; meta name="viewport" content="width=device-width, initial-scale=1";
        title {"Promptlist / Privacy"} link rel="stylesheet" href="/style.css";
    } body {main class="privacy-notice" {
        a.brand.wordmark href="/" {"promptlist"}
        h1 {"Privacy & cookies"}
        p {"We use an essential, first-party session cookie to keep this device signed in for up to 90 days. Signing out, clearing cookies, or revoked Spotify access can require reconnecting sooner."}
        p {"Your Spotify account ID, library snapshot, playlist requests and results are stored privately in AWS. Reconnecting on another device restores the same saved library. Spotify credentials never appear in analytics."}
        p {"Login security records include time, a random device identifier and coarse browser/device type. We do not fingerprint your device or store its raw browser header or IP address in app analytics. Operational action records are kept for 90 days; daily aggregate counts for one year. Saved libraries and prompt history remain until you delete your account data."}
        p {"Optional navigation and player-action analytics are off by default. You can enable or withdraw them in Settings & privacy. Operational security, request and playlist-edit records remain necessary to run and debug the service."}
        p {"OpenAI interprets your request text, not your Spotify library. Last.fm and ReccoBeats receive limited track identifiers or artist/title information for matching. Reusable evidence respects each provider's retention restrictions. Spotify playback contacts Spotify and follows its own privacy settings."}
        p {"There are no advertising cookies, pixels or audience uploads. Your music library and Spotify-derived activity are not used to target advertising. Deleting account data in Settings removes stored libraries, history, evidence and account records; signing out alone does not."}
        a href="/" {"Back to Promptlist"}
    }}}}.into_string())
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Privacy {
    pub analytics: bool,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize)]
struct Device {
    id: String,
    first_login: i64,
    last_login: i64,
    browser: String,
    platform: String,
    class: String,
}

fn device(headers: &HeaderMap, id: String) -> Device {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let browser = if ua.contains("Edg") {
        "Edge"
    } else if ua.contains("Firefox") || ua.contains("FxiOS") {
        "Firefox"
    } else if ua.contains("Chrome") || ua.contains("CriOS") {
        "Chrome"
    } else if ua.contains("Safari") {
        "Safari"
    } else {
        "Other"
    };
    let platform = if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "Other"
    };
    let class = if ua.contains("iPad") {
        "tablet"
    } else if ua.contains("Mobile") || ua.contains("iPhone") {
        "mobile"
    } else {
        "desktop_or_unknown"
    };
    Device {
        id,
        first_login: now(),
        last_login: now(),
        browser: browser.into(),
        platform: platform.into(),
        class: class.into(),
    }
}

pub(super) fn record_login(rt: &mut Runtime, sid: &str, account: &str, headers: &HeaderMap) {
    let has_snapshot = rt
        .store
        .get::<Snapshot>(account, "snapshot")
        .ok()
        .flatten()
        .is_some_and(|s| s.completed_at.is_some());
    let session = rt.sessions.get_mut(sid).unwrap();
    session.auto_sync_pending = !has_snapshot;
    session.created = now();
    let mut current = device(headers, session.device_id.clone());
    let mut devices: Vec<Device> = rt
        .store
        .get(account, "devices")
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(old) = devices.iter().find(|d| d.id == current.id) {
        current.first_login = old.first_login;
    }
    devices.retain(|d| d.id != current.id && now() - d.last_login < crate::config::SESSION_SECONDS);
    if devices.len() >= 20 {
        devices.remove(0);
    }
    let detail = serde_json::json!({"device_id":current.id,"browser":current.browser,"platform":current.platform,"device_class":current.class,"restored_library":has_snapshot});
    devices.push(current);
    if rt.store.put(account, "devices", &devices).is_err() {
        eprintln!("Promptlist: device record unavailable");
    }
    audit_event(rt, account, "signed_in", None, detail);
}

pub(super) fn analytics_enabled(rt: &Runtime, account: &str) -> bool {
    rt.store
        .get::<Privacy>(account, "privacy")
        .ok()
        .flatten()
        .is_some_and(|p| p.analytics)
}

#[derive(Deserialize)]
pub(super) struct PrivacyForm {
    csrf: String,
    analytics: Option<String>,
}

pub(super) async fn save(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<PrivacyForm>,
) -> Response {
    let result = (|| {
        let mut rt = app.runtime.lock().unwrap();
        let id = authorize(&rt, &headers, &basic_action(form.csrf))?;
        let account = rt.sessions[&id]
            .account
            .as_ref()
            .ok_or_else(|| message("Connect Spotify first."))?
            .profile
            .account()
            .to_owned();
        rt.store.put(
            &account,
            "privacy",
            &Privacy {
                analytics: form.analytics.as_deref() == Some("yes"),
                updated_at: now(),
            },
        )?;
        audit_event(
            &rt,
            &account,
            "privacy_changed",
            None,
            serde_json::json!({"analytics":analytics_enabled(&rt,&account)}),
        );
        Ok::<_, crate::Error>(())
    })();
    match result {
        Ok(()) => Redirect::to("/?tab=settings").into_response(),
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_record_is_coarse_and_not_a_fingerprint() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0) Mobile Safari/604.1"
                .parse()
                .unwrap(),
        );
        let record = device(&headers, "independent-id".into());
        let value = serde_json::to_string(&record).unwrap();
        assert_eq!(record.platform, "iOS");
        assert_eq!(record.class, "mobile");
        assert!(!value.contains("18_0"));
        assert!(!value.contains("604.1"));
    }
}
