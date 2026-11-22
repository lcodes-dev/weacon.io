//! The vendors' own programs: finding them, and having one answer a prompt.

use std::process::Stdio;
use std::time::Duration;

use agent_run::KEPT;
use core_link::{Agent, Found, Ready};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// A page can take minutes to write.
const PATIENCE: Duration = Duration::from_secs(15 * 60);

/// How long an agent's program may take to say whether someone is signed in.
const STATUS: Duration = Duration::from_secs(10);

/// The vendor's command that says, by how it ends, whether someone is signed in.
fn status(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "status"],
        Agent::Codex => &["login", "status"],
    }
}

/// The vendor's own sign-in, in a form that needs no browser on this machine.
pub fn sign_in(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["auth", "login"],
        Agent::Codex => &["login", "--device-auth"],
    }
}

/// The vendor's own installer for macOS, Linux and WSL, from the vendor's pages.
pub fn installer(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "curl -fsSL https://claude.ai/install.sh | bash",
        Agent::Codex => "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
    }
}

pub fn name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude Code",
        Agent::Codex => "Codex",
    }
}

/// The agents on this machine. Only whether someone is signed in is learned, nothing of who.
pub async fn found() -> Vec<Found> {
    let mut found = Vec::new();
    for agent in Agent::ALL {
        let mut command = Command::new(agent.program());
        let quiet = command.args(status(agent)).stdin(Stdio::null());
        let quiet = quiet.stdout(Stdio::null()).stderr(Stdio::null());
        let ready = match quiet.kill_on_drop(true).spawn() {
            Err(_) => Ready::Missing,
            // A program that doesn't answer is there, and is of no use as it is.
            Ok(mut asked) => match tokio::time::timeout(STATUS, asked.wait()).await {
                Ok(Ok(ended)) if ended.success() => Ready::SignedIn,
                _ => Ready::SignedOut,
            },
        };
        found.push(Found { agent, ready });
    }
    found
}

/// Has an agent answer one prompt, with no tools and none of the person's own settings.
pub async fn ask(agent: Agent, model: &str, system: &str, prompt: &str) -> Result<String, String> {
    answer(agent.program(), agent, model, system, prompt).await
}

async fn answer(
    program: &str,
    agent: Agent,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<String, String> {
    match agent {
        Agent::Claude => {
            let model = if model.is_empty() {
                "claude-haiku-5-5"
            } else {
                model
            };
            // No tools, and none of the hooks, instructions and tool servers of the person's own setup.
            let args = [
                "--print",
                "--safe-mode",
                "--model",
                model,
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                "--system-prompt",
                system,
            ];
            run(program, &args, prompt).await
        }
        Agent::Codex => {
            let model = if model.is_empty() {
                "gpt-5.6-luna"
            } else {
                model
            };
            // The sandbox only stops writing, so the tools that run commands are off too:
            // text in a repository must not be able to make Codex read this machine's files.
            let args = [
                "exec",
                "--model",
                model,
                "--sandbox",
                "read-only",
                "--disable",
                "shell_tool",
                "--disable",
                "unified_exec",
                "--ignore-user-config",
                "--ignore-rules",
                "--skip-git-repo-check",
                "--ephemeral",
                "--color",
                "never",
                "-",
            ];
            // Codex takes no separate system prompt.
            run(program, &args, &format!("{system}\n\n{prompt}")).await
        }
    }
}

/// Runs a program in an empty folder, with `input` on its input, and returns what it prints.
async fn run(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    let folder = std::env::temp_dir().join(format!("weacon-core-{:016x}", rand::random::<u64>()));
    let made = tokio::fs::create_dir_all(&folder).await;
    made.map_err(|error| error.to_string())?;
    let child = Command::new(program)
        .args(args)
        .current_dir(&folder)
        .env_clear()
        .envs(std::env::vars().filter(|(name, _)| KEPT.contains(&name.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let output = match child {
        Ok(mut child) => {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            let input = input.to_owned();
            // Written on the side, so a program that prints before it has read everything can't block us.
            tokio::spawn(async move { stdin.write_all(input.as_bytes()).await });
            match tokio::time::timeout(PATIENCE, child.wait_with_output()).await {
                Ok(output) => output.map_err(|error| error.to_string()),
                Err(_) => Err(format!("{program} took too long")),
            }
        }
        Err(error) => Err(format!("{program} could not be started: {error}")),
    };
    tokio::fs::remove_dir_all(&folder).await.ok();

    let output = output?;
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        return Ok(printed);
    }
    let complaint = String::from_utf8_lossy(&output.stderr);
    let complaint = if complaint.trim().is_empty() {
        &printed
    } else {
        complaint.trim()
    };
    // The end of it says what went wrong.
    let tail = complaint.char_indices().rev().nth(399);
    let tail = &complaint[tail.map_or(0, |(at, _)| at)..];
    Err(format!("{program} failed: {tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // No request reaches a vendor's program in the server's tests, so it is stood in for here.
    #[tokio::test]
    async fn an_agent_answers_from_its_prompt_and_says_why_when_it_fails() {
        let program = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_agent.sh");
        for agent in Agent::ALL {
            let plan = answer(program, agent, "", "Answer with JSON", "The outline").await;
            assert!(plan.unwrap().starts_with("{\"left_out\""));
            let page = answer(program, agent, "", "Write a page", "The files").await;
            assert_eq!(page.unwrap(), "Written by the program.");
        }
        let failed = answer("false", Agent::Codex, "", "", "").await;
        assert_eq!(failed.unwrap_err(), "false failed: ");
        let missing = answer("no-such-program", Agent::Claude, "", "", "").await;
        assert!(
            missing
                .unwrap_err()
                .starts_with("no-such-program could not be started")
        );
    }
}
