use tracing::info;
use tracing_subscriber::EnvFilter;
use weacon_server::{AppState, db, map, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let levels = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(levels).init();

    let addr = env_or("WEACON_ADDR", "127.0.0.1:8080");
    let url = env_or("WEACON_URL", "http://localhost:8080");
    let url = url.trim_end_matches('/').to_owned();
    let database = env_or("WEACON_DB", "weacon.db");
    let pool = db::open(&database).await?;
    let files = std::path::Path::new(&database).with_extension("files");
    std::fs::create_dir_all(&files)?;
    // Like the database, the files are for the server's account only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&files, std::fs::Permissions::from_mode(0o700))?;
    }
    match db::smtp(&pool).await? {
        Some(smtp) => info!("mail goes through {}", smtp.host),
        None => info!("mail is printed to this log until the owner saves SMTP settings"),
    }
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("listening on http://{addr}");
    let state = AppState {
        pool,
        url,
        github_api: "https://api.github.com".into(),
        map_wake: Default::default(),
        anthropic_api: "https://api.anthropic.com".into(),
        cores: Default::default(),
        live: Default::default(),
        weacon: Default::default(),
        files,
        embed: doc_search::embed,
    };
    db::fail_interrupted_docs(&state.pool).await?;
    tokio::spawn(map::worker(state.clone()));
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
