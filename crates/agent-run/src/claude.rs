//! Claude Code: how its program is started for a turn, and what it prints.

use core_link::{Deed, Event};
use serde_json::{Value, json};

use crate::{Ended, did, said, text, wanted};

/// What makes it follow the repository's instructions, which it would otherwise leave out
/// with the person's own setup. A repository's settings and hooks stay out.
const INSTRUCTIONS: &str = "This repository's own instructions are in CLAUDE.md at its root, \
    or in AGENTS.md where there is no CLAUDE.md. Read that file before anything else, with \
    the files it names, and follow them. Where there is neither, go on without.";

pub fn args(session: Option<&str>, hosts: &[&str], cache: &str) -> Vec<String> {
    // Its own sandbox for commands, with nothing let out of it.
    let sandbox = json!({"sandbox": {
        "enabled": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": {"allowedDomains": hosts},
        "filesystem": {"allowWrite": [cache]},
    }});
    let sandbox = sandbox.to_string();
    let mut args = vec![
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--safe-mode",
        "--strict-mcp-config",
        "--permission-mode",
        "acceptEdits",
        // What would ask a person is refused, and the agent is told.
        "--permission-prompts",
        "none",
        // Every command is allowed, since every command is in the sandbox.
        "--allowedTools",
        "Bash",
        "--settings",
        &sandbox,
        "--append-system-prompt",
        INSTRUCTIONS,
    ];
    if let Some(session) = session {
        args.extend(["--resume", session]);
    }
    args.into_iter().map(str::to_owned).collect()
}

/// One line of `claude --output-format stream-json`. What a sub-agent says and does is its
/// own, and isn't shown yet.
pub fn read(
    printed: &Value,
    folder: &str,
    session: &mut Option<String>,
    tell: &mut dyn FnMut(Event),
) -> Ended {
    let parts = printed["message"]["content"].as_array();
    let parts = parts.into_iter().flatten();
    let own = printed["parent_tool_use_id"].is_null();
    match printed["type"].as_str() {
        Some("assistant") if own => {
            for part in parts {
                let input = &part["input"];
                let event = match (part["type"].as_str(), part["name"].as_str()) {
                    (Some("text"), _) => said(&part["text"], folder),
                    (Some("tool_use"), Some("Bash")) => did(Deed::Command, &input["command"], ""),
                    (Some("tool_use"), Some("Read")) => {
                        did(Deed::Read, &input["file_path"], folder)
                    }
                    (Some("tool_use"), Some("Edit" | "Write" | "NotebookEdit")) => {
                        let path = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str());
                        did(Deed::Edit, &path.into(), folder)
                    }
                    (Some("tool_use"), Some(name)) => did(Deed::Other, &name.into(), ""),
                    _ => None,
                };
                event.into_iter().for_each(&mut *tell);
            }
        }
        // A command's result, where the sandbox lists the hosts it refused, each with its port.
        Some("user") if own => {
            for part in parts {
                let result = part["content"].to_string();
                wanted(&result, "deny network-outbound ", ':', tell);
            }
        }
        Some("result") => {
            *session = printed["session_id"].as_str().map(str::to_owned);
            let why = text(&printed["result"]).unwrap_or("Claude Code failed");
            return Some(match printed["is_error"].as_bool() {
                Some(true) => Err(why.to_owned()),
                _ => Ok(()),
            });
        }
        _ => {}
    }
    None
}
