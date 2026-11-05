//! `weacon`: the core's daemon and command line.

mod app;
mod cli;
mod doctor;
mod logs;
mod platform;
mod report;
#[cfg(test)]
mod test_support;

use std::io;
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
            verbose: cli.verbose,
            mode: cli.command.name(),
        },
    );
    let result = match cli.command {
        Command::Doctor => doctor::run(&app, &mut io::stdout().lock()),
    };
    app.finish(result)
}
