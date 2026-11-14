use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use weacon_protocol::{AccessLevel, AgentKind, WorkspaceId};
use weacon_version::Version;

/// Weacon's core: runs agents for your chats and tasks.
#[derive(Debug, Parser)]
#[command(name = "weacon")]
pub(crate) struct Cli {
    /// Also print info logs.
    #[arg(long, global = true)]
    pub(crate) verbose: bool,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum Command {
    /// Run the daemon that clients connect to, until Ctrl-C or SIGTERM.
    Daemon,
    /// Check that the daemon is running and print its build.
    Status,
    /// Check this install and print what it found.
    Doctor,
    /// Run the daemon as a systemd user service that keeps running after logout.
    #[command(subcommand)]
    Service(ServiceCommand),
    /// Sign an agent in for a workspace, with the vendor's own sign-in in this terminal.
    Login {
        /// The agent: claude or codex.
        #[arg(value_parser = agent)]
        agent: AgentKind,
        /// The workspace's id.
        #[arg(long)]
        workspace: WorkspaceId,
    },
    /// Choose what a workspace's agents may do on this machine.
    Access {
        /// The workspace's id.
        workspace: WorkspaceId,
        /// What its agents may do: off, read-only, ask-first or full.
        #[arg(value_parser = crate::workspaces::parse_level)]
        level: AccessLevel,
        /// Whether other people are in the workspace; kept as it was when left out.
        #[arg(long)]
        team: Option<bool>,
    },
    /// Choose whether this core sends error reports, or show the answer.
    Reports {
        /// on or off; left out, shows the answer.
        #[arg(value_enum)]
        answer: Option<crate::reports::Answer>,
    },
    /// Let Weacon's Chrome extension reach this core.
    ConnectChrome,
    /// Relay messages between Chrome's extension and the daemon; Chrome starts it.
    #[command(hide = true)]
    NativeHost { origin: String },
    /// Print where to begin in a repository's code, or work with its code map.
    Map(MapCommand),
}

fn agent(name: &str) -> Result<AgentKind, &'static str> {
    serde_json::from_value(name.into())
        .ok()
        .filter(|agent| *agent != AgentKind::Newer)
        .ok_or("not an agent this core runs")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum ServiceCommand {
    /// Install, enable and start the service, and let it run with nobody logged in.
    Install,
    /// Stop the service and remove it.
    Uninstall,
}

/// Without a subcommand, prints the text the MCP's `map` tool answers.
#[derive(Debug, PartialEq, Eq, Args)]
pub(crate) struct MapCommand {
    #[command(subcommand)]
    pub(crate) read: Option<MapRead>,
    /// The repository's root folder.
    #[arg(long, global = true, default_value = ".")]
    pub(crate) root: PathBuf,
    /// The most bytes an answer may hold.
    #[arg(long, global = true)]
    pub(crate) budget: Option<u32>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum MapRead {
    /// Print the text the MCP's `symbol` tool answers for an item's full path.
    Symbol { path: String },
    /// Read pinned anchors on stdin, one per line, and print those that no longer match.
    CheckAnchors,
}

impl Cli {
    /// Whether info logs reach stderr: with `--verbose`, or for a daemon started in a terminal.
    pub(crate) fn prints_info(&self, stderr_is_terminal: bool) -> bool {
        self.verbose || (self.command == Command::Daemon && stderr_is_terminal)
    }
}

impl Command {
    /// The daemon and short commands each rotate their own log file.
    pub(crate) const fn log_name(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon.log",
            Self::Status
            | Self::Doctor
            | Self::Service(_)
            | Self::Login { .. }
            | Self::Access { .. }
            | Self::Reports { .. }
            | Self::ConnectChrome
            | Self::Map(_) => "cli.log",
            Self::NativeHost { .. } => "native-host.log",
        }
    }

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Status => "status",
            Self::Doctor => "doctor",
            Self::Service(_) => "service",
            Self::Login { .. } => "login",
            Self::Access { .. } => "access",
            Self::Reports { .. } => "reports",
            Self::ConnectChrome => "connect-chrome",
            Self::NativeHost { .. } => "native-host",
            Self::Map(_) => "map",
        }
    }
}

