//! The tools the agent of a chat has while its turn runs: its project's tasks and docs. The
//! agent's program calls them here as the Model Context Protocol has it, one JSON-RPC request
//! in and one answer out.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::db::{self, Task, TaskFields, ToolChat};
use crate::{ApiError, AppState, not_found, tasks};

const STATUSES: [(&str, &str); 3] = [("todo", "To do"), ("doing", "Doing"), ("done", "Done")];

/// What each tool is called, what it does and what it takes.
fn tools() -> Value {
    let text = json!({"type": "string"});
    let name = json!({"type": "string", "description": "As SF-12. Left out, it is the task this chat is for."});
    let checks = json!({
        "type": "array",
        "description": "What it is done when: the whole list, each with whether it is done. To tick one, send the list with that one done.",
        "items": {"type": "object", "properties": {"text": text, "done": {"type": "boolean"}}, "required": ["text", "done"]},
    });
    let tool = |name: &str, does: &str, takes: Value, needs: &[&str]| json!({"name": name, "description": does, "inputSchema": {"type": "object", "properties": takes, "required": needs}});
    json!([
        tool(
            "tasks",
            "Lists this project's tasks: name, status, title and owner.",
            json!({}),
            &[]
        ),
        tool(
            "task",
            "Reads one task: its text, what it is done when, and the pages of the docs it touches.",
            json!({"name": name}),
            &[]
        ),
        tool(
            "new_task",
            "Makes a task in this project, asked for by the person whose chat this is.",
            json!({"title": text, "text": text, "checks": {"type": "array", "items": text, "description": "What it is done when."}}),
            &["title"]
        ),
        tool(
            "change_task",
            "Changes what it is given of a task and leaves the rest.",
            json!({"name": name, "title": text, "text": text, "status": {"enum": ["todo", "doing", "done"]}, "checks": checks}),
            &[]
        ),
        tool(
            "doc",
            "Reads a page of this project's docs by its title.",
            json!({"title": text}),
            &["title"]
        ),
    ])
}

pub async fn call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let token = token.and_then(|value| value.strip_prefix("Bearer "));
    let chat = match token {
        Some(token) => db::tool_chat(&state.pool, token).await.ok().flatten(),
        None => None,
    };
    let Some(chat) = chat else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (id, params) = (&request["id"], &request["params"]);
    // Something the program only lets us know.
    if id.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let mut answer = match request["method"].as_str() {
        Some("initialize") => json!({"result": {
            "protocolVersion": params["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "weacon", "version": env!("CARGO_PKG_VERSION")},
        }}),
        Some("tools/list") => json!({"result": {"tools": tools()}}),
        Some("tools/call") => {
            let name = params["name"].as_str().unwrap_or_default();
            let done = tool(&state, &chat, name, &params["arguments"]).await;
            let failed = done.is_err();
            let text = done.unwrap_or_else(|ApiError(_, why)| why);
            json!({"result": {"content": [{"type": "text", "text": text}], "isError": failed}})
        }
        _ => json!({"error": {"code": -32601, "message": "Method not found"}}),
    };
    answer["jsonrpc"] = "2.0".into();
    answer["id"] = id.clone();
    Json(answer).into_response()
}

fn refused(why: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, why.into())
}

/// The task named in `name`, as SF-12 or 12, or the chat's own.
fn named<'a>(tasks: &'a [Task], chat: &ToolChat, name: &Value) -> Result<&'a Task, ApiError> {
    let found = match name.as_str() {
        Some(name) => {
            let number = name
                .rsplit('-')
                .next()
                .and_then(|number| number.trim().parse().ok());
            tasks.iter().find(|task| Some(task.number) == number)
        }
        None => tasks
            .iter()
            .find(|task| Some(&task.id) == chat.task_id.as_ref()),
    };
    found.ok_or_else(|| match name.as_str() {
        Some(name) => refused(format!("This project has no task {name}")),
        None => refused("This chat is for no task. Name one, as the tasks tool lists them"),
    })
}

fn status(of: &str) -> &str {
    STATUSES
        .iter()
        .find(|(status, _)| *status == of)
        .map_or(of, |(_, shown)| shown)
}

