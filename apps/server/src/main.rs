//! `weacon-server`: the server that holds every workspace.

mod app;
mod cli;
mod http;
#[cfg(test)]
mod test_support;

use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;

use tokio::net::TcpListener;
use weacon_observability::{Fault, json_lines};
use weacon_protocol::code;

use crate::app::App;
use crate::cli::Command;
use crate::http::Shared;

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
    let app = App::start(
        version,
        json_lines(io::stdout()),
        weacon_appkit::http(),
        dsn.as_deref(),
    );
    std::panic::set_hook(app.panic_hook());
    let result = match cli.command {
        Command::Serve { listen } => serve(&app, listen),
    };
    app.finish(result)
}

/// Serves on `listen` until Ctrl-C or the system asks the server to stop.
fn serve(app: &App, listen: SocketAddr) -> Result<(), Fault> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Fault::new(code!("runtime.failed"), "Weacon couldn't start its runtime").because(error)
        })?;
    runtime.block_on(async {
        let signals = weacon_appkit::Signals::new().map_err(|error| {
            Fault::new(
                code!("server.signals_failed"),
                "The server couldn't catch signals",
            )
            .because(error)
        })?;
        let listener = TcpListener::bind(listen).await.map_err(|error| {
            Fault::new(
                code!("server.bind_failed"),
                "The server couldn't listen on its address",
            )
            .because(error)
        })?;
        let shared = Shared {
            app: weacon_appkit::build(&app.version),
            log: app.log.clone(),
        };
        http::serve(listener, shared, signals.wait()).await
    })
}
