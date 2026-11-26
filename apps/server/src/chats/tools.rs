use serde_json::{Value, json};

use super::db;
use super::db::Caller;
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::tasks::db::{Task, TaskFields};
use crate::{AppState, docs, live, tasks};

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

pub async fn call(state: &AppState, caller: &Caller, name: &str, given: &Value) -> Result<String> {
    let tools = Tools { state, caller };
    let text = |name: &str| given[name].as_str().unwrap_or_default().trim();
    match name {
        "tasks" => tools.tasks().await,
        "task" => tools.task(&given["name"]).await,
        "new_task" => tools.new_task(given).await,
        "change_task" => tools.change_task(given).await,
        "doc" => tools.doc(text("title")).await,
        "propose_doc" => {
            let page = (text("title"), text("markdown"), text("section"));
            tools.propose_doc(page, text("reason")).await
        }
        "search_docs" => tools.search_docs(text("about")).await,
        _ => Err(ApiError::invalid("There is no such tool")),
    }
}

struct Tools<'a> {
    state: &'a AppState,
    caller: &'a Caller,
}

impl Tools<'_> {
    async fn tasks(&self) -> Result<String> {
        let tasks = self.all_tasks().await?;
        let line = |task: &Task| {
            let (name, owner) = (self.name(task), nobody(&task.owner));
            format!("{name} · {} · {} · {owner}", task.status, task.title)
        };
        Ok(tasks.iter().map(line).collect::<Vec<_>>().join("\n"))
    }

    async fn task(&self, name: &Value) -> Result<String> {
        let tasks = self.all_tasks().await?;
        let task = self.named(&tasks, name)?;
        let docs = docs::db::list(&self.state.pool, &self.caller.project_id).await?;
        let pages = docs.iter().filter(|doc| task.docs.contains(&doc.id));
        let pages: Vec<String> = pages
            .map(|doc| format!("{}{}", doc.title, written_from(&doc.files)))
            .collect();
        let checks: Vec<String> = task
            .checks
            .iter()
            .enumerate()
            .map(|(at, check)| {
                let ticked = if check.done { "x" } else { " " };
                format!("{}. [{ticked}] {}", at + 1, check.text)
            })
            .collect();
        Ok(format!(
            "{} {}\nStatus: {} · Owner: {}\n\n{}\n\nDone when:\n{}\n\n\
             Docs it touches, each read with the doc tool: {}",
            self.name(task),
            task.title,
            task.status,
            nobody(&task.owner),
            task.text,
            checks.join("\n"),
            pages.join("; "),
        ))
    }

    async fn new_task(&self, given: &Value) -> Result<String> {
        let fields = fields(given)?;
        let title = fields.title.clone().unwrap_or_default();
        let (user, chat) = (&self.caller.user_id, self.caller.chat_id.as_deref());
        let (id, number) = tasks::create(self.state, self.place(), user, chat, fields).await?;
        self.wrote(&id, number, title.trim(), true).await
    }

    async fn change_task(&self, given: &Value) -> Result<String> {
        let tasks = self.all_tasks().await?;
        let task = self.named(&tasks, &given["name"])?;
        let fields = tasks::update(self.state, self.place(), &task.id, fields(given)?).await?;
        let title = fields.title.as_ref().unwrap_or(&task.title);
        self.wrote(&task.id, task.number, title, false).await
    }

    async fn doc(&self, title: &str) -> Result<String> {
        let docs = docs::db::list(&self.state.pool, &self.caller.project_id).await?;
        let Some(doc) = docs
            .iter()
            .find(|doc| doc.title.eq_ignore_ascii_case(title))
        else {
            let titles: Vec<&str> = docs.iter().map(|doc| doc.title.as_str()).collect();
            let message = format!(
                "No page is called that. The pages are: {}",
                titles.join("; ")
            );
            return Err(ApiError::invalid(message));
        };
        Ok(format!("# {}\n\n{}", doc.title, doc.markdown))
    }

    async fn propose_doc(
        &self,
        (title, markdown, section): (&str, &str, &str),
        reason: &str,
    ) -> Result<String> {
        if title.is_empty() || markdown.is_empty() || reason.is_empty() {
            return Err(ApiError::invalid(
                "A proposal needs the page's title, its text and why",
            ));
        }
        let (state, caller) = (self.state, self.caller);
        let docs = docs::db::list(&state.pool, &caller.project_id).await?;
        let page = docs
            .iter()
            .find(|doc| doc.title.eq_ignore_ascii_case(title));
        let headed = markdown.split_once('\n').filter(|(first, _)| {
            let first = first.strip_prefix("# ").unwrap_or_default().trim();
            first.eq_ignore_ascii_case(title)
        });
        let markdown = headed.map_or(markdown, |(_, text)| text.trim());
        if page.is_some_and(|page| page.markdown.trim() == markdown) {
            return Err(ApiError::invalid("That is the page's text already"));
        }
        let section = match (page, section) {
            (Some(page), _) => &page.section,
            (None, "works") => "works",
            (None, _) => "built",
        };
        let title = page.map_or(title, |page| &page.title);
        docs::db::propose(&state.pool, caller, page, section, title, markdown, reason).await?;
        live::team(state, &caller.team_id, Topic::Docs).await;
        self.note(&json!({"kind": "proposal", "title": title, "new": page.is_none()}))
            .await?;
        Ok(format!(
            "Proposed. \"{title}\" changes when a person of the team accepts it on the docs page."
        ))
    }

    async fn search_docs(&self, about: &str) -> Result<String> {
        let parts = docs::search::search(self.state, &self.caller.project_id, about).await?;
        let shown = parts.iter().take(5).map(|part| {
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

    fn place(&self) -> (&str, &str) {
        (&self.caller.team_id, &self.caller.project_id)
    }

    fn name(&self, task: &Task) -> String {
        format!("{}-{}", self.caller.key, task.number)
    }

    async fn all_tasks(&self) -> Result<Vec<Task>> {
        let (team_id, project_id) = self.place();
        Ok(tasks::db::list(&self.state.pool, team_id, project_id).await?)
    }

    fn named<'t>(&self, tasks: &'t [Task], name: &Value) -> Result<&'t Task> {
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
                .find(|task| Some(&task.id) == self.caller.task_id.as_ref()),
        };
        found.ok_or_else(|| match name.as_str() {
            Some(name) => ApiError::invalid(format!("This project has no task {name}")),
            None => ApiError::invalid("Name the task, as the tasks tool lists them"),
        })
    }

    async fn wrote(&self, id: &str, number: i64, title: &str, made: bool) -> Result<String> {
        let name = format!("{}-{number}", self.caller.key);
        let named = format!("{name} {title}");
        self.note(&json!({"kind": "task", "task_id": id, "name": named, "made": made}))
            .await?;
        Ok(format!("{} {name}.", if made { "Made" } else { "Changed" }))
    }

    async fn note(&self, body: &Value) -> Result<()> {
        if let Some(chat) = &self.caller.chat_id {
            db::note(&self.state.pool, chat, body).await?;
            live::chat(self.state, chat).await;
        }
        Ok(())
    }
}

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

fn nobody(owner: &Option<String>) -> &str {
    owner.as_deref().unwrap_or("nobody")
}

fn written_from(files: &str) -> String {
    let files: Vec<String> = serde_json::from_str(files).unwrap_or_default();
    if files.is_empty() {
        return String::new();
    }
    format!(" (written from {})", files.join(", "))
}
