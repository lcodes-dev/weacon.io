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
}

impl Command {
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Status => "status",
            Self::Doctor => "doctor",
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
        let verbose = run(&["weacon", "doctor", "--verbose"]).unwrap();

        assert_eq!(doctor.command, Command::Doctor);
        assert_eq!(names, ["daemon", "status", "doctor"]);
        assert!(!doctor.verbose);
        assert!(verbose.verbose);
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
