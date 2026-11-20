//! The core: the program on a person's own machine that pairs with their Weacon, stays
//! connected to it, and runs their agents there on their own sign-in.

mod agents;
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
    paired().unwrap_or_else(|| fail(how))
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
