//! The team's own agent, called in a group as `@weacon`. It runs here, on the team's API
//! key, and answers from the tasks and docs of the group's project with the tools a chat's
//! agent has. It keeps nothing between calls.

use serde_json::{Value, json};

use crate::db::{self, Caller, Group, User};
use crate::{ApiError, AppState, chats, tools, writer};

/// What its answers are marked with in a group.
pub const NAME: &str = "weacon";

/// How often it may ask for its tools in one call.
const ROUNDS: usize = 10;

const SYSTEM: &str = "You are Weacon, the agent of a team, called in one of its groups about one \
    of its projects. You know the project by its tasks and its docs, which you read and search \
    with your tools; the docs say which files they were written from. You can't read the code \
    itself or run anything, and you say so when that is what an answer needs. You remember \
    nothing between calls: what was said before is among the group's messages. Answer briefly, \
    in Markdown.";

/// Has the team's agent answer in a group, in the background. Gives why when nothing runs.
pub async fn called(
    state: &AppState,
    user: &User,
    team_id: &str,
    group: &Group,
) -> Result<Option<String>, ApiError> {
    let projects = db::projects(&state.pool, team_id).await?;
    let about = group.project_id.as_deref();
    let Some(project) = projects.iter().find(|project| Some(&*project.id) == about) else {
        return Ok(Some(chats::NO_PROJECT.into()));
    };
    let Some((key, model)) = db::api_key(&state.pool, team_id).await? else {
        let why = "This team has no API key for @weacon. Its owner saves one in Team settings";
        return Ok(Some(why.into()));
    };
    let written = db::group_messages(&state.pool, &group.id, "").await?;
    let asked = chats::told(group, user, None, &written);
    if !state.weacon.lock().unwrap().insert(group.id.clone()) {
        return Ok(Some("Weacon is still working on the last call here".into()));
    }
    let caller = Caller {
        chat_id: None,
        team_id: team_id.into(),
        project_id: project.id.clone(),
        key: project.key.clone(),
        user_id: user.id.clone(),
        task_id: None,
        agent: NAME.into(),
    };
    let (state, group_id) = (state.clone(), group.id.clone());
    tokio::spawn(async move {
        let model = writer::model(model.as_deref());
        let answer = answer(&state, &key, model, &caller, asked).await;
        let text = answer.unwrap_or_else(|why| format!("It couldn't answer: {why}"));
        // The group may be gone by now.
        let _ = db::answer_as_weacon(&state.pool, &group_id, &caller.user_id, &text).await;
        state.weacon.lock().unwrap().remove(&group_id);
    });
    Ok(None)
}

/// Asks the model, runs each tool it asks for and tells it the result, until it has its answer.
async fn answer(
    state: &AppState,
    key: &str,
    model: &str,
    caller: &Caller,
    asked: String,
) -> Result<String, String> {
    // The same tools, as Anthropic names their parts.
    let tools = tools::tools();
    let tool = |tool: &Value| {
        let (name, does, takes) = (&tool["name"], &tool["description"], &tool["inputSchema"]);
        json!({"name": name, "description": does, "input_schema": takes})
    };
    let tools: Vec<Value> = tools.as_array().into_iter().flatten().map(tool).collect();
    let mut messages = vec![json!({"role": "user", "content": asked})];
    for _ in 0..ROUNDS {
        let body = json!({
            "model": model, "max_tokens": 16000, "system": SYSTEM, "tools": tools, "messages": messages,
        });
        let answer = writer::anthropic(state, key, &body).await?;
        match answer["stop_reason"].as_str() {
            Some("end_turn") => {
                let text = writer::text(&answer);
                return Ok(if text.trim().is_empty() {
                    "It had nothing to say.".into()
                } else {
                    text
                });
            }
            Some("tool_use") => {}
            _ => return Err(writer::stopped(&answer)),
        }
        let blocks = answer["content"].as_array().cloned().unwrap_or_default();
        let mut results = Vec::new();
        for block in blocks.iter().filter(|block| block["type"] == "tool_use") {
            let name = block["name"].as_str().unwrap_or_default();
            let done = tools::tool(state, caller, name, &block["input"]).await;
            let failed = done.is_err();
            let text = done.unwrap_or_else(|ApiError(_, why)| why);
            results.push(json!({
                "type": "tool_result", "tool_use_id": block["id"], "content": text, "is_error": failed,
            }));
        }
        messages.push(json!({"role": "assistant", "content": blocks}));
        messages.push(json!({"role": "user", "content": results}));
    }
    Err(format!("It asked for its tools more than {ROUNDS} times"))
}
