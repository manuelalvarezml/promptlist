//! Explicit, bounded two-request model acceptance using synthetic prompts only.
use promptlist::{Result, config::Config, message, preferences, provider};
use serde_json::json;
#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load(&std::env::current_dir().unwrap())?;
    let key = config
        .openai_key
        .ok_or_else(|| message("OpenAI key unavailable."))?;
    let http = provider::http_client()?;
    let mut evidence = vec![];
    let mut previous = None;
    for turn in 0..2 {
        let prompt = if turn == 0 {
            "weird kinda energetic music to dedicate to a girl you love".into()
        } else {
            json!({"original_request":"weird kinda energetic music to dedicate to a girl you love","previous_preferences":previous,"latest_refinement":"make it calmer but keep the experimental and romantic feeling, 12 tracks"}).to_string()
        };
        let mut body = preferences::request_body(&prompt);
        body["model"] = json!(config.openai_model);
        let start = std::time::Instant::now();
        let raw: serde_json::Value = if std::env::args().any(|v| v == "--curl") {
            use std::io::Write;
            std::fs::create_dir_all(".deploy").map_err(|_| message("Cannot prepare fixture."))?;
            std::fs::write(".deploy/model-request.json", serde_json::to_vec(&body)?)
                .map_err(|_| message("Cannot prepare fixture."))?;
            let mut child = std::process::Command::new("curl")
                .args([
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "45",
                    "--config",
                    "-",
                    "--data-binary",
                    "@.deploy/model-request.json",
                    "https://api.openai.com/v1/responses",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|_| message("Curl unavailable."))?;
            let auth = serde_json::to_string(&format!("Authorization: Bearer {key}"))?;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(
                    format!("header = {auth}\nheader = \"Content-Type: application/json\"\n")
                        .as_bytes(),
                )
                .map_err(|_| message("Curl configuration failed."))?;
            let output = child
                .wait_with_output()
                .map_err(|_| message("Curl failed."))?;
            if !output.status.success() {
                return Err(message("Model transport failed."));
            }
            serde_json::from_slice(&output.stdout)?
        } else {
            let reply = http
                .post("https://api.openai.com/v1/responses")
                .bearer_auth(&key)
                .json(&body)
                .timeout(std::time::Duration::from_secs(45))
                .send()
                .await
                .map_err(provider::network_error)?;
            if !reply.status().is_success() {
                return Err(message(format!(
                    "Model acceptance returned HTTP {}",
                    reply.status().as_u16()
                )));
            }
            reply
                .json()
                .await
                .map_err(|_| message("Invalid model acceptance response."))?
        };
        let plan = preferences::parse_response(&raw)?;
        if turn == 1 && plan.limit != 12 {
            return Err(message("Refinement count did not carry through."));
        }
        evidence.push(json!({"turn":turn,"model":raw["model"],"elapsed_ms":start.elapsed().as_millis(),"usage":raw["usage"],"preferences":plan}));
        previous = Some(plan);
    }
    std::fs::create_dir_all(".deploy").map_err(|_| message("Cannot create acceptance output."))?;
    std::fs::write(
        ".deploy/model-smoke.json",
        serde_json::to_vec_pretty(&evidence)?,
    )
    .map_err(|_| message("Cannot save acceptance output."))?;
    println!("Two prompt-only structured-output calls passed; receipt saved privately.");
    Ok(())
}
