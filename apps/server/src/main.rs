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
mod links;
mod outbox;
mod pairing;
mod relay;
mod secret;
mod shared;
mod sign_in;
mod sign_up;
mod socket;
mod tasks;
#[cfg(test)]
mod test_support;
mod uplink;
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
use weacon_appkit::{App, Ending};
use weacon_observability::{Fault, Logger, json_lines};
use weacon_protocol::code;

use crate::cli::{Command, Serve};
use crate::db::Db;
use crate::outbox::Outbox;
use crate::secret::Secret;
use crate::shared::Shared;

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
    let app = app::start(
        version,
        logs,
        weacon_appkit::deliver_over_https(),
        dsn.as_deref(),
    );
    std::panic::set_hook(app.panic_hook(app::panicked));
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
    app.finish(result.map(Ending::from))
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
    let signals = signals()?;
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
        sockets: socket::TIMING,
        changes: broadcast::channel(CHANGES).0,
        unpaired: broadcast::channel(CHANGES).0,
        links: Arc::default(),
        relay: Arc::default(),
        sign_ins: Arc::new(Semaphore::new(sign_in::AT_ONCE)),
        finish_takes: sign_in::FINISH_TAKES,
        sign_in: Arc::new(sign_in::limits()),
        pairing: Arc::new(pairing::limits()),
        web: web::FILES,
    };
    http::serve(listener, shared, signals.wait()).await;
    db.close().await;
    Ok(())
}

fn signals() -> Result<weacon_appkit::Signals, Fault> {
    weacon_appkit::Signals::new().map_err(|error| {
        Fault::new(
            code!("server.signals_failed"),
            "The server couldn't catch signals",
        )
        .because(error)
    })
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
