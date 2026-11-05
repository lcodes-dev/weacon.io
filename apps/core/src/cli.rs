use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum ServiceCommand {
    /// Install, enable and start the service, and let it run with nobody logged in.
    Install,
    /// Stop the service and remove it.
    Uninstall,
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
            Self::Status | Self::Doctor | Self::Service(_) => "cli.log",
        }
    }

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Status => "status",
            Self::Doctor => "doctor",
            Self::Service(_) => "service",
        }
    }
}

/// `0.1.0 (1a2b3c4)`
pub(crate) fn version_text(version: &Version) -> String {
    format!("{} ({})", version.version, version.commit)
}

/// The command line, or the help, version or error text clap would print instead.
pub(crate) fn parse<I: IntoIterator<Item = OsString>>(
    args: I,
    version: &Version,
) -> Result<Cli, clap::Error> {
    let matches = Cli::command()
        .version(version_text(version))
        .try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;
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
        let names = ["daemon", "status", "doctor"]
            .map(|name| run(&["weacon", name]).unwrap().command.name());
        let install = run(&["weacon", "service", "install"]).unwrap().command;
        let uninstall = run(&["weacon", "service", "uninstall"]).unwrap().command;
        let verbose = run(&["weacon", "doctor", "--verbose"]).unwrap();

        assert_eq!(doctor.command, Command::Doctor);
        assert_eq!(names, ["daemon", "status", "doctor"]);
        assert_eq!(
            [install, uninstall],
            [ServiceCommand::Install, ServiceCommand::Uninstall].map(Command::Service)
        );
        let service = run(&["weacon", "service", "install"]).unwrap().command;
        assert_eq!((service.name(), service.log_name()), ("service", "cli.log"));
        assert!(!doctor.verbose);
        assert!(verbose.verbose);
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
