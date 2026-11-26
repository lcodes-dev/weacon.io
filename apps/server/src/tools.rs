//! The tools an agent has for its project's tasks and docs, which it reads and proposes
//! changes to. A chat's agent calls them over `/api/tools`; for the team's own agent the
//! server runs them itself.

use serde_json::{Value, json};

use crate::db::chats::Caller;
use crate::db::tasks::{Task, TaskFields};
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db, search, tasks};

/// What each tool is called, what it does and what it takes.
pub fn list() -> Value {
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

/// The task named in `name`, as SF-12 or 12, or the chat's own.
fn named<'a>(tasks: &'a [Task], caller: &Caller, name: &Value) -> Result<&'a Task> {
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
        Some(name) => ApiError::invalid(format!("This project has no task {name}")),
        None => ApiError::invalid("Name the task, as the tasks tool lists them"),
    })
}

/// What an agent gave for a task, read as a person's request is. An agent gives a task to
/// nobody and doesn't say which pages it touches.
fn fields(given: &Value) -> Result<TaskFields> {
    let read = serde_json::from_value::<TaskFields>(given.clone());
    let fields =
        read.map_err(|error| ApiError::invalid(format!("That can't be read as a task: {error}")))?;
    Ok(TaskFields {
        owner_id: None,
        docs: None,
        ..fields
    })
}

pub async fn call(state: &AppState, caller: &Caller, name: &str, given: &Value) -> Result<String> {
    let pool = &state.pool;
    let place = (caller.team_id.as_str(), caller.project_id.as_str());
    let tasks = async || db::tasks::list(pool, place.0, place.1).await;
    // Says in the agent's chat, if it has one, what it did to a task, and answers the agent.
    let wrote = async |id: &str, number: i64, title: &str, made: bool| {
        let name = format!("{}-{number}", caller.key);
        let body =
            json!({"kind": "task", "task_id": id, "name": format!("{name} {title}"), "made": made});
        if let Some(chat) = &caller.chat_id {
            db::chats::note(pool, chat, &body).await?;
            live::chat(state, chat).await;
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
            let docs = db::docs::list(pool, place.1).await?;
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
            let (id, number) = tasks::create(
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
            let fields = tasks::update(state, place, &task.id, fields(given)?).await?;
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
            let docs = db::docs::list(pool, place.1).await?;
            let title = given["title"].as_str().unwrap_or_default().trim();
            match docs
                .iter()
                .find(|doc| doc.title.eq_ignore_ascii_case(title))
            {
                Some(doc) => Ok(format!("# {}\n\n{}", doc.title, doc.markdown)),
                None => {
                    let titles: Vec<&str> = docs.iter().map(|doc| doc.title.as_str()).collect();
                    Err(ApiError::invalid(format!(
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
                return Err(ApiError::invalid(
                    "A proposal needs the page's title, its text and why",
                ));
            }
            let docs = db::docs::list(pool, place.1).await?;
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
                return Err(ApiError::invalid("That is the page's text already"));
            }
            let section = match (page, said("section")) {
                (Some(page), _) => &page.section,
                (None, "works") => "works",
                (None, _) => "built",
            };
            let title = page.map_or(title, |page| &page.title);
            db::proposals::propose(pool, caller, page, section, title, markdown, reason).await?;
            live::team(state, place.0, Topic::Docs).await;
            if let Some(chat) = &caller.chat_id {
                let body = json!({"kind": "proposal", "title": title, "new": page.is_none()});
                db::chats::note(pool, chat, &body).await?;
                live::chat(state, chat).await;
            }
            Ok(format!(
                "Proposed. \"{title}\" changes when a person of the team accepts it on the docs page."
            ))
        }
        _ => Err(ApiError::invalid("There is no such tool")),
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
