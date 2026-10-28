//! Explicit synthetic cloud acceptance. Never calls Spotify or sends a library to AI.
use promptlist::{
    Result,
    cloud::{Aws, decode, hash},
    message,
    model::{Membership, Snapshot, Source, Track},
    now,
    preview::Preview,
    provider,
    store::Store,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let aws = Aws::env()?;
    let url = std::env::var("PROMPTLIST_CLOUD_URL").unwrap();
    let origin = url.trim_end_matches('/');
    let account = "promptlist-cloud-smoke";
    let model_test = std::env::args().any(|v| v == "--model");
    let pk = format!("account#{}", hash(account));
    let lease = Arc::new(aws.lock(&pk, 120)?);
    if !aws.query(&pk, "row#")?.is_empty() {
        return Err(message(
            "Synthetic account already has data; inspect and clean the previous smoke run.",
        ));
    }
    let sid = provider::random_secret();
    let csrf = provider::random_secret();
    let generation = provider::random_secret();
    aws.put(&pk, "control", &json!({"generation":generation}), None)?;
    let mut store = Store::memory()?;
    store.attach_cloud(aws.clone(), account, Some(lease.clone()))?;
    let mut snapshot = Snapshot {
        completed_at: Some(now()),
        sources: vec![Source {
            id: "liked".into(),
            name: "Liked Songs".into(),
            accessible: true,
            count: 2,
            ..Default::default()
        }],
        ..Default::default()
    };
    for (id, name) in [
        ("a".repeat(22), "Synthetic Alpha"),
        ("b".repeat(22), "Synthetic Beta"),
    ] {
        snapshot.tracks.insert(
            id.clone(),
            Track {
                id,
                name: name.into(),
                artists: vec!["Synthetic Artist".into()],
                memberships: vec![Membership {
                    source: "liked".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    }
    let mut blob = serde_json::to_value(snapshot)?;
    blob["acceptance_padding"] = json!("x".repeat(150_000));
    store.promote(account, &blob)?;
    let s3 = aws
        .query(&pk, "row#")?
        .iter()
        .filter_map(|r| r["blob"]["S"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    if s3.len() != 1 {
        return Err(message("Expected private compressed snapshot."));
    }
    let session = json!({"csrf":csrf,"created":now(),"pending":null,"profile":{"id":account,"account_id":null,"display_name":"Synthetic acceptance"},"tokens":{"access":"synthetic-not-a-real-token","refresh":"synthetic-not-a-real-token","expires_at":now()+3600},"generation":generation,"notice":null,"build":null});
    aws.put(
        &format!("session#{}", hash(&sid)),
        "session",
        &session,
        Some(now() + 86400),
    )?;
    lease.release()?;
    let replacement = aws.lock(&pk, 30)?;
    if store
        .put(
            account,
            "fenced-test",
            &json!({"invalid":"must not persist"}),
        )
        .is_ok()
    {
        return Err(message("Stale worker lease was accepted."));
    }
    replacement.release()?;
    let http = provider::http_client()?;
    let cookie = format!("promptlist_session={sid}");
    let page = http
        .get(&url)
        .header("cookie", &cookie)
        .send()
        .await
        .map_err(provider::network_error)?;
    if !page.status().is_success() {
        return Err(message(format!("Cloud page returned {}", page.status())));
    }
    let html = page.text().await.map_err(provider::network_error)?;
    if !html.contains("Synthetic acceptance") {
        return Err(message("Durable session was not restored."));
    }
    let bad = http
        .post(format!("{origin}/build"))
        .header("cookie", &cookie)
        .header("origin", "https://wrong.invalid")
        .form(&[("csrf", csrf.as_str()), ("prompt", "all tracks")])
        .send()
        .await
        .map_err(provider::network_error)?;
    if bad.status() != 403 {
        return Err(message("Origin denial failed."));
    }
    let reply = http
        .post(format!("{origin}/build"))
        .header("cookie", &cookie)
        .header("origin", origin)
        .form(&[
            ("csrf", csrf.as_str()),
            (
                "prompt",
                if model_test {
                    "strange romantic music"
                } else {
                    "all tracks"
                },
            ),
            ("limit", "2"),
        ])
        .send()
        .await
        .map_err(provider::network_error)?;
    if reply.status() != 303 {
        return Err(message(format!(
            "Queue submission returned {}",
            reply.status()
        )));
    }
    let started = std::time::Instant::now();
    let mut final_job = Value::Null;
    for _ in 0..35 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let job = aws
            .get(&pk, "job")?
            .ok_or_else(|| message("Job was not durable."))?;
        final_job = decode(&job)?;
        if !matches!(final_job["status"].as_str(), Some("queued" | "running")) {
            break;
        }
    }
    if final_job["status"] != "completed" {
        return Err(message(format!(
            "Worker did not complete: {}",
            final_job["text"]
        )));
    }
    let mut fresh = Store::memory()?;
    fresh.attach_cloud(aws.clone(), account, None)?;
    let preview: Preview = fresh
        .get(account, "preview")?
        .ok_or_else(|| message("Worker result was not durable."))?;
    if preview.track_ids.len() != 2 {
        return Err(message("Cloud preview mismatch."));
    }
    let rows = aws.query(&pk, "row#prompt_runs#")?;
    if rows.len() != 1 {
        return Err(message("Expected one persisted prompt history entry."));
    }
    if model_test {
        let row = aws.payload(&rows[0])?;
        let run: Value = serde_json::from_str(row["values"][3].as_str().unwrap())?;
        if run["interpretation"]["engine"] != "gpt-6.1-sol" {
            return Err(message(
                "Hosted model interpretation fell back; inspect provider access.",
            ));
        }
    }
    let mut duplicate = aws.get(&pk, "job")?.unwrap();
    duplicate["status"] = json!({"S":"queued"});
    promptlist::web::serverless::handle(json!({"Records":[{"dynamodb":{"Keys":{"pk":{"S":pk},"sk":{"S":"job"}},"NewImage":duplicate}}]})).await?;
    if aws.query(&pk, "row#prompt_runs#")?.len() != 1 {
        return Err(message("Duplicate worker delivery repeated the request."));
    }
    let result = http
        .post(format!("{origin}/delete-data"))
        .header("cookie", &cookie)
        .header("origin", origin)
        .form(&[("csrf", csrf.as_str()), ("consent", "yes")])
        .send()
        .await
        .map_err(provider::network_error)?;
    if result.status() != 303 {
        return Err(message(format!(
            "Account deletion returned {}",
            result.status()
        )));
    }
    if !aws.query(&pk, "row#")?.is_empty() {
        return Err(message("Account rows survived deletion."));
    }
    for key in &s3 {
        if aws.s3("GET", key, vec![]).is_ok() {
            return Err(message("Private object survived deletion."));
        }
    }
    aws.delete(&pk, "control")?;
    aws.delete(&format!("session#{}", hash(&sid)), "session")?;
    std::fs::write(if model_test {".deploy/cloud-model-smoke.json"}else{".deploy/cloud-smoke.json"},serde_json::to_vec_pretty(&json!({"synthetic":true,"duration_seconds":started.elapsed().as_secs_f64(),"durable_session":true,"private_s3_roundtrip":true,"origin_denial":true,"stream_worker":true,"persisted_tracks":2,"prompt_history":true,"account_deletion":true,"stale_lease_denied":true,"duplicate_job_ignored":true,"hosted_model_checked":model_test,"external_provider_data":if model_test{"synthetic request/artist only"}else{"none"}}))?).map_err(|_|message("Cannot write smoke receipt."))?;
    println!(
        "Cloud acceptance passed: session, S3, origin, worker, result/history, duplicate delivery, stale lease and deletion. Synthetic data only."
    );
    Ok(())
}
