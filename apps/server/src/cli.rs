use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::builder::RangedU64ValueParser;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use weacon_appkit::version_text;
use weacon_protocol::WorkspaceId;
use weacon_version::Version;

use crate::client::Net;

/// Weacon's server: holds every workspace.
#[derive(Debug, Parser)]
#[command(name = "weacon-server")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum Command {
    /// Serve workspaces until Ctrl-C or SIGTERM.
    Serve(Serve),
    #[command(flatten)]
    Admin(AdminCommand),
}

#[derive(Debug, PartialEq, Eq, Args)]
pub(crate) struct Serve {
    /// The address and port to listen on.
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub(crate) listen: SocketAddr,
    /// The database file, created if missing.
    #[arg(long)]
    pub(crate) db: PathBuf,
    /// The folder where sign-in codes wait, one JSON file each, for a mail relay to send.
    #[arg(long)]
    pub(crate) outbox: PathBuf,
    /// The file with the server's key for sign-in codes, made if missing and readable only by
    /// this user. To change the key, replace the file and restart: codes already sent stop
    /// working, and nobody is signed out.
    #[arg(long)]
    pub(crate) secret: PathBuf,
    /// How many connections may be open at once; more wait their turn. Keep it well under the
    /// open-file limit.
    #[arg(long, default_value_t = 512, value_parser = RangedU64ValueParser::<usize>::new().range(1..=1_000_000))]
    pub(crate) max_connections: usize,
    /// A reverse proxy in front of the server, as an address or a block such as 10.0.0.0/8;
    /// repeat it for more. Only these are believed about the client in X-Forwarded-For, where
    /// they must write bare addresses, without ports. Without any, every request counts as
    /// coming from the address that connected.
    #[arg(long)]
    pub(crate) trusted_proxy: Vec<Net>,
}

/// Commands an operator runs against the database.
#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum AdminCommand {
    /// Create a workspace and print its id.
    CreateWorkspace {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        name: String,
    },
    /// Add a member to a workspace, with an account if their address has none, and print the
    /// member's id.
    AddMember {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        workspace: WorkspaceId,
        #[arg(long)]
        email: String,
    },
    /// Print a workspace's members, one id and email per line.
    Members {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        workspace: WorkspaceId,
    },
    /// Copy the database to a new file while the server runs, and check the copy.
    Backup {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
    /// Check a backup and copy it to a new database file, with the server stopped.
    Restore {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        db: PathBuf,
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
    use crate::test_support::{VERSION, workspace_id};

    fn run(args: &[&str]) -> Result<Cli, clap::Error> {
        parse(args.iter().map(OsString::from), &VERSION)
    }

    #[test]
    fn version_prints_the_version_and_commit() {
        let error = run(&["weacon-server", "--version"]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert_eq!(error.to_string(), "weacon-server 0.1.0 (1a2b3c4)\n");
    }

    fn serve(args: &[&str]) -> Serve {
        let Command::Serve(serve) = run(args).unwrap().command else {
            panic!("not serve");
        };
        serve
    }

    #[test]
    fn serves_where_it_is_told() {
        let args = [
            "weacon-server",
            "serve",
            "--db",
            "team.db",
            "--outbox",
            "outbox",
            "--secret",
            "secret",
        ];

        assert_eq!(
            serve(&args),
            Serve {
                listen: "127.0.0.1:8080".parse().unwrap(),
                db: PathBuf::from("team.db"),
                outbox: PathBuf::from("outbox"),
                secret: PathBuf::from("secret"),
                max_connections: 512,
                trusted_proxy: Vec::new(),
            }
        );
        let told = [
            "--listen",
            "0.0.0.0:9000",
            "--max-connections",
            "2",
            "--trusted-proxy",
            "10.0.0.0/8",
            "--trusted-proxy",
            "::1",
        ];
        let told = serve(&[&args[..], &told].concat());
        assert_eq!(told.listen.to_string(), "0.0.0.0:9000");
        assert_eq!(told.max_connections, 2);
        let proxies = ["10.0.0.0/8".parse().unwrap(), "::1".parse().unwrap()];
        assert_eq!(told.trusted_proxy, proxies);
        run(&[&args[..], &["--trusted-proxy", "proxy.example"]].concat()).unwrap_err();
        run(&[&args[..], &["--max-connections", "0"]].concat()).unwrap_err();
        run(&[&args[..], &["--listen", "nowhere"]].concat()).unwrap_err();
        run(&["weacon-server", "serve", "--db", "team.db"]).unwrap_err();
        run(&["weacon-server"]).unwrap_err();
    }

    #[test]
    fn admin_commands_parse() {
        let add = run(&[
            "weacon-server",
            "add-member",
            "--db",
            "team.db",
            "--workspace",
            "w1",
            "--email",
            "ada@example.com",
        ])
        .unwrap();

        assert_eq!(
            add.command,
            Command::Admin(AdminCommand::AddMember {
                db: "team.db".into(),
                workspace: workspace_id("w1"),
                email: "ada@example.com".to_owned(),
            })
        );
        run(&["weacon-server", "restore", "--from", "a.db"]).unwrap_err();
        let bad_id = [
            "weacon-server",
            "members",
            "--db",
            "team.db",
            "--workspace",
            "a b",
        ];
        assert_eq!(run(&bad_id).unwrap_err().kind(), ErrorKind::ValueValidation);
    }
}
