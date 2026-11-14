//! Claude Code's pre-tool hook: what it sends the guard, and the answer that asks a person.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{changed_file, changes_file};
use crate::guard::{Context, Reason, check_command, check_path, reading};

/// What Claude Code sends a pre-tool hook, as far as the guard reads it.
#[derive(Deserialize)]
struct ToolUse {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    cwd: PathBuf,
}

/// The reason to ask about the tool call in `input`, a pre-tool hook's JSON, with the
/// context `context` builds for the folder the call runs in.
///
/// # Errors
/// When `input` isn't a pre-tool hook's JSON.
pub fn claude_hook(
    input: &[u8],
    context: impl FnOnce(&Path) -> Context,
) -> Result<Option<Reason>, serde_json::Error> {
    let call: ToolUse = serde_json::from_slice(input)?;
    let context = context(&call.cwd);
    Ok(check_tool(&call.tool_name, &call.tool_input, &context))
}

/// The hook's answer that makes Claude Code ask a person first, whatever its own rules allow.
#[must_use]
pub fn claude_ask(reason: Reason) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": reason.text(),
        }
    })
    .to_string()
}

// Whether a call of `tool` with `input` needs a person first.
fn check_tool(tool: &str, input: &Value, context: &Context) -> Option<Reason> {
    let path = |key: &str| input.get(key).and_then(Value::as_str);
    if tool == "Bash" {
        return path("command").map_or(Some(Reason::Unsure), |line| check_command(line, context));
    }
    if let Some(file) = changed_file(tool, input) {
        return check_path(&context.cwd.join(file), context, Reason::WriteOutside);
    }
    match tool {
        "Read" | "NotebookRead" | "Grep" | "Glob" | "LS" => {
            let deep = matches!(tool, "Grep" | "Glob");
            ["file_path", "notebook_path", "path"]
                .into_iter()
                .filter_map(path)
                .find_map(|file| reading(&context.cwd.join(file), context, deep))
        }
        // A tool that changes a file, without saying which.
        _ if changes_file(tool) => Some(Reason::Unsure),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::guard::tests::context as guard_context;

    fn context(cwd: &Path) -> Context {
        Context {
            cwd: cwd.to_path_buf(),
            worktree: cwd.to_path_buf(),
            ..Context::default()
        }
    }

    #[test]
    fn a_tool_call_is_checked_in_the_folder_it_runs_in() {
        let input = |command: &str| {
            json!({
                "session_id": "s",
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "command": command },
                "cwd": "/work",
            })
            .to_string()
        };

        let outside = claude_hook(input("rm -rf /etc").as_bytes(), context).unwrap();
        let inside = claude_hook(input("rm -rf /work/target").as_bytes(), context).unwrap();
        let other = json!({ "tool_name": "WebFetch", "cwd": "/work" }).to_string();

        assert_eq!(outside, Some(Reason::DeleteOutside));
        assert_eq!(inside, None);
        assert_eq!(claude_hook(other.as_bytes(), context).unwrap(), None);
        claude_hook(b"not json", context).unwrap_err();
        claude_hook(br#"{"tool_name":"Bash"}"#, context).unwrap_err();
    }

    #[test]
    fn the_answer_asks_with_the_reason() {
        let answer: Value = serde_json::from_str(&claude_ask(Reason::ForcePush)).unwrap();

        assert_eq!(
            answer,
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "ask",
                    "permissionDecisionReason": "It force-pushes.",
                }
            })
        );
    }

    #[test]
    fn file_tools_ask_outside_the_worktree_and_near_credentials() {
        let dir = TempDir::new().unwrap();
        let context = guard_context(&dir);
        let tool = |name: &str, input: Value| check_tool(name, &input, &context);
        let inside = dir.join("wt/src/lib.rs").display().to_string();
        let outside = dir.join("main/src/lib.rs").display().to_string();
        let secret = dir
            .join("home/.claude/.credentials.json")
            .display()
            .to_string();
        let settings = dir.join("state/settings/w-w1.json").display().to_string();

        assert_eq!(tool("Write", json!({ "file_path": inside })), None);
        assert_eq!(tool("Edit", json!({ "file_path": "src/lib.rs" })), None);
        assert_eq!(
            tool("Write", json!({ "file_path": outside })),
            Some(Reason::WriteOutside)
        );
        assert_eq!(
            tool("Write", json!({ "file_path": settings })),
            Some(Reason::WeaconFolder)
        );
        assert_eq!(
            tool("Edit", json!({ "file_path": secret })),
            Some(Reason::Credentials)
        );
        assert_eq!(
            tool("Read", json!({ "file_path": secret })),
            Some(Reason::Credentials)
        );
        assert_eq!(
            tool(
                "Grep",
                json!({ "pattern": "x", "path": dir.join("home/.codex") })
            ),
            Some(Reason::Credentials)
        );
        assert_eq!(tool("Read", json!({ "file_path": inside })), None);
        assert_eq!(tool("Write", json!({})), Some(Reason::Unsure));
        assert_eq!(tool("Bash", json!({})), Some(Reason::Unsure));
        assert_eq!(
            tool("Bash", json!({ "command": "rm -rf /" })),
            Some(Reason::DeleteOutside)
        );
        assert_eq!(
            tool("WebFetch", json!({ "url": "https://example.com" })),
            None
        );
    }

    #[test]
    fn a_search_through_home_reaches_credentials_and_a_listing_doesnt() {
        let dir = TempDir::new().unwrap();
        let context = guard_context(&dir);
        let home = dir.join("home").display().to_string();
        let tool = |name: &str, input: Value| check_tool(name, &input, &context);

        assert_eq!(
            tool("Grep", json!({ "pattern": "x", "path": home })),
            Some(Reason::Credentials)
        );
        assert_eq!(tool("LS", json!({ "path": home })), None);
    }
}
