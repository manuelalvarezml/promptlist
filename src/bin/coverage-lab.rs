#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().skip(1).collect::<Vec<_>>() == ["--public-cache-probe"] {
        let root = std::env::current_dir().expect("working directory");
        match promptlist::lab::public_cache_probe(&root).await {
            Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
            Err(e) => {
                eprintln!("Public probe stopped: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--live"] {
        eprintln!(
            "Usage: cargo run --bin coverage-lab -- --live\nSends ISRCs for up to 200 baseline misses to ReccoBeats, then artist/title for at most 40 baseline tracks to Last.fm. No LLM, audio or paid requests. Saves aggregate receipts only; catalog and coverage are unchanged."
        );
        std::process::exit(2);
    }
    let root = std::env::current_dir().expect("working directory");
    match promptlist::lab::run_coverage(&root).await {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&promptlist::lab::summary(&report)).unwrap()
            );
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Experiment stopped: {e}");
            std::process::exit(1);
        }
    }
}
