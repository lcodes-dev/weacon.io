use std::sync::Arc;

use tracing::info;
use tracing_subscriber::EnvFilter;
use weacon_server::mail::Mailer;
use weacon_server::{AppState, db, map, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let levels = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(levels).init();

    let addr = env_or("WEACON_ADDR", "127.0.0.1:8080");
    let url = env_or("WEACON_URL", "http://localhost:8080");
    let url = url.trim_end_matches('/').to_owned();
    let pool = db::open(&env_or("WEACON_DB", "weacon.db")).await?;
    let smtp = db::smtp(&pool).await?;
    match &smtp {
        Some(smtp) => info!("mail goes through {}", smtp.host),
        None => info!("mail is printed to this log until the owner saves SMTP settings"),
    }
    let mailer = Arc::new(Mailer::default());
    mailer.set(smtp.as_ref())?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("listening on http://{addr}");
    let state = AppState {
        pool,
        mailer,
        url,
        github_api: "https://api.github.com".into(),
        map_wake: Arc::default(),
    };
    tokio::spawn(map::worker(state.clone()));
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
