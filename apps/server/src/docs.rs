use std::collections::HashMap;
use std::fmt::Write as _;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{self, Doc, MapFile, User, Written};
use crate::teams::{membership, refused};
use crate::writer::Writer;
use crate::{ApiError, AppState, live, now};

/// At most this many pages in each of "How it works" and "How it's built".
const PAGES: usize = 8;
/// How much code one page may read, in characters.
const PAGE_CHARS: usize = 150_000;
/// A repository whose outline is longer than this is refused.
const OUTLINE_CHARS: usize = 400_000;
/// More symbols than this in one file say little, as in a minified bundle.
const SYMBOLS_PER_FILE: usize = 40;

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

const UPDATING: &str = "\
The page already exists, and you are given it as it is now. Keep its text word for word wherever \
the code hasn't changed, and change only what the code or the request calls for.

A person may have edited the page. When you are also given the page as you last wrote it, what \
differs between the two is a person's words. Never change a person's sentence silently: keep it, \
and where the code shows it to be wrong, say so in a note.";

const TAIL: &str = "\
After the page, write a line that says only ---REASON--- and then one sentence on what you \
changed or added and why. Then write a line that says only ---NOTES--- and then a JSON list, [] \
when you have nothing to note, with one entry for each sentence a person wrote that the code \
shows to be wrong:

