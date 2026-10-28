use promptlist::{Result, message, text_evidence};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    time::{Duration, Instant},
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--live"] {
        eprintln!(
            "Usage: public-text-smoke --live. One paid OpenAI response, up to three search calls, 2000 output tokens; three fixed public tracks, no user-library input."
        );
        std::process::exit(2);
    }
    if let Err(e) = run().await {
        eprintln!("Public text smoke: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let mut values = HashMap::new();
    for entry in
        dotenvy::from_path_iter(".env.local").map_err(|_| message("Missing local configuration"))?
    {
        let (k, v) = entry.map_err(|_| message("Invalid local configuration"))?;
        if values.insert(k, v).is_some() {
            return Err(message("Duplicate local setting"));
        }
    }
    let key = values
        .get("OPENAI_API_KEY")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| message("OPENAI_API_KEY required"))?;
    // Fixed model for repeatability and bounded costs; never auto-upgrade on error.
    let model = "gpt-5.4-mini";
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| message("HTTP initialization failed"))?;
    let start = Instant::now();
    let response = client
        .post("https://api.openai.com/v1/responses")
        .bearer_auth(key)
        .json(&text_evidence::public_request(model))
        .send()
        .await
        .map_err(|e| {
            message(format!(
                "OpenAI {} after {} ms; no automatic retry; provider billing outcome unknown",
                if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connection failure"
                } else {
                    "transport failure"
                },
                start.elapsed().as_millis()
            ))
        })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(message(format!(
            "OpenAI HTTP {status}; no response body logged and no automatic retry"
        )));
    }
    if response.content_length().is_some_and(|n| n > 2_000_000) {
        return Err(message("Response too large"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| message("Response interrupted"))?;
    if bytes.len() > 2_000_000 {
        return Err(message("Response too large"));
    }
    let body: Value = serde_json::from_slice(&bytes)?;
    let mut report = text_evidence::inspect_response(&body)?;
    report["elapsed_ms"] = json!(start.elapsed().as_millis());
    report["observed_at"] = json!(promptlist::now());
    let path = format!("research/PUBLIC_TEXT_SMOKE_{}.json", promptlist::now());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|_| message("Cannot save public experiment receipt"))?;
    file.write_all(serde_json::to_string_pretty(&report)?.as_bytes())
        .map_err(|_| message("Cannot write experiment receipt"))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    println!("Saved {path}; claims require source review, not ranking-ready facts.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "Explicit opt-in paid public-fixture request; no library input"]
    async fn live_public_web() {
        if let Err(error) = super::run().await {
            panic!("{error}");
        }
    }
}
