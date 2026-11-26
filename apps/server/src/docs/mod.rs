pub mod db;
pub mod http;
pub mod jobs;
pub mod prompts;
pub mod search;

use std::collections::HashMap;
use std::fmt::Write as _;

use prompts::{BUILT, TAIL, UPDATING, WORKS, WRITING};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::docs::db::{Doc, Written};
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::projects::db::MapFile;
use crate::settings::writer::Writer;
use crate::{AppState, live, projects, settings};

const PAGE_CHARS: usize = 150_000;
const OUTLINE_CHARS: usize = 400_000;
const SYMBOLS_PER_FILE: usize = 40;

pub async fn writer(state: &AppState, team_id: &str, user_id: &str) -> Result<Writer> {
    let writer = settings::db::writer(&state.pool, &state.server_key, team_id).await?;
    let mut writer =
        writer.ok_or_else(|| ApiError::invalid("Choose a docs writer in Team settings first"))?;
    writer.asker = user_id.to_owned();
    Ok(writer)
}

pub struct Sources {
    pub outline: String,
    pub texts: HashMap<String, String>,
    pub commit: String,
}

impl Sources {
    pub async fn of(
        state: &AppState,
        team_id: &str,
        project_id: &str,
        branch: &str,
    ) -> Result<Self> {
        let sources = projects::db::sources(&state.pool, team_id, project_id, branch).await?;
        let (commit, files, texts) =
            sources.ok_or_else(|| ApiError::invalid("This branch isn't mapped"))?;
        let commit = commit.ok_or_else(|| ApiError::invalid("This branch isn't mapped yet"))?;
        let outline = outline(&files);
        if outline.len() > OUTLINE_CHARS {
            return Err(ApiError::invalid(
                "This repository is too large to write docs for yet",
            ));
        }
        Ok(Self {
            outline,
            texts,
            commit,
        })
    }

    pub fn readme(&self) -> &str {
        let readme = self
            .texts
            .iter()
            .find(|(path, _)| path.eq_ignore_ascii_case("README.md"));
        readme.map_or("(none)", |(_, text)| text)
    }

    pub fn numbered(&self, paths: &[String]) -> (String, Vec<String>) {
        let (mut code, mut unread) = (String::new(), Vec::new());
        for path in paths {
            let text = &self.texts[path];
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
}

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

pub struct Brief<'a> {
    pub section: &'a str,
    pub title: &'a str,
    pub covers: &'a str,
    pub files: &'a [String],
    pub left_out: &'a [String],
    pub current: Option<&'a Doc>,
    pub request: Option<&'a str>,
}

pub async fn write_page(
    state: &AppState,
    writer: &Writer,
    sources: &Sources,
    brief: Brief<'_>,
) -> Result<Written> {
    let mut files: Vec<String> = brief.files.to_vec();
    files.retain(|path| sources.texts.contains_key(path));
    let (code, unread) = sources.numbered(&files);
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

pub async fn progress(state: &AppState, project_id: &str, run: i64, message: &str) -> Result<()> {
    let held = db::set_state(&state.pool, project_id, run, "running", Some(message)).await?;
    live::project(state, project_id, Topic::Docs).await;
    if !held {
        return Err(ApiError::invalid("The project changed"));
    }
    Ok(())
}

pub async fn fail_run(state: &AppState, project_id: &str, run: i64, why: &str) {
    let failed = db::set_state(&state.pool, project_id, run, "failed", Some(why));
    failed.await.ok();
    live::project(state, project_id, Topic::Docs).await;
}

pub fn json_in(answer: &str, open: char, close: char) -> &str {
    let ends = answer.find(open).zip(answer.rfind(close));
    ends.filter(|(from, to)| from < to)
        .map_or("", |(from, to)| &answer[from..=to])
}

#[derive(Deserialize, Serialize)]
struct Note {
    quote: String,
    says: String,
    #[serde(default)]
    place: String,
    instead: Option<String>,
}

pub fn notes(answer: &str) -> String {
    let notes: Vec<Note> = serde_json::from_str(json_in(answer, '[', ']')).unwrap_or_default();
    json!(notes).to_string()
}
