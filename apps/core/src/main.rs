//! `weacon`: the core's daemon and command line.

mod app;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the chat engine, which uses it, comes next")
)]
mod chats;
mod cli;
mod daemon;
mod disk;
mod doctor;
mod guard;
mod map;
mod memory;
mod native_host;
mod picks;
mod platform;
mod reports;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the chat engine, which uses it, comes next")
)]
mod runs;
mod service;
mod session;
mod status;
#[cfg(test)]
mod test_support;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "`weacon update`, which uses it, comes next")
)]
mod update;
mod workspaces;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use weacon_observability::{Fault, json_lines};

use crate::app::{App, Setup};
use crate::cli::Command;
use weacon_appkit::Outcome;

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
            deliver: weacon_appkit::http(),
            verbose: cli.prints_info(io::stderr().is_terminal()),
            mode: cli.command.name(),
            log_name: cli.command.log_name(),
        },
    );
    std::panic::set_hook(app.panic_hook());
    let result = run(&app, cli.command, &env);
    app.finish(result)
}

fn run(app: &App, command: Command, env: platform::Env<'_>) -> Result<Outcome, Fault> {
    match command {
        Command::Daemon => daemon::run(app).map(Outcome::from),
        Command::Status => status::run(app, &mut io::stdout().lock()).map(Outcome::from),
        Command::Doctor => doctor::run(app, &mut io::stdout().lock()).map(Outcome::from),
        Command::Service(command) => {
            service::run(app, command, env, &mut service::system).map(Outcome::from)
        }
        Command::Login { agent, workspace } => {
            workspaces::login(app, agent, &workspace).map(Outcome::from)
        }
        Command::Access {
            workspace,
            level,
            team,
        } => workspaces::access::run(app, &workspace, level, team, &mut io::stdout().lock())
            .map(Outcome::from),
        Command::Guard(command) => guard::run(
            app,
            command,
            env,
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        )
        .map(Outcome::from),
        Command::Reports { answer } => {
            reports::run(app, answer, &mut io::stdout().lock()).map(Outcome::from)
        }
        Command::ConnectChrome => {
            native_host::connect_chrome(app, env, &mut io::stdout().lock()).map(Outcome::from)
        }
        Command::NativeHost { origin } => native_host::run(app, &origin).map(Outcome::from),
        Command::Map(command) => map::run(
            app,
            command,
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        ),
    }
}
