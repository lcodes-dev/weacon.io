use std::ffi::OsString;
use std::net::SocketAddr;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use weacon_appkit::version_text;
use weacon_version::Version;

/// Weacon's server: holds every workspace.
#[derive(Debug, Parser)]
#[command(name = "weacon-server")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Serve workspaces until Ctrl-C or SIGTERM.
    Serve {
        /// The address and port to listen on.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
    },
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
        let error = run(&["weacon-server", "--version"]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(error.to_string(), "weacon-server 0.1.0 (1a2b3c4)\n");
    }

    fn listen(args: &[&str]) -> String {
        let Command::Serve { listen } = run(args).unwrap().command;
        listen.to_string()
    }

    #[test]
    fn serves_where_it_is_told() {
        assert_eq!(listen(&["weacon-server", "serve"]), "127.0.0.1:8080");
        assert_eq!(
            listen(&["weacon-server", "serve", "--listen", "0.0.0.0:9000"]),
            "0.0.0.0:9000"
        );
        run(&["weacon-server", "serve", "--listen", "nowhere"]).unwrap_err();
        run(&["weacon-server"]).unwrap_err();
    }
}