/// The command line, or the help, version or error text clap would print instead.
pub(crate) fn parse<I: IntoIterator<Item = OsString>>(
    args: I,
    version: &Version,
) -> Result<Cli, clap::Error> {
    let matches = Cli::command()
        .version(version.with_commit())
        .try_get_matches_from(from_chrome(args.into_iter().collect()))?;
    Cli::from_arg_matches(&matches)
}

// Chrome starts a native host with the extension's origin as its first argument, and on
// Windows a window handle after it.
fn from_chrome(mut args: Vec<OsString>) -> Vec<OsString> {
    let started_by_chrome = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| arg.starts_with("chrome-extension://"));
    if started_by_chrome {
        args.truncate(2);
        args.insert(1, "native-host".into());
    }
    args
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;
    use crate::reports::Answer;
    use crate::test_support::VERSION;

    fn run(args: &[&str]) -> Result<Cli, clap::Error> {
        parse(args.iter().map(OsString::from), &VERSION)
    }

    #[test]
    fn version_prints_the_version_and_commit() {
        let error = run(&["weacon", "--version"]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(error.to_string(), "weacon 0.1.0 (1a2b3c4)\n");
    }

    #[test]
    fn commands_and_flags_parse() {
        let doctor = run(&["weacon", "doctor"]).unwrap();
        let names = ["daemon", "status", "doctor", "connect-chrome"]
            .map(|name| run(&["weacon", name]).unwrap().command.name());
        let install = run(&["weacon", "service", "install"]).unwrap().command;
        let uninstall = run(&["weacon", "service", "uninstall"]).unwrap().command;
        let map = run(&["weacon", "map", "check-anchors"]).unwrap().command;
        let rooted = run(&["weacon", "map", "check-anchors", "--root", "/srv/repo"]).unwrap();
        let outline = run(&["weacon", "map", "--budget", "2000"]).unwrap().command;
        let symbol = run(&["weacon", "map", "symbol", "a::b", "--root", "r"])
            .unwrap()
            .command;
        let verbose = run(&["weacon", "doctor", "--verbose"]).unwrap();

        assert_eq!(doctor.command, Command::Doctor);
        assert_eq!(names, ["daemon", "status", "doctor", "connect-chrome"]);
        assert_eq!(
            [install, uninstall],
            [ServiceCommand::Install, ServiceCommand::Uninstall].map(Command::Service)
        );
        let service = run(&["weacon", "service", "install"]).unwrap().command;
        assert_eq!((service.name(), service.log_name()), ("service", "cli.log"));
        let map_of = |read, root: &str, budget| {
            Command::Map(MapCommand {
                read,
                root: root.into(),
                budget,
            })
        };
        assert_eq!(map, map_of(Some(MapRead::CheckAnchors), ".", None));
        assert_eq!((map.name(), map.log_name()), ("map", "cli.log"));
        assert_eq!(
            rooted.command,
            map_of(Some(MapRead::CheckAnchors), "/srv/repo", None)
        );
        assert_eq!(outline, map_of(None, ".", Some(2000)));
        let path = "a::b".to_owned();
        assert_eq!(symbol, map_of(Some(MapRead::Symbol { path }), "r", None));
        assert!(!doctor.verbose);
        assert!(verbose.verbose);
    }

    #[test]
    fn login_takes_a_known_agent_and_a_workspace_id() {
        let login = |agent| run(&["weacon", "login", agent, "--workspace", "w-1"]).unwrap();

        for (name, kind) in [("claude", AgentKind::Claude), ("codex", AgentKind::Codex)] {
            let login = login(name);
            let Command::Login { agent, workspace } = &login.command else {
                panic!("not a login");
            };
            assert_eq!(*agent, kind);
            assert_eq!(workspace.as_str(), "w-1");
            assert_eq!(
                (login.command.name(), login.command.log_name()),
                ("login", "cli.log")
            );
        }
        for bad in [
            &["weacon", "login", "amp", "--workspace", "w-1"][..],
            &["weacon", "login", "newer", "--workspace", "w-1"],
            &["weacon", "login", "Claude", "--workspace", "w-1"],
            &["weacon", "login", "claude", "--workspace", "../w"],
            &["weacon", "login", "claude"],
        ] {
            run(bad).unwrap_err();
        }
    }

    #[test]
    fn access_takes_a_workspace_a_level_and_whether_its_a_team() {
        let levels = ["off", "read-only", "ask-first", "full"].map(|level| {
            let access = run(&["weacon", "access", "w-1", level]).unwrap().command;
            let Command::Access { level, team, .. } = access else {
                panic!("not an access command");
            };
            assert_eq!(team, None);
            level
        });
        let team = run(&["weacon", "access", "w-1", "full", "--team", "true"]).unwrap();
        let left = run(&["weacon", "access", "w-1", "full", "--team", "false"]).unwrap();
        for bad in ["read_only", "Full", "newer", "on", ""] {
            run(&["weacon", "access", "w-1", bad]).unwrap_err();
        }

        assert_eq!(
            levels,
            [
                AccessLevel::Off,
                AccessLevel::ReadOnly,
                AccessLevel::AskFirst,
                AccessLevel::Full
            ]
        );
        assert!(matches!(
            &team.command,
            Command::Access { workspace, team: Some(true), .. } if workspace.as_str() == "w-1"
        ));
        assert!(matches!(
            left.command,
            Command::Access {
                team: Some(false),
                ..
            }
        ));
        assert_eq!(
            (team.command.name(), team.command.log_name()),
            ("access", "cli.log")
        );
        for bad in [
            &["weacon", "access", "w-1", "newer"][..],
            &["weacon", "access", "a b", "full"],
            &["weacon", "access", "w-1"],
        ] {
            run(bad).unwrap_err();
        }
    }

    #[test]
    fn reports_takes_on_off_or_nothing() {
        let answer = |args: &[&str]| {
            let Command::Reports { answer } = run(args).unwrap().command else {
                panic!("not a reports command");
            };
            answer
        };
        let shown = run(&["weacon", "reports"]).unwrap().command;

        assert_eq!(answer(&["weacon", "reports"]), None);
        assert_eq!(answer(&["weacon", "reports", "on"]), Some(Answer::On));
        assert_eq!(answer(&["weacon", "reports", "off"]), Some(Answer::Off));
        assert_eq!((shown.name(), shown.log_name()), ("reports", "cli.log"));
        for bad in ["yes", "On", "true", ""] {
            run(&["weacon", "reports", bad]).unwrap_err();
        }
    }

    #[test]
    fn the_daemon_logs_to_its_terminal_and_its_own_file() {
        let daemon = run(&["weacon", "daemon"]).unwrap();
        let status = run(&["weacon", "status"]).unwrap();
        let verbose = run(&["weacon", "status", "--verbose"]).unwrap();

        assert!(daemon.prints_info(true));
        assert!(!daemon.prints_info(false));
        assert!(!status.prints_info(true));
        assert!(verbose.prints_info(false));
        assert_eq!(daemon.command.log_name(), "daemon.log");
        assert_eq!(status.command.log_name(), "cli.log");
        assert_eq!(
            run(&["weacon", "doctor"]).unwrap().command.log_name(),
            "cli.log"
        );
    }

    #[test]
    fn chrome_starts_the_native_host_with_its_origin() {
        let origin = "chrome-extension://phnfnmnpncfhkhlkmglnlhlkaaociaij/";
        let host = run(&["weacon", origin, "--parent-window=0"]).unwrap();

        assert_eq!(
            host.command,
            Command::NativeHost {
                origin: origin.to_owned()
            }
        );
        assert_eq!(host.command.name(), "native-host");
        assert_eq!(host.command.log_name(), "native-host.log");
        assert!(!host.prints_info(true));
        assert_eq!(
            run(&["weacon", "native-host", origin]).unwrap().command,
            host.command
        );
    }

    #[test]
    fn a_missing_or_unknown_command_is_an_error() {
        assert_eq!(
            run(&["weacon"]).unwrap_err().kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(
            run(&["weacon", "fly"]).unwrap_err().kind(),
            ErrorKind::InvalidSubcommand
        );
    }
}
