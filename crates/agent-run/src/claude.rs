use core_link::{Deed, Event};
use serde_json::{Value, json};

use crate::{Ended, TOKEN, did, said, text, wanted};

const INSTRUCTIONS: &str = "This repository's own instructions are in CLAUDE.md at its root, \
    or in AGENTS.md where there is no CLAUDE.md. Read that file before anything else, with \
    the files it names, and follow them. Where there is neither, go on without.";

pub fn args(session: Option<&str>, hosts: &[&str], cache: &str, tools: &str) -> Vec<String> {
    let sandbox = json!({"sandbox": {
        "enabled": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": {"allowedDomains": hosts},
        "filesystem": {"allowWrite": [cache]},
    }});
    let sandbox = sandbox.to_string();
    let bearer = format!("Bearer ${{{TOKEN}}}");
    let tools = json!({"mcpServers": {"weacon": {
        "type": "http", "url": tools, "headers": {"Authorization": bearer},
    }}});
    let tools = tools.to_string();
    let mut args = vec![
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        &tools,
        "--permission-mode",
        "acceptEdits",
        "--permission-prompts",
        "none",
        "--allowedTools",
        "Bash",
        "mcp__weacon",
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

pub fn read(
    printed: &Value,
    folder: &str,
    session: &mut Option<String>,
    tell: &mut dyn FnMut(Event),
) -> Ended {
    let parts = printed["message"]["content"].as_array().into_iter().flatten();
    let own = printed["parent_tool_use_id"].is_null();
    match printed["type"].as_str() {
        Some("assistant") if own => {
            for part in parts {
                let input = &part["input"];
                let event = match (part["type"].as_str(), part["name"].as_str()) {
                    (Some("text"), _) => said(&part["text"], folder),
                    (Some("tool_use"), Some("Bash")) => did(Deed::Command, &input["command"], ""),
                    (Some("tool_use"), Some("Read")) => did(Deed::Read, &input["file_path"], folder),
                    (Some("tool_use"), Some("Edit" | "Write" | "NotebookEdit")) => {
                        let path = input["file_path"].as_str().or(input["notebook_path"].as_str());
                        did(Deed::Edit, &path.into(), folder)
                    }
                    (Some("tool_use"), Some(name))
                        if name.starts_with("mcp__weacon__") || name == "ToolSearch" =>
                    {
                        None
                    }
                    (Some("tool_use"), Some(name)) => did(Deed::Other, &name.into(), ""),
                    _ => None,
                };
                event.into_iter().for_each(&mut *tell);
            }
        }
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
