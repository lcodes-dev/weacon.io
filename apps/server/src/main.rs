//! `weacon-server`: the server that holds every workspace.

mod admin;
mod app;
mod auth;
mod cli;
mod client;
mod connections;
mod db;
mod docs;
mod email;
mod feed;
mod http;
mod limit;
mod outbox;
mod secret;
mod sign_in;
mod sign_up;
mod tasks;
#[cfg(test)]
mod test_support;
mod web;
mod workspaces;

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast};
use weacon_observability::{Fault, Logger, json_lines};
use weacon_protocol::code;

use crate::app::App;
use crate::cli::{Command, Serve};
use crate::db::Db;
use crate::http::Shared;
use crate::outbox::Outbox;
use crate::secret::Secret;

/// How many wake-ups a feed may miss before it rereads the database.
const CHANGES: usize = 256;

fn main() -> ExitCode {
    let version = weacon_version::version!();
    let cli = match cli::parse(std::env::args_os(), &version) {
        Ok(cli) => cli,
        Err(error) => {
            drop(error.print());
            return u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from);
        }
    };
    let dsn = std::env::var("WEACON_SENTRY_DSN")
        .ok()
        .filter(|dsn| !dsn.is_empty());
    // The server logs to stdout; admin commands print their output there instead.
    let logs = match cli.command {
        Command::Serve(_) => json_lines(io::stdout()),
        Command::Admin(_) => json_lines(io::stderr()),
    };
    let app = App::start(version, logs, weacon_appkit::http(), dsn.as_deref());
    std::panic::set_hook(app.panic_hook());
    let runtime = weacon_appkit::runtime(tokio::runtime::Builder::new_multi_thread());
    let result = runtime.and_then(|runtime| {
        runtime.block_on(async {
            match cli.command {
                Command::Serve(args) => serve(&app, args).await,
                Command::Admin(command) => {
                    admin::run(command, &app::new_id, &mut io::stdout()).await
                }
            }
        })
    });
    app.finish(result)
}

/// Serves until Ctrl-C or the system asks the server to stop.
async fn serve(app: &App, args: Serve) -> Result<(), Fault> {
    let Serve {
        listen,
        db,
        outbox,
        secret,
        max_connections,
        trusted_proxy,
        sign_up,
    } = args;
    usable_outbox(outbox.clone()).await?;
    let signals = weacon_appkit::Signals::new().map_err(|error| {
        Fault::new(
            code!("server.signals_failed"),
            "The server couldn't catch signals",
        )
        .because(error)
    })?;
    let listener = bind(listen).await?;
    let secret = open_secret(secret, app.log.clone()).await?;
    let db = Db::create(&db).await?;
    let shared = Shared {
        app: weacon_appkit::build(&app.version),
        log: app.log.clone(),
        reporter: Arc::clone(&app.reporter),
        db: db.clone(),
        outbox: Outbox::new(outbox),
        secret: Arc::new(secret),
        clock: Arc::new(Box::new(SystemTime::now)),
        head_within: connections::HEAD_WITHIN,
        proxies: trusted_proxy.into(),
        sign_up: sign_up.into(),
        connections: Arc::new(Semaphore::new(max_connections)),
        feed: feed::TIMING,
        changes: broadcast::channel(CHANGES).0,
        sign_ins: Arc::new(Semaphore::new(sign_in::AT_ONCE)),
        finish_within: sign_in::FINISH_WITHIN,
        starts: Arc::new(sign_in::starts()),
        tries: Arc::new(sign_in::tries()),
        web: web::FILES,
    };
    http::serve(listener, shared, signals.wait()).await;
    db.close().await;
    Ok(())
}

/// Checks that `outbox` is a folder; otherwise every sign-in would fail after its answer, and
/// only the logs would say so.
async fn usable_outbox(outbox: PathBuf) -> Result<(), Fault> {
    let usable = tokio::task::spawn_blocking(move || outbox.is_dir())
        .await
        .map_err(|error| app::panicked().because(error))?;
    if usable {
        return Ok(());
    }
    Err(Fault::new(
        code!("server.outbox_unusable"),
        "The outbox folder doesn't exist",
    ))
}

async fn open_secret(path: PathBuf, log: Logger) -> Result<Secret, Fault> {
    tokio::task::spawn_blocking(move || Secret::open(&path, &log))
        .await
        .map_err(|error| app::panicked().because(error))?
}

async fn bind(listen: SocketAddr) -> Result<TcpListener, Fault> {
    TcpListener::bind(listen).await.map_err(|error| {
        Fault::new(
            code!("server.bind_failed"),
            "The server couldn't listen on its address",
        )
        .because(error)
    })
}