async fn tool(
    state: &AppState,
    chat: &ToolChat,
    name: &str,
    given: &Value,
) -> Result<String, ApiError> {
    let pool = &state.pool;
    let tasks = db::tasks(pool, &chat.team_id, &chat.project_id).await?;
    let called = |task: &Task| format!("{}-{}", chat.key, task.number);
    // Says in the chat what its agent did to a task.
    let note = async |id: &str, name: String, made: bool| {
        let body = json!({"kind": "task", "task_id": id, "name": name, "made": made});
        db::note_chat(pool, &chat.id, &body).await
    };
    match name {
        "tasks" => {
            let line = |task: &Task| {
                let owner = task.owner.as_deref().unwrap_or("nobody");
                format!(
                    "{} · {} · {} · {owner}",
                    called(task),
                    status(&task.status),
                    task.title
                )
            };
            Ok(tasks.iter().map(line).collect::<Vec<_>>().join("\n"))
        }
        "task" => {
            let task = named(&tasks, chat, &given["name"])?;
            let docs = db::docs(pool, &chat.project_id).await?;
            let pages = docs.iter().filter(|doc| task.docs.contains(&doc.id));
            let pages: Vec<&str> = pages.map(|doc| doc.title.as_str()).collect();
            let checks = task.checks.iter().enumerate().map(|(place, check)| {
                format!(
                    "{}. [{}] {}",
                    place + 1,
                    if check.done { "x" } else { " " },
                    check.text
                )
            });
            Ok(format!(
                "{} {}\nStatus: {} · Owner: {}\n\n{}\n\nDone when:\n{}\n\nDocs it touches, each read with the doc tool: {}",
                called(task),
                task.title,
                status(&task.status),
                task.owner.as_deref().unwrap_or("nobody"),
                task.text,
                checks.collect::<Vec<_>>().join("\n"),
                pages.join("; "),
            ))
        }
        "new_task" => {
            let checks = given["checks"].as_array().into_iter().flatten();
            let checks = checks.filter_map(|check| check.as_str());
            let checks = checks.map(|text| json!({"text": text, "done": false}));
            let fields = json!({"title": given["title"], "text": given["text"].as_str(), "checks": checks.collect::<Vec<_>>()});
            let fields = fields_of(fields)?;
            let fields = tasks::checked(state, &chat.team_id, fields).await?;
            if fields.title.is_none() {
                return Err(refused("A task needs a title"));
            }
            let id = db::create_task(
                pool,
                &chat.project_id,
                &chat.user_id,
                Some(&chat.id),
                &fields,
            )
            .await?;
            let tasks = db::tasks(pool, &chat.team_id, &chat.project_id).await?;
            let made = tasks
                .iter()
                .find(|task| task.id == id)
                .ok_or_else(not_found)?;
            note(&id, format!("{} {}", called(made), made.title), true).await?;
            Ok(format!("Made {}.", called(made)))
        }
        "change_task" => {
            let task = named(&tasks, chat, &given["name"])?;
            let mut fields = serde_json::Map::new();
            for field in ["title", "text", "checks"] {
                if !given[field].is_null() {
                    fields.insert(field.into(), given[field].clone());
                }
            }
            let fields = tasks::checked(state, &chat.team_id, fields_of(fields.into())?).await?;
            db::change_task(pool, &chat.project_id, &task.id, &fields).await?;
            if let Some(to) = given["status"].as_str() {
                if !STATUSES.iter().any(|(status, _)| *status == to) {
                    return Err(refused("A task is todo, doing or done"));
                }
                // It goes to the end of its new status.
                let others = tasks
                    .iter()
                    .filter(|other| other.status == to && other.id != task.id);
                let mut order: Vec<String> = others.map(|other| other.id.clone()).collect();
                order.push(task.id.clone());
                db::order_tasks(pool, &chat.project_id, to, &order).await?;
            }
            let title = fields.title.as_ref().unwrap_or(&task.title);
            note(&task.id, format!("{} {title}", called(task)), false).await?;
            Ok(format!("Changed {}.", called(task)))
        }
        "doc" => {
            let docs = db::docs(pool, &chat.project_id).await?;
            let title = given["title"].as_str().unwrap_or_default();
            match docs
                .iter()
                .find(|doc| doc.title.eq_ignore_ascii_case(title.trim()))
            {
                Some(doc) => Ok(format!("# {}\n\n{}", doc.title, doc.markdown)),
                None => {
                    let titles: Vec<&str> = docs.iter().map(|doc| doc.title.as_str()).collect();
                    Err(refused(format!(
                        "No page is called that. The pages are: {}",
                        titles.join("; ")
                    )))
                }
            }
        }
        _ => Err(refused("There is no such tool")),
    }
}

/// What an agent gave for a task, as a person's request gives it.
fn fields_of(given: Value) -> Result<TaskFields, ApiError> {
    serde_json::from_value(given)
        .map_err(|error| refused(format!("That can't be read as a task: {error}")))
}
