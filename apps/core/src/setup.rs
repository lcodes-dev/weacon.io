use std::io::Write;
use std::process::Command;

use core_link::Ready;
use serde_json::{Value, json};

use crate::{PLAIN, Paired, agents, fail, home, printed, private};

const OS: &str = if cfg!(target_os = "macos") {
    "macos"
} else {
    "linux"
};

fn yes(question: &str) -> bool {
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok();
    matches!(answer.trim(), "y" | "Y" | "yes")
}

async fn post(address: &str, token: Option<&str>, body: Value) -> Value {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    let client = client.build().unwrap_or_else(|error| fail(error));
    let request = client.post(address).json(&body);
    let request = match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    let answer = match request.send().await {
        Ok(answer) => answer,
        Err(error) => fail(format!("{address} can't be reached: {error}")),
    };
    let ok = answer.status().is_success();
    let answer: Value = answer.json().await.unwrap_or_default();
    if !ok {
        fail(answer["error"].as_str().unwrap_or("Weacon refused"));
    }
    answer
}

pub async fn pair(server: &str, code: &str) {
    let server = server.trim_end_matches('/');
    if !private(server) {
        fail(PLAIN);
    }
    let name = printed("hostname", &[]).unwrap_or_else(|| "A machine".into());
    let pairing = json!({"code": code, "name": name, "os": OS});
    let machine = post(&format!("{server}/api/machines"), None, pairing).await;
    let paired = Paired {
        server: server.to_owned(),
        token: machine["token"].as_str().unwrap_or_default().to_owned(),
    };
    let folder = home();
    let file = folder.join("core.json");
    let kept = std::fs::create_dir_all(&folder).and_then(|()| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let text = serde_json::to_string(&paired).expect("a pairing is JSON");
        let new = folder.join("core.json.new");
        options.open(&new)?.write_all(text.as_bytes())?;
        std::fs::rename(new, &file)
    });
    if let Err(error) = kept {
        fail(format!("{} can't be written: {error}", file.display()));
    }
    println!("Paired \"{name}\" with {server}.");

    for found in agents::found().await {
        let (agent, name) = (found.agent, agents::name(found.agent));
        match found.ready {
            Ready::SignedIn => println!("{name} is installed and signed in."),
            Ready::SignedOut => {
                println!("{name} is installed. Sign in to it from Weacon, under Machines.")
            }
            Ready::Missing => {
                let installer = agents::installer(agent);
                println!("{name} isn't installed. Its own installer is:\n  {installer}");
                if yes("Run it now?") {
                    let ended = Command::new("sh").args(["-c", installer]).status();
                    if !ended.is_ok_and(|ended| ended.success()) {
                        println!("The installer didn't finish. Run it yourself when you like.");
                    }
                }
            }
        }
    }
    println!(
        "\nRun `weacon-core` to connect now, or `weacon-core install` to stay connected in the background."
    );
}

pub async fn say_agents() {
    for found in agents::found().await {
        let ready = match found.ready {
            Ready::SignedIn => "installed and signed in",
            Ready::SignedOut => "installed, not signed in",
            Ready::Missing => "not installed",
        };
        println!("{}: {ready}", agents::name(found.agent));
    }
}

fn must(program: &str, args: &[&str], failed: &str) {
    let ended = Command::new(program).args(args).status();
    if !ended.is_ok_and(|ended| ended.success()) {
        fail(failed);
    }
}

pub async fn add(paired: &Paired, host: &str) {
    let system = ["uname", "-sm"];
    let here = printed(system[0], &system[1..]);
    let there = printed("ssh", &[host, "uname", "-sm"]);
    let Some(there) = there else {
        fail(format!(
            "{host} can't be reached over SSH. Try `ssh {host}` first."
        ));
    };
    if here.as_deref() != Some(&there) {
        let own = "Build or download weacon-core for it, copy it there, and pair it from Weacon's Machines page.";
        fail(format!(
            "{host} runs {there}, which this copy of the core isn't built for. {own}"
        ));
    }
    println!(
        "This will, on {host}:\n  copy weacon-core to ~/.local/bin\n  pair it with {}\n  keep it running in the background",
        paired.server
    );
    if !yes("Go on?") {
        return;
    }
    let core = std::env::current_exe().unwrap_or_else(|error| fail(error));
    let core = core.to_string_lossy();
    must(
        "ssh",
        &[host, "mkdir", "-p", ".local/bin"],
        "The folder couldn't be made.",
    );
    must(
        "scp",
        &["-q", &core, &format!("{host}:.local/bin/weacon-core")],
        "The core couldn't be copied.",
    );
    let code = post(
        &format!("{}/api/machines/codes", paired.server),
        Some(&paired.token),
        json!(null),
    )
    .await;
    let code = code["code"].as_str().unwrap_or_default();
    must(
        "ssh",
        &[
            "-t",
            host,
            ".local/bin/weacon-core",
            "pair",
            &paired.server,
            code,
        ],
        "Pairing failed.",
    );
    must(
        "ssh",
        &[host, ".local/bin/weacon-core", "install"],
        "It is paired, but couldn't be set to run in the background.",
    );
    println!("{host} is set up. Sign in to its agents from Weacon, under Machines.");
}
