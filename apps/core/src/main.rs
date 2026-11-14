//! `weacon`: the core's daemon and command line.

mod app;
mod chats;
mod cli;
mod daemon;
mod disk;
mod doctor;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "chats start their agents in worktrees once wired in"
    )
)]
mod git;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the server's uplink, which uses it, comes with pairing"
    )
)]
mod machine_settings;
mod map;
mod mcp;
mod memory;
mod names;
mod native_host;
mod picks;
mod platform;
mod reports;
mod runs;
mod service;
mod session;
mod status;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the sign-in terminal, which uses it, comes next")
)]
mod terminal;
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

use weacon_appkit::Ending;
use weacon_observability::{Fault, json_lines};

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
            deliver: weacon_appkit::deliver_over_https(),
            verbose: cli.prints_info(io::stderr().is_terminal()),
            mode: cli.command.name(),
            log_name: cli.command.log_name(),
        },
    );
    std::panic::set_hook(app.panic_hook(app::panicked));
    let result = run(&app, cli.command, &env);
    app.finish(result)
}

fn run(app: &App, command: Command, env: platform::Env<'_>) -> Result<Ending, Fault> {
    let done = match command {
        Command::Daemon => daemon::run(app),
        Command::Status => status::run(app, &mut io::stdout().lock()),
        Command::Doctor => doctor::run(app, &mut io::stdout().lock()),
        Command::Service(command) => service::run(app, command, env, &mut service::system),
        Command::Login { agent, workspace } => workspaces::login(app, agent, &workspace),
        Command::Access {
            workspace,
            level,
            team,
        } => workspaces::access(app, &workspace, level, team, &mut io::stdout().lock()),
        Command::Reports { answer } => reports::run(app, answer, &mut io::stdout().lock()),
        Command::ConnectChrome => native_host::connect_chrome(app, env, &mut io::stdout().lock()),
        Command::NativeHost { origin } => native_host::run(app, &origin),
        Command::Map(command) => {
            return map::run(
                app,
                command,
                &mut io::stdin().lock(),
                &mut io::stdout().lock(),
            );
        }
    };
    done.map(Ending::from)
}
