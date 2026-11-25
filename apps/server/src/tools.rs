//! The tools an agent has for its project's tasks and docs, which it reads and proposes changes to. The program of a chat's agent
//! calls them here while its turn runs, as the Model Context Protocol has it, one JSON-RPC request
//! in and one answer out. For the team's own agent the server runs them itself.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::db::{self, Caller, Task, TaskFields};
use crate::teams::refused;
use crate::{ApiError, AppState, search, tasks};

/// What each tool is called, what it does and what it takes.
pub fn tools() -> Value {
    let text = json!({"type": "string"});
    let name = json!({"type": "string", "description": "As SF-12. Left out, it is the task your chat is for, if it is for one."});
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
            "Makes a task in this project, asked for by the person you work for.",
            json!({"title": text, "text": text, "checks": checks}),
            &["title"]
        ),
        tool(
            "change_task",
            "Changes what it is given of a task and leaves the rest.",
            json!({"name": name, "title": text, "text": text, "status": {"enum": tasks::STATUSES}, "checks": checks}),
            &[]
        ),
        tool(
            "doc",
            "Reads a page of this project's docs by its title.",
            json!({"title": text}),
            &["title"]
        ),
        tool(
            "propose_doc",
            "Proposes a new text for a page of this project's docs, or a new page, when a page is wrong or you changed the code it describes. Nothing changes until a person of the team accepts it. Read the page with the doc tool first, and give its whole new text.",
            json!({
                "title": {"type": "string", "description": "The page's title. One that no page has makes a new page."},
                "markdown": {"type": "string", "description": "The page's whole text, in Markdown."},
                "reason": {"type": "string", "description": "Why, in a sentence."},
                "section": {"enum": ["works", "built"], "description": "For a new page: how the product works for its users, or how it is built. Left out, built."},
            }),
            &["title", "markdown", "reason"]
        ),
        tool(
            "search_docs",
            "Finds what this project's docs say about something: the parts of its pages nearest in meaning, each with the files its page was written from.",
            json!({"about": text}),
            &["about"]
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
    let caller = match token {
        Some(token) => db::caller(&state.pool, token).await.ok().flatten(),
        None => None,
    };
    let Some(caller) = caller else {
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
            let done = tool(&state, &caller, name, &params["arguments"]).await;
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

/// The task named in `name`, as SF-12 or 12, or the chat's own.
fn named<'a>(tasks: &'a [Task], caller: &Caller, name: &Value) -> Result<&'a Task, ApiError> {
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
            .find(|task| Some(&task.id) == caller.task_id.as_ref()),
    };
    found.ok_or_else(|| match name.as_str() {
        Some(name) => refused(format!("This project has no task {name}")),
        None => refused("Name the task, as the tasks tool lists them"),
    })
}

/// What an agent gave for a task, read as a person's request is. An agent gives a task to
/// nobody and doesn't say which pages it touches.
fn fields(given: &Value) -> Result<TaskFields, ApiError> {
    let read = serde_json::from_value::<TaskFields>(given.clone());
    let fields = read.map_err(|error| refused(format!("That can't be read as a task: {error}")))?;
    Ok(TaskFields {
        owner_id: None,
        docs: None,
        ..fields
    })
}

