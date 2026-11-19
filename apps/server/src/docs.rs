use std::collections::HashMap;
use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, MapFile, User};
use crate::teams::{membership, refused};
use crate::writer::Writer;
use crate::{ApiError, AppState, now};

/// At most this many pages in each of "How it works" and "How it's built".
const PAGES: usize = 8;
/// How much code one page may read, in characters.
const PAGE_CHARS: usize = 150_000;
/// A repository whose outline is longer than this is refused.
const OUTLINE_CHARS: usize = 400_000;
/// More symbols than this in one file say little, as in a minified bundle.
const SYMBOLS_PER_FILE: usize = 40;

/// What a run adds under a page about its files.
const WRITTEN_FROM: &str = "\n\nWritten from: ";
const NOT_READ: &str = "\n\nNot read, for lack of room: ";

const WRITING: &str = "\
Write for a person. Use plain sentences and short paragraphs, explain a term the first time you \
use it, and start with what the reader most needs. Leave out anything that only restates the code.

Describe only what the code you were given shows. Where it doesn't settle something, say so in a \
sentence; don't guess.

Answer with the page in Markdown and nothing else. Don't repeat the page's title as a heading, \
and add no preamble or closing remark.";

const OUTLINE: &str = "\
You plan the documentation of a software project. You are given its README and an outline of its \
repository: every file, and for files in a language we parse, their first symbols.

The documentation is about the project's own code. Repositories also hold code the project didn't \
write: a framework's scaffolding left as generated, vendored packages, build output, lock files. \
First decide which folders hold such code.

Then plan two sets of pages, using only the project's own files:

- \"works\": pages for the people who use the product. Each is one thing a user does from start \
to finish, such as signing up or exporting a report. Choose the files that show it: routes, \
screens, and the code behind them.
- \"built\": pages for the people who work on the code. Each is one area of the code with a \
clear responsibility. Choose the files a developer must read to understand it.

Plan at most 8 pages in each set, fewer for a small project, and none where the code shows \
nothing to describe. Give each page the files it needs and no more, as paths exactly as they \
appear in the outline.

Answer with JSON and nothing else, in this shape:

