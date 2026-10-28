use promptlist::{
    config::Config,
    store::Store,
    web::{App, router},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Promptlist: {error}");
        std::process::exit(1);
    }
}

async fn run() -> promptlist::Result<()> {
    let root = std::env::current_dir()
        .map_err(|_| promptlist::message("Cannot determine project directory."))?;
    let config = Config::load(&root)?;
    let origin = config.origin();
    let port = config.port();
    let store = Store::open(&config.data_dir)?;
    let app = App::new(config, store)?;
    let cleanup = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            cleanup.cleanup();
        }
    });
    let listener=tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,port)).await
        .map_err(|_|promptlist::message(format!("Port {port} is unavailable. Choose a free callback port in .env.local and register the same URI in Spotify.")))?;
    println!("Promptlist is running at {origin}");
    println!(
        "Local preview and confirmed private saving. Tokens stay in memory; reconnect after restarting."
    );
    axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| promptlist::message("Local web server stopped unexpectedly."))?;
    Ok(())
}
