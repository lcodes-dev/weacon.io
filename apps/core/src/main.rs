//! The core: the program on a person's own machine that pairs with their Weacon, stays
//! connected to it, and runs their agents there on their own sign-in.

mod agents;
mod chats;
mod link;
mod service;
mod setup;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const USAGE: &str = "\
weacon-core pair <server> <code>   pair this machine with your Weacon, from its Machines page
weacon-core                        connect, and stay connected
weacon-core status                 which Weacon this machine is paired with
weacon-core agents                 which agents are installed and signed in here
weacon-core install                start with this computer and run in the background
weacon-core uninstall              undo that
weacon-core add <user@host>        set up a server of yours over SSH and pair it";

/// Which Weacon this machine is paired with, and the token it connects with.
#[derive(Deserialize, Serialize)]
struct Paired {
    server: String,
    token: String,
}

/// The folder the core keeps its one file in.
fn home() -> PathBuf {
    let chosen = std::env::var_os("WEACON_CORE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    chosen.unwrap_or_else(|| home.join(".config/weacon-core"))
}

fn paired() -> Option<Paired> {
    let text = std::fs::read_to_string(home().join("core.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether what is sent to a server's address is encrypted on its way, or never leaves this
/// computer. The token is sent with every connection, so nothing else will do.
fn private(server: &str) -> bool {
    let Some(address) = server.strip_prefix("http://") else {
        return server.starts_with("https://");
    };
    // The host ends where a path begins, or a port: after the bracket of an IPv6 address.
    let host = address.split('/').next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "::1"
}

const PLAIN: &str = "Weacon's address must start with https://, so that this machine's token can't be read on its way. Only an address on this computer itself may be plain.";

/// What a command printed, when it ran and ended well.
fn printed(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    output.status.success().then_some(text)
}

/// Ends with what went wrong, said to the person at the terminal.
fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn needs_pairing() -> Paired {
    let how =
        "This machine isn't paired. Open Machines in your Weacon and run the command it shows.";
    let paired = paired().unwrap_or_else(|| fail(how));
    if !private(&paired.server) {
        fail(PLAIN);
    }
    paired
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["pair", server, code] => setup::pair(server, code).await,
        [] => link::stay_connected(needs_pairing()).await,
        ["status"] => match paired() {
            Some(paired) => println!("Paired with {}", paired.server),
            None => println!("Not paired"),
        },
        ["agents"] => setup::say_agents().await,
        ["install"] => service::install(),
        ["uninstall"] => service::uninstall(),
        ["add", host] => setup::add(&needs_pairing(), host).await,
        _ => fail(USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::private;

    // No request reaches the core's own checks in the server's tests.
    #[test]
    fn only_an_address_on_this_computer_may_be_plain() {
        for server in [
            "https://weacon.example.com",
            "http://localhost:8080",
            "http://127.0.0.1",
            "http://[::1]:8080/",
            "http://dev.localhost:5173",
        ] {
            assert!(private(server), "{server}");
        }
        for server in [
            "http://weacon.example.com",
            "http://localhost.example.com",
            "http://127.0.0.1.example.com:80",
            "http://10.0.0.5:8080",
            "ws://localhost",
        ] {
            assert!(!private(server), "{server}");
        }
    }
}