{\"left_out\": [\"folder\", ...],
 \"works\": [{\"title\": \"...\", \"summary\": \"one sentence on what the page covers\", \"files\": [\"path\", ...]}],
 \"built\": [{\"title\": \"...\", \"summary\": \"...\", \"files\": [\"path\", ...]}]}";

const WORKS: &str = "\
You write one page of a product's documentation for the people who use it. The page describes one \
flow: what a user does and what they get.

Tell it as numbered steps: the user does this, clicks that, and sees or receives this result. Use \
the words the product's own screens use for buttons, pages and messages. Say what happens when \
something goes wrong if the code shows it.

This reader doesn't read code. Name no file, function, class, database table or framework.";

const BUILT: &str = "\
You write one page of a project's technical documentation for the developers who work on it. \
Explain this area of the code the way a colleague would at a whiteboard. Don't list its functions.

Use these parts, each under a level-two heading, and leave a part out when you have nothing true \
to put in it:

## What this part is for
Two or three sentences.

## How it flows
The main paths through the code, told in order: what starts each one, what happens next and why, \
and where the data ends up. Mention a function when the story passes through it, not because it \
exists.

## Handle with care
The pieces that matter more than they look: something many other parts depend on, a check that \
guards money or access, an order of steps that must not change. For each, say why it matters and \
what would break if it were changed carelessly, or why it is safe to change if it looks \
frightening.

## Possible problems
Weaknesses or bugs the code itself shows: a missing check, a case that isn't handled, two places \
that disagree. For each, say where it is and why you think so, and make clear it is your reading \
of the code, not a confirmed bug. Never invent a problem to fill this part.

The files are given with line numbers. Back every claim about the code with its place, written \
as `path:line` in backticks, using the path exactly as given.

This page is about the project's own code. Mention the framework or a library only where the \
reader needs it to follow the flow, in a sentence, and don't explain how the framework works.";

const OVERVIEW: &str = "\
You write the overview page of a project's documentation, from its README and the pages already \
written about it.

Use these parts, each under a level-two heading:

## What it is
What the product is for and who uses it, in a short paragraph.

## How the code is laid out
The main areas of the project's own code and how they fit together, in a few short paragraphs.

## Worth knowing first
The few cautions and possible problems from the pages that matter most, at most five, each in a \
sentence that names the page it comes from in bold. Leave this part out if the pages raise none.";

#[derive(Deserialize)]
struct Outline {
    #[serde(default)]
    left_out: Vec<String>,
    #[serde(default)]
    works: Vec<Planned>,
    #[serde(default)]
    built: Vec<Planned>,
}

#[derive(Deserialize)]
struct Planned {
    title: String,
    summary: String,
    #[serde(default)]
    files: Vec<String>,
}

/// Every file with its language and length, and the first symbols of the files we parse.
fn outline(files: &[MapFile]) -> String {
    let mut outline = String::new();
    for file in files {
        let language = file.language.unwrap_or("not parsed");
        writeln!(outline, "{} ({language}, {} lines)", file.path, file.lines).ok();
        for symbol in file.symbols.iter().take(SYMBOLS_PER_FILE) {
            writeln!(
                outline,
                "  {} {} :{}",
                symbol.kind, symbol.name, symbol.line
            )
            .ok();
        }
    }
    outline
}

/// The page's files with numbered lines, as far as they fit, and the paths that didn't.
fn numbered(paths: &[String], texts: &HashMap<String, String>) -> (String, Vec<String>) {
    let (mut code, mut unread) = (String::new(), Vec::new());
    for path in paths {
        let text = &texts[path];
        if code.len() + text.len() > PAGE_CHARS {
            unread.push(path.clone());
            continue;
        }
        writeln!(code, "=== {path} ===").ok();
        for (number, line) in text.lines().enumerate() {
            writeln!(code, "{}| {line}", number + 1).ok();
        }
    }
    (code, unread)
}

/// Writes the docs of a branch and saves them, or returns why it couldn't.
async fn generate(
    state: &AppState,
    writer: &Writer,
    team_id: &str,
    project_id: &str,
    run: i64,
    branch: &str,
) -> Result<(), String> {
    let broken = |error: sqlx::Error| {
        tracing::error!("database error while writing docs: {error}");
        "Something went wrong".to_owned()
    };
    let progress = async |message: String| {
        let saved = db::set_docs_state(&state.pool, project_id, run, "running", Some(&message));
        // A run that lost its project stops here, before it asks the writer for more.
        let held = saved.await.map_err(broken)?;
        held.then_some(()).ok_or("The project changed".to_owned())
    };
    let sources = db::sources(&state.pool, team_id, project_id, branch).await;
    let (map, texts) = sources.map_err(broken)?.ok_or("This branch isn't mapped")?;
    let commit = map.commit.ok_or("This branch isn't mapped yet")?;
    let readme = map
        .files
        .iter()
        .find(|file| file.path.eq_ignore_ascii_case("README.md"));
    let readme = readme
        .and_then(|file| texts.get(&file.path))
        .map_or("(none)", String::as_str);

    progress("Planning the pages".into()).await?;
    let repository = outline(&map.files);
    if repository.len() > OUTLINE_CHARS {
        return Err("This repository is too large to write docs for yet".into());
    }
    let prompt = format!("README:\n{readme}\n\nRepository outline:\n{repository}");
    let answer = writer.ask(state, OUTLINE, &prompt).await?;
    // A writer may wrap the JSON in a sentence or a code fence.
    let json = answer
        .find('{')
        .zip(answer.rfind('}'))
        .filter(|(from, to)| from < to)
        .map(|(from, to)| &answer[from..=to]);
    let plan: Outline = serde_json::from_str(json.unwrap_or_default())
        .map_err(|error| format!("The writer's plan couldn't be read: {error}"))?;

    let own = |path: &String| {
        let inside = |folder: &String| path.starts_with(&format!("{}/", folder.trim_matches('/')));
        texts.contains_key(path) && !plan.left_out.iter().any(inside)
    };
    let planned = [("works", WORKS, plan.works), ("built", BUILT, plan.built)];
    let planned: Vec<(&str, &str, Planned)> = planned
        .into_iter()
        .flat_map(|(section, rules, pages)| {
            let pages = pages.into_iter().take(PAGES);
            pages.map(move |page| (section, rules, page))
        })
        .collect();
    let paths: String = map
        .files
        .iter()
        .map(|file| format!("{}\n", file.path))
        .collect();

    let mut pages = Vec::new();
    for (index, (section, rules, page)) in planned.iter().enumerate() {
        progress(format!("Writing page {} of {}", index + 1, planned.len())).await?;
        let mut files: Vec<String> = page
            .files
            .iter()
            .filter(|path| own(path))
            .cloned()
            .collect();
        let (code, unread) = numbered(&files, &texts);
        let prompt = format!(
            "Page: {}\nIt covers: {}\n\nFolders that aren't the project's own code: {:?}\n\n\
             Every file in the repository:\n{paths}\nThe files for this page:\n{code}",
            page.title, page.summary, plan.left_out
        );
        let system = format!("{rules}\n\n{WRITING}");
        let mut markdown = writer.ask(state, &system, &prompt).await?;
        files.retain(|path| !unread.contains(path));
        if *section == "works" && !files.is_empty() {
            write!(markdown, "{WRITTEN_FROM}{}", files.join(", ")).ok();
        }
        if !unread.is_empty() {
            write!(markdown, "{NOT_READ}{}", unread.join(", ")).ok();
        }
        pages.push((*section, page.title.clone(), markdown));
    }

    progress("Writing the overview".into()).await?;
    let written: String = pages
        .iter()
        .map(|(_, title, markdown)| format!("=== {title} ===\n{markdown}\n\n"))
        .collect();
    let prompt = format!("README:\n{readme}\n\nThe pages:\n{written}");
    let system = format!("{OVERVIEW}\n\n{WRITING}");
    let overview = writer.ask(state, &system, &prompt).await?;
    pages.insert(0, ("overview", "Overview".to_owned(), overview));

    let left_out = json!(plan.left_out).to_string();
    let saved = db::save_docs(
        &state.pool,
        project_id,
        run,
        branch,
        &commit,
        &left_out,
        &pages,
    );
    saved.await.map_err(broken)
}

#[derive(Deserialize)]
pub struct Start {
    branch: String,
}

/// Starts a run in the background.
pub async fn start(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(start): Json<Start>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    let commit = db::branch_commit(&state.pool, &team_id, &project_id, &start.branch).await?;
    if commit.ok_or_else(crate::not_found)?.is_none() {
        return Err(refused("This branch isn't mapped yet"));
    }
    let Some(writer) = db::writer(&state.pool, &team_id).await? else {
        return Err(refused("Choose a docs writer in Team settings first"));
    };
    let run = rand::random::<u32>().into();
    if !db::claim_docs_run(&state.pool, &project_id, run).await? {
        return Err(refused("The docs are already being written"));
    }
    tokio::spawn(async move {
        let (pool, project) = (state.pool.clone(), project_id.clone());
        let writing = tokio::spawn(async move {
            generate(&state, &writer, &team_id, &project, run, &start.branch).await
        });
        // A run that crashes must not stay marked as running.
        let message = match writing.await {
            Ok(Ok(())) => return,
            Ok(Err(message)) => message,
            Err(_) => "Something went wrong".into(),
        };
        let failed = db::set_docs_state(&pool, &project_id, run, "failed", Some(&message));
        failed.await.ok();
    });
    Ok(StatusCode::ACCEPTED)
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let status = db::docs_status(&state.pool, &team_id, &project_id).await?;
    let status = status.ok_or_else(crate::not_found)?;
    let pages = db::docs(&state.pool, &project_id).await?;
    let left_out: Value =
        serde_json::from_str(status.left_out.as_deref().unwrap_or("[]")).unwrap_or_default();
    Ok(Json(json!({
        "state": status.state, "message": status.message,
        "branch": status.branch, "commit": status.commit,
        "left_out": left_out, "pages": pages,
    })))
}

/// A page without the lines a run added about its files, which a public link must not show.
fn without_files(markdown: &str) -> &str {
    let added = [WRITTEN_FROM, NOT_READ];
    let ends = added.iter().filter_map(|line| markdown.rfind(line));
    &markdown[..ends.min().unwrap_or(markdown.len())]
}

/// Makes a public link to a copy of the project's user docs.
pub async fn share(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    membership(&user, &team_id)?;
    let pages = db::docs(&state.pool, &project_id).await?;
    let pages: Vec<(&str, &str)> = pages
        .iter()
        .filter(|page| page.section == "works")
        .map(|page| (page.title.as_str(), without_files(&page.markdown)))
        .collect();
    if pages.is_empty() {
        return Err(refused("This project has no user docs to share"));
    }
    let token = format!("{:032x}", rand::random::<u128>());
    let shared = db::share_docs(
        &state.pool,
        &team_id,
        &project_id,
        &token,
        &user.id,
        now(),
        &pages,
    );
    if !shared.await? {
        return Err(crate::not_found());
    }
    Ok(Json(json!({"token": token})))
}

pub async fn shares(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
) -> Result<Json<Vec<db::Share>>, ApiError> {
    membership(&user, &team_id)?;
    Ok(Json(db::shares(&state.pool, &team_id, &project_id).await?))
}

pub async fn end_share(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id, token)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    if !db::end_share(&state.pool, &team_id, &project_id, &token).await? {
        return Err(crate::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// What a public link shows, to anyone who has it.
pub async fn shared(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<db::Shared>, ApiError> {
    let shared = db::shared(&state.pool, &token).await?;
    Ok(Json(shared.ok_or_else(crate::not_found)?))
}
