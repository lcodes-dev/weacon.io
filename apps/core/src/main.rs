//! `weacon`: the core's daemon and command line.

mod app;
mod cli;
mod daemon;
mod doctor;
mod logs;
mod platform;
mod report;
mod service;
mod session;
mod status;
#[cfg(test)]
mod test_support;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use weacon_observability::json_lines;

use crate::app::{App, Setup};
use crate::cli::Command;

fn main() -> ExitCode {
    let version = weacon_version::version!();
    let cli = match cli::parse(std::env::args_os(), &version) {
        Ok(cli) => cli,
        Err(error) => {
            drop(error.print());
            return u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from);
        }
    };
    let env = |key: &str| std::env::var_os(key);
    let app = App::start(
        version,
        Setup {
            env: &env,
            stderr: json_lines(io::stderr()),
            deliver: report::http(),
            verbose: cli.prints_info(io::stderr().is_terminal()),
            mode: cli.command.name(),
            log_name: cli.command.log_name(),
        },
    );
    std::panic::set_hook(app.panic_hook());
    let result = match cli.command {
        Command::Daemon => daemon::run(&app),
        Command::Status => status::run(&app, &mut io::stdout().lock()),
        Command::Doctor => doctor::run(&app, &mut io::stdout().lock()),
        Command::Service { action } => std::env::current_exe()
            .map_err(service::no_exe)
            .and_then(|exe| service::run(action, &env, &exe, &app.log, &mut service::system)),
    };
    app.finish(result)
}