[{\"quote\": \"the person's sentence, exactly as it stands in the page\", \
\"says\": \"what the code does instead\", \"place\": \"path:line\", \
\"instead\": \"the sentence corrected, when you can offer one\"}]";

const REQUEST: &str = "\
Someone asks for a change to a project's documentation. You are given what they ask, the pages \
that exist, the page they had open if any, and an outline of the repository: every file, and for \
files in a language we parse, their first symbols.

Decide which one page to change, or that a new page is needed, and which files must be read to \
write it, as paths exactly as they appear in the outline. A page in the set \"works\" is for the \
people who use the product; one in \"built\" is for the developers who work on its code.

Answer with JSON and nothing else, in this shape:

{\"page\": \"the id of the page to change, or null for a new page\", \
\"section\": \"works or built, for a new page\", \"title\": \"the page's title\", \
\"summary\": \"one sentence on what the page must cover\", \"files\": [\"path\", ...]}";

const FILES: &str = "\
You are given one page of a project's documentation, which a person wrote, and an outline of the \
repository. Say which files the page is about: the ones to read to check what it says, as paths \
exactly as they appear in the outline, at most ten.

Answer with JSON and nothing else: {\"files\": [\"path\", ...]}";

const CHECK: &str = "\
You check one page of a project's documentation against the code it is about. A person edited \
the page. When you are also given the page as you last wrote it, what differs between the two is \
the person's words; otherwise the whole page is theirs.

Find the sentences a person wrote that the code shows to be wrong. Leave alone what the code \
doesn't settle, and matters of wording or taste. Each note is your reading of the code, not a \
verdict.

Answer with a JSON list and nothing else, [] when the person's words agree with the code:

[{\"quote\": \"the person's sentence, exactly as it stands in the page\", \
\"says\": \"what the code does instead\", \"place\": \"path:line\", \
\"instead\": \"the sentence corrected, when you can offer one\"}]";

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

/// Which page a request is about. A writer leaves out, or gives as null, what it has nothing
/// to say about, such as the set of a page that exists.
#[derive(Deserialize)]
struct Target {
    page: Option<String>,
    section: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    files: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct Files {
    #[serde(default)]
    files: Vec<String>,
}

/// Where the writer finds a person's sentence at odds with the code.
#[derive(Deserialize, Serialize)]
struct Note {
    quote: String,
    says: String,
    #[serde(default)]
    place: String,
    instead: Option<String>,
}

/// The JSON in an answer, which a writer may wrap in a sentence or a code fence.
fn json_in(answer: &str, open: char, close: char) -> &str {
    let ends = answer.find(open).zip(answer.rfind(close));
    ends.filter(|(from, to)| from < to)
        .map_or("", |(from, to)| &answer[from..=to])
}

/// The writer's notes as a JSON list, empty when they can't be read.
fn notes(answer: &str) -> String {
    let notes: Vec<Note> = serde_json::from_str(json_in(answer, '[', ']')).unwrap_or_default();
    json!(notes).to_string()
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

fn broken(error: sqlx::Error) -> String {
    tracing::error!("database error while writing docs: {error}");
    "Something went wrong".to_owned()
}

/// A branch as its map has it: the outline of its files, their text, and its commit.
struct Sources {
    outline: String,
    texts: HashMap<String, String>,
    commit: String,
}

async fn sources(
    state: &AppState,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> Result<Sources, String> {
    let sources = db::sources(&state.pool, team_id, project_id, branch).await;
    let (commit, files, texts) = sources.map_err(broken)?.ok_or("This branch isn't mapped")?;
    let commit = commit.ok_or("This branch isn't mapped yet")?;
    let outline = outline(&files);
    if outline.len() > OUTLINE_CHARS {
        return Err("This repository is too large to write docs for yet".into());
    }
    Ok(Sources {
        outline,
        texts,
        commit,
    })
}

/// A page to write: its set, what it must cover, and the page it replaces, if any.
struct Brief<'a> {
    section: &'a str,
    title: &'a str,
    covers: &'a str,
    files: &'a [String],
    left_out: &'a [String],
    current: Option<&'a Doc>,
    request: Option<&'a str>,
}

/// Asks the writer for one page, from the files that fit.
async fn write_page(
    state: &AppState,
    writer: &Writer,
    sources: &Sources,
    brief: Brief<'_>,
) -> Result<Written, String> {
    let mut files: Vec<String> = brief.files.to_vec();
    files.retain(|path| sources.texts.contains_key(path));
    let (code, unread) = numbered(&files, &sources.texts);
    let paths: Vec<&str> = sources.texts.keys().map(String::as_str).collect();
    let mut prompt = format!(
        "Page: {}\nIt covers: {}\n\nFolders that aren't the project's own code: {:?}\n\n\
         Every file in the repository:\n{}\n\nThe files for this page:\n{code}",
        brief.title,
        brief.covers,
        brief.left_out,
        paths.join("\n")
    );
    let rules = if brief.section == "works" {
        WORKS
    } else {
        BUILT
    };
    let mut system = format!("{rules}\n\n{WRITING}");
    if let Some(request) = brief.request {
        write!(prompt, "\n\nA person asks for this:\n{request}").ok();
    }
    if let Some(current) = brief.current {
        write!(system, "\n\n{UPDATING}").ok();
        write!(prompt, "\n\nThe page as it is now:\n{}", current.markdown).ok();
        match current.written.as_deref() {
            Some(written) if written == current.markdown => {}
            Some(written) => {
                write!(prompt, "\n\nThe page as you last wrote it:\n{written}").ok();
            }
            None => prompt.push_str("\n\nA person wrote the whole page."),
        }
    }
    // The first run on a project has nothing to compare with, so its pages come as they are.
    let tailed = brief.current.is_some() || brief.request.is_some();
    if tailed {
        write!(system, "\n\n{TAIL}").ok();
    }
    let answer = writer.ask(state, &system, &prompt).await?;
    let (page, tail) = answer.split_once("---REASON---").unwrap_or((&answer, ""));
    let (reason, noted) = tail.split_once("---NOTES---").unwrap_or((tail, ""));
    let markdown = if tailed { page.trim() } else { page }.to_owned();
    files.retain(|path| !unread.contains(path));
    let reason = Some(reason.trim()).filter(|reason| !reason.is_empty());
    Ok(Written {
        doc: brief
            .current
            .map(|doc| (doc.id.clone(), doc.version.clone())),
        section: brief.section.to_owned(),
        title: brief.title.to_owned(),
        markdown: Some(markdown),
        files: json!(files).to_string(),
        unread: json!(unread).to_string(),
        notes: notes(noted),
        reason: reason.unwrap_or(brief.covers).to_owned(),
    })
}

/// Tells where a run is, and stops a run that lost its project before it asks the writer for more.
async fn progress(
    state: &AppState,
    project_id: &str,
    run: i64,
    message: String,
) -> Result<(), String> {
    let saved = db::set_docs_state(&state.pool, project_id, run, "running", Some(&message));
    let held = saved.await.map_err(broken)?;
    live::project(state, project_id, "docs").await;
    held.then_some(()).ok_or("The project changed".to_owned())
}

/// Writes the docs of a branch: as pages when the project has none, else as proposals to change them.
async fn generate(
    state: &AppState,
    writer: &Writer,
    team_id: &str,
    project_id: &str,
    run: i64,
    branch: &str,
) -> Result<(), String> {
    let sources = sources(state, team_id, project_id, branch).await?;
    let existing = db::docs(&state.pool, project_id).await.map_err(broken)?;
    let readme = sources.texts.iter().find_map(|(path, text)| {
        path.eq_ignore_ascii_case("README.md")
            .then_some(text.as_str())
    });
    let readme = readme.unwrap_or("(none)");

    progress(state, project_id, run, "Planning the pages".into()).await?;
    let mut prompt = format!(
        "README:\n{readme}\n\nRepository outline:\n{}",
        sources.outline
    );
    if !existing.is_empty() {
        prompt.push_str(
            "\nThe pages that exist now. Plan a page that still covers the same thing under \
             the same title, in the same set:\n",
        );
        for doc in &existing {
            writeln!(prompt, "{}: {}", doc.section, doc.title).ok();
        }
    }
    let answer = writer.ask(state, OUTLINE, &prompt).await?;
    let plan: Outline = serde_json::from_str(json_in(&answer, '{', '}'))
        .map_err(|error| format!("The writer's plan couldn't be read: {error}"))?;

    let own = |path: &String| {
        let inside = |folder: &String| path.starts_with(&format!("{}/", folder.trim_matches('/')));
        !plan.left_out.iter().any(inside)
    };
    let planned = [("works", plan.works), ("built", plan.built)];
    let planned: Vec<(&'static str, Planned)> = planned
        .into_iter()
        .flat_map(|(section, pages)| {
            pages
                .into_iter()
                .take(PAGES)
                .map(move |page| (section, page))
        })
        .collect();

    let mut pages = Vec::new();
    for (index, (section, page)) in planned.iter().enumerate() {
        let message = format!("Writing page {} of {}", index + 1, planned.len());
        progress(state, project_id, run, message).await?;
        let files: Vec<String> = page
            .files
            .iter()
            .filter(|path| own(path))
            .cloned()
            .collect();
        let same = |doc: &&Doc| doc.section == *section && doc.title == page.title;
        let brief = Brief {
            section,
            title: &page.title,
            covers: &page.summary,
            files: &files,
            left_out: &plan.left_out,
            current: existing.iter().find(same),
            request: None,
        };
        pages.push(write_page(state, writer, &sources, brief).await?);
    }

    progress(state, project_id, run, "Writing the overview".into()).await?;
    let written: String = pages
        .iter()
        .map(|page| {
            let markdown = page.markdown.as_deref().unwrap_or_default();
            format!("=== {} ===\n{markdown}\n\n", page.title)
        })
        .collect();
    let mut prompt = format!("README:\n{readme}\n\nThe pages:\n{written}");
    let mut system = format!("{OVERVIEW}\n\n{WRITING}");
    let current = existing.iter().find(|doc| doc.section == "overview");
    if let Some(current) = current {
        write!(system, "\n\n{UPDATING}").ok();
        write!(prompt, "\nThe page as it is now:\n{}", current.markdown).ok();
    }
    let overview = writer.ask(state, &system, &prompt).await?;
    pages.insert(
        0,
        Written {
            doc: current.map(|doc| (doc.id.clone(), doc.version.clone())),
            section: "overview".to_owned(),
            title: "Overview".to_owned(),
            markdown: Some(overview),
            files: "[]".into(),
            unread: "[]".into(),
            notes: "[]".into(),
            reason: "Written again from the pages as they would be".into(),
        },
    );

    // A page whose text would stay as it is needs no proposal.
    let text = |id: &String| {
        existing
            .iter()
            .find(|doc| doc.id == *id)
            .map(|doc| &doc.markdown)
    };
    pages.retain(|page| match &page.doc {
        Some((id, _)) => text(id) != page.markdown.as_ref(),
        None => true,
    });
    for doc in &existing {
        let files: Vec<String> = serde_json::from_str(&doc.files).unwrap_or_default();
        let planned = planned
            .iter()
            .any(|(section, page)| doc.section == *section && doc.title == page.title);
        let gone = files.iter().all(|path| !sources.texts.contains_key(path));
        // A page a person made has no files, and is never proposed for removal.
        if !planned && !files.is_empty() && gone {
            pages.push(Written {
                doc: Some((doc.id.clone(), doc.version.clone())),
                section: doc.section.clone(),
                title: doc.title.clone(),
                markdown: None,
                files: "[]".into(),
                unread: "[]".into(),
                notes: "[]".into(),
                reason: "The files it was written from are no longer in the repository".into(),
            });
        }
    }

    let left_out = json!(plan.left_out).to_string();
    let (commit, from) = (&sources.commit, Some((branch, left_out.as_str())));
    let saved = db::save_written(&state.pool, project_id, run, commit, from, None, &pages);
    saved.await.map_err(broken)
}

/// Does `work` in the background as the project's `run`, and marks the run failed if it fails.
fn background(
    state: AppState,
    project_id: String,
    run: i64,
    work: impl Future<Output = Result<(), String>> + Send + 'static,
) {
    tokio::spawn(async move {
        // A run that crashes must not stay marked as running.
        let failure = match tokio::spawn(work).await {
            Ok(Ok(())) => None,
            Ok(Err(message)) => Some(message),
            Err(_) => Some("Something went wrong".into()),
        };
        if let Some(message) = failure {
            let failed =
                db::set_docs_state(&state.pool, &project_id, run, "failed", Some(&message));
            failed.await.ok();
        }
        live::project(&state, &project_id, "docs").await;
    });
}

/// The team's writer, writing for `user`, and a run that holds the project, to write from
/// `branch`, or why there is none.
async fn claim(
    state: &AppState,
    user: &User,
    team_id: &str,
    project_id: &str,
    branch: &str,
) -> Result<(Writer, i64), ApiError> {
    let commit = db::branch_commit(&state.pool, team_id, project_id, branch).await?;
    if commit.ok_or_else(crate::not_found)?.is_none() {
        return Err(refused("This branch isn't mapped yet"));
    }
    let Some(mut writer) = db::writer(&state.pool, team_id).await? else {
        return Err(refused("Choose a docs writer in Team settings first"));
    };
    writer.asker = user.id.clone();
    let run = rand::random::<u32>().into();
    if !db::claim_docs_run(&state.pool, project_id, run).await? {
        return Err(refused("The docs are already being written"));
    }
    live::project(state, project_id, "docs").await;
    Ok((writer, run))
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
    let (writer, run) = claim(&state, &user, &team_id, &project_id, &start.branch).await?;
    background(state.clone(), project_id.clone(), run, async move {
        generate(&state, &writer, &team_id, &project_id, run, &start.branch).await
    });
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct Request {
    /// What the person asks for.
    text: String,
    /// The page they had open.
    page: Option<String>,
    branch: String,
    /// A request they made before and the proposal they rejected, to do better this time.
    earlier: Option<String>,
}

/// Has the writer answer a request with one proposal, in the background.
pub async fn ask(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Json(request): Json<Request>,
) -> Result<StatusCode, ApiError> {
    membership(&user, &team_id)?;
    if request.text.trim().is_empty() {
        return Err(refused("Say what to change"));
    }
    let (writer, run) = claim(&state, &user, &team_id, &project_id, &request.branch).await?;
    background(state.clone(), project_id.clone(), run, async move {
        answer(&state, &writer, &team_id, &project_id, run, &request).await
    });
    Ok(StatusCode::ACCEPTED)
}

/// Finds the page a request is about, writes it, and saves the result as a proposal.
async fn answer(
    state: &AppState,
    writer: &Writer,
    team_id: &str,
    project_id: &str,
    run: i64,
    request: &Request,
) -> Result<(), String> {
    progress(state, project_id, run, "Reading the request".into()).await?;
    let sources = sources(state, team_id, project_id, &request.branch).await?;
    let existing = db::docs(&state.pool, project_id).await.map_err(broken)?;
    let mut asked = request.text.trim().to_owned();
    if let Some(earlier) = &request.earlier {
        write!(
            asked,
            "\n\nAn earlier try that the person rejected:\n{earlier}"
        )
        .ok();
    }
    let mut prompt = format!("The request:\n{asked}\n\nThe pages, as id, set and title:\n");
    for doc in &existing {
        writeln!(prompt, "{} | {} | {}", doc.id, doc.section, doc.title).ok();
    }
    let open = |id: &String| existing.iter().find(|doc| doc.id == *id);
    if let Some(open) = request.page.as_ref().and_then(open) {
        let (id, text) = (&open.id, &open.markdown);
        write!(prompt, "\nThe page they had open is {id}:\n{text}\n").ok();
    }
    write!(prompt, "\nRepository outline:\n{}", sources.outline).ok();
    let found = writer.ask(state, REQUEST, &prompt).await?;
    let target: Target = serde_json::from_str(json_in(&found, '{', '}'))
        .map_err(|error| format!("The writer's answer couldn't be read: {error}"))?;

    progress(state, project_id, run, "Writing the page".into()).await?;
    let current = target.page.as_ref().and_then(open);
    let new = match target.section.as_deref() {
        Some("works") => "works",
        _ => "built",
    };
    let title = current.map(|doc| &doc.title).or(target.title.as_ref());
    let brief = Brief {
        section: current.map_or(new, |doc| &doc.section),
        title: title.ok_or("The writer didn't say which page to write")?,
        covers: target.summary.as_deref().unwrap_or(&asked),
        files: target.files.as_deref().unwrap_or_default(),
        left_out: &[],
        current,
        request: Some(&asked),
    };
    let page = write_page(state, writer, &sources, brief).await?;
    if current.map(|doc| &doc.markdown) == page.markdown.as_ref() {
        return Err("The writer found nothing to change".into());
    }
    let (commit, asked, pages) = (&sources.commit, Some(request.text.trim()), [page]);
    let saved = db::save_written(&state.pool, project_id, run, commit, None, asked, &pages);
    saved.await.map_err(broken)
}

/// Checks a page a person just saved against the code, and keeps the writer's notes on it.
pub async fn check(
    state: AppState,
    writer: Writer,
    team_id: String,
    project_id: String,
    doc_id: String,
) {
    let Ok(docs) = db::docs(&state.pool, &project_id).await else {
        return;
    };
    let Some(doc) = docs.iter().find(|doc| doc.id == doc_id) else {
        return;
    };
    let checked = async {
        // A text that is all the writer's own holds no person's words to check.
        if doc.written.as_ref() == Some(&doc.markdown) {
            return Ok((doc.files.clone(), "[]".to_owned()));
        }
        let branch = db::check_branch(&state.pool, &project_id).await;
        let branch = branch.map_err(broken)?.ok_or("No branch is mapped yet")?;
        let sources = sources(&state, &team_id, &project_id, &branch).await?;
        let mut files: Vec<String> = serde_json::from_str(&doc.files).unwrap_or_default();
        files.retain(|path| sources.texts.contains_key(path));
        let page = format!("The page, {}:\n{}", doc.title, doc.markdown);
        // A page a person made has no files yet, so the writer says which it is about.
        if files.is_empty() {
            let prompt = format!("{page}\n\nRepository outline:\n{}", sources.outline);
            let answer = writer.ask(&state, FILES, &prompt).await?;
            let named: Files = serde_json::from_str(json_in(&answer, '{', '}')).unwrap_or_default();
            files = named.files;
            files.retain(|path| sources.texts.contains_key(path));
        }
        let (code, unread) = numbered(&files, &sources.texts);
        files.retain(|path| !unread.contains(path));
        let mut prompt = format!("{page}\n\nThe files it is about:\n{code}");
        if let Some(written) = &doc.written {
            write!(prompt, "\nThe page as you last wrote it:\n{written}").ok();
        }
        let answer = writer.ask(&state, CHECK, &prompt).await?;
        Ok::<_, String>((json!(files).to_string(), notes(&answer)))
    };
    let checked = checked.await;
    let (files, notes) = match &checked {
        Ok((files, notes)) => (Some(files.as_str()), Some(notes.as_str())),
        Err(error) => {
            tracing::warn!("a page couldn't be checked against the code: {error}");
            (None, None)
        }
    };
    let ended = db::end_check(&state.pool, &doc_id, &doc.version, files, notes);
    ended.await.ok();
    live::project(&state, &project_id, "docs").await;
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
    let proposals = db::proposals(&state.pool, &project_id).await?;
    let left_out: Value =
        serde_json::from_str(status.left_out.as_deref().unwrap_or("[]")).unwrap_or_default();
    Ok(Json(json!({
        "state": status.state, "message": status.message,
        "branch": status.branch, "commit": status.commit,
        "left_out": left_out, "pages": pages, "proposals": proposals,
    })))
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
        .map(|page| (page.title.as_str(), page.markdown.as_str()))
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
