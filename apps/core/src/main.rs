//! `weacon`: the core's daemon and command line.

mod app;
mod cli;
mod daemon;
mod disk;
mod doctor;
mod logs;
mod native_host;
mod picks;
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
        Command::Service(command) => service::run(&app, command, &env, &mut service::system),
        Command::ConnectChrome => native_host::connect_chrome(&app, &env, &mut io::stdout().lock()),
        Command::NativeHost { origin } => native_host::run(&app, &origin),
    };
    app.finish(result)
}
