use core_link::{Deed, Event};
use serde_json::{Value, json};

use crate::{Ended, TOKEN, did, said, text, wanted};

pub fn args(session: Option<&str>, hosts: &[&str], cache: &str, tools: &str) -> Vec<String> {
    let allowed: Vec<String> = hosts.iter().map(|host| format!("{}=\"allow\"", json!(host))).collect();
    let hosts = format!("features.network_proxy.domains={{{}}}", allowed.join(","));
    let cache = format!("sandbox_workspace_write.writable_roots=[{}]", json!(cache));
    let tools = format!("mcp_servers.weacon.url={}", json!(tools));
    let token = format!("mcp_servers.weacon.bearer_token_env_var={}", json!(TOKEN));
    let mut args = vec!["exec"];
    if let Some(session) = session {
        args.extend(["resume", session]);
    }
    args.extend([
        "--json",
        "--config",
        r#"sandbox_mode="workspace-write""#,
        "--config",
        "sandbox_workspace_write.network_access=true",
        "--config",
        "features.network_proxy.enabled=true",
        "--config",
        &hosts,
        "--config",
        &cache,
        "--config",
        r#"project_doc_fallback_filenames=["CLAUDE.md"]"#,
        "--config",
        &tools,
        "--config",
        &token,
        "--config",
        r#"mcp_servers.weacon.default_tools_approval_mode="approve""#,
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "-",
    ]);
    args.into_iter().map(str::to_owned).collect()
}

pub fn read(
    printed: &Value,
    folder: &str,
    session: &mut Option<String>,
    tell: &mut dyn FnMut(Event),
) -> Ended {
    let item = &printed["item"];
    match (printed["type"].as_str(), item["type"].as_str()) {
        (Some("thread.started"), _) => {
            *session = printed["thread_id"].as_str().map(str::to_owned);
        }
        (Some("item.completed"), Some("agent_message")) => {
            said(&item["text"], folder).into_iter().for_each(tell);
        }
        (Some("item.started"), Some("command_execution")) => {
            let command = text(&item["command"]).unwrap_or_default();
            let shell = ["/bin/zsh -lc ", "/bin/bash -lc ", "/bin/sh -lc "];
            let bare = shell.iter().find_map(|shell| command.strip_prefix(shell));
            let command = bare.map_or(command, |bare| bare.trim_matches(['\'', '"']));
            did(Deed::Command, &command.into(), "").into_iter().for_each(tell);
        }
        (Some("item.completed"), Some("command_execution")) => {
            let output = item["aggregated_output"].as_str().unwrap_or_default();
            wanted(output, "Network access to \"", '"', tell);
        }
        (Some("item.completed"), Some("file_change")) => {
            for change in item["changes"].as_array().into_iter().flatten() {
                did(Deed::Edit, &change["path"], folder).into_iter().for_each(&mut *tell);
            }
        }
        (Some("turn.completed"), _) => return Some(Ok(())),
        (Some("turn.failed" | "error"), _) => {
            let why = text(&printed["error"]["message"]).or(text(&printed["message"]));
            return Some(Err(why.unwrap_or("Codex failed").to_owned()));
        }
        _ => {}
    }
    None
}
