use std::path::{Path, PathBuf};

use tracing::info;
use tracing_subscriber::EnvFilter;
use weacon_server::settings::secrets;
use weacon_server::{AppState, db, jobs, projects, router, settings};

type Error = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let levels = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(levels).init();

    let addr = env_or("WEACON_ADDR", "127.0.0.1:8080");
    let url = env_or("WEACON_URL", "http://localhost:8080");
    let database = env_or("WEACON_DB", "weacon.db");
    let pool = db::open(&database).await?;
    let files = private_folder(&Path::new(&database).with_extension("files"))?;
    let key_file = std::env::var("WEACON_KEY_FILE").map_or_else(
        |_| Path::new(&database).with_extension("key"),
        PathBuf::from,
    );
    let server_key = secrets::load(&pool, &key_file).await?;
    match settings::db::smtp(&pool, &server_key).await? {
        Some(smtp) => info!("mail goes through {}", smtp.host),
        None => info!("mail is printed to this log until the owner saves SMTP settings"),
    }
    projects::db::forget_other_readers(&pool, weacon_server::MAP_READER).await?;

    let state = AppState {
        pool,
        url: url.trim_end_matches('/').to_owned(),
        github_api: "https://api.github.com".into(),
        anthropic_api: "https://api.anthropic.com".into(),
        cores: Default::default(),
        live: Default::default(),
        files,
        embed: doc_search::embed,
        server_key,
    };
    jobs::resume(&state).await?;
    tokio::spawn(jobs::schedule(state.clone()));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("listening on http://{addr}");
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn private_folder(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path.to_owned())
}