pub async fn tool(
    state: &AppState,
    caller: &Caller,
    name: &str,
    given: &Value,
) -> Result<String, ApiError> {
    let pool = &state.pool;
    let place = (caller.team_id.as_str(), caller.project_id.as_str());
    let tasks = async || db::tasks(pool, place.0, place.1).await;
    // Says in the agent's chat, if it has one, what it did to a task, and answers the agent.
    let wrote = async |id: &str, number: i64, title: &str, made: bool| {
        let name = format!("{}-{number}", caller.key);
        let body =
            json!({"kind": "task", "task_id": id, "name": format!("{name} {title}"), "made": made});
        if let Some(chat) = &caller.chat_id {
            db::note_chat(pool, chat, &body).await?;
        }
        Ok(format!("{} {name}.", if made { "Made" } else { "Changed" }))
    };
    let nobody = |owner: &Option<String>| owner.clone().unwrap_or("nobody".into());
    match name {
        "tasks" => {
            let line = |task: &Task| {
                let (number, owner) = (task.number, nobody(&task.owner));
                format!(
                    "{}-{number} · {} · {} · {owner}",
                    caller.key, task.status, task.title
                )
            };
            Ok(tasks()
                .await?
                .iter()
                .map(line)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "task" => {
            let tasks = tasks().await?;
            let task = named(&tasks, caller, &given["name"])?;
            let docs = db::docs(pool, place.1).await?;
            let pages = docs.iter().filter(|doc| task.docs.contains(&doc.id));
            let pages = pages.map(|doc| format!("{}{}", doc.title, written_from(&doc.files)));
            let checks = task.checks.iter().enumerate().map(|(place, check)| {
                format!(
                    "{}. [{}] {}",
                    place + 1,
                    if check.done { "x" } else { " " },
                    check.text
                )
            });
            Ok(format!(
                "{}-{} {}\nStatus: {} · Owner: {}\n\n{}\n\nDone when:\n{}\n\nDocs it touches, each read with the doc tool: {}",
                caller.key,
                task.number,
                task.title,
                task.status,
                nobody(&task.owner),
                task.text,
                checks.collect::<Vec<_>>().join("\n"),
                pages.collect::<Vec<_>>().join("; "),
            ))
        }
        "new_task" => {
            let fields = fields(given)?;
            let title = fields.title.clone().unwrap_or_default();
            let (id, number) = tasks::made(
                state,
                place,
                &caller.user_id,
                caller.chat_id.as_deref(),
                fields,
            )
            .await?;
            wrote(&id, number, title.trim(), true).await
        }
        "change_task" => {
            let tasks = tasks().await?;
            let task = named(&tasks, caller, &given["name"])?;
            let fields = tasks::changed(state, place, &task.id, fields(given)?).await?;
            let title = fields.title.as_ref().unwrap_or(&task.title);
            wrote(&task.id, task.number, title, false).await
        }
        "search_docs" => {
            let about = given["about"].as_str().unwrap_or_default();
            let parts = search::search(state, place.1, about).await?;
            let shown = parts.iter().take(5).map(|part| {
                // A part's first line is its page's title.
                let text = part.text.split_once('\n').map_or("", |(_, text)| text);
                format!(
                    "# {}{}\n\n{}",
                    part.title,
                    written_from(&part.files),
                    text.trim()
                )
            });
            Ok(shown.collect::<Vec<_>>().join("\n\n---\n\n"))
        }
        "doc" => {
            let docs = db::docs(pool, place.1).await?;
            let title = given["title"].as_str().unwrap_or_default().trim();
            match docs
                .iter()
                .find(|doc| doc.title.eq_ignore_ascii_case(title))
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
        "propose_doc" => {
            let said = |name: &str| given[name].as_str().unwrap_or_default().trim();
            let (title, markdown, reason) = (said("title"), said("markdown"), said("reason"));
            if title.is_empty() || markdown.is_empty() || reason.is_empty() {
                return Err(refused(
                    "A proposal needs the page's title, its text and why",
                ));
            }
            let docs = db::docs(pool, place.1).await?;
            let page = docs
                .iter()
                .find(|doc| doc.title.eq_ignore_ascii_case(title));
            // The doc tool shows a page under its title, which isn't part of its text.
            let headed = markdown.split_once('\n').filter(|(first, _)| {
                let first = first.strip_prefix("# ").unwrap_or_default().trim();
                first.eq_ignore_ascii_case(title)
            });
            let markdown = headed.map_or(markdown, |(_, text)| text.trim());
            if page.is_some_and(|page| page.markdown.trim() == markdown) {
                return Err(refused("That is the page's text already"));
            }
            let section = match (page, said("section")) {
                (Some(page), _) => &page.section,
                (None, "works") => "works",
                (None, _) => "built",
            };
            let title = page.map_or(title, |page| &page.title);
            db::propose(pool, caller, page, section, title, markdown, reason).await?;
            if let Some(chat) = &caller.chat_id {
                let body = json!({"kind": "proposal", "title": title, "new": page.is_none()});
                db::note_chat(pool, chat, &body).await?;
            }
            Ok(format!(
                "Proposed. \"{title}\" changes when a person of the team accepts it on the docs page."
            ))
        }
        _ => Err(refused("There is no such tool")),
    }
}

/// The files a page was written from, which is where the code it describes is; `files` is their JSON list.
fn written_from(files: &str) -> String {
    let files: Vec<String> = serde_json::from_str(files).unwrap_or_default();
    match files.is_empty() {
        true => String::new(),
        false => format!(" (written from {})", files.join(", ")),
    }
}
