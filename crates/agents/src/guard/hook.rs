//! Claude Code's pre-tool hook: what it sends the guard, and the answer that asks a person.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Context, Reason, check_tool};

/// What Claude Code sends a pre-tool hook, as far as the guard reads it.
#[derive(Deserialize)]
struct ToolUse {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    cwd: PathBuf,
}

/// The reason to ask about the tool call in `input`, a pre-tool hook's JSON, with the
/// context `context` builds for the folder the call runs in; `Err` when `input` isn't one.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
