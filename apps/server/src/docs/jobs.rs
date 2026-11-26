use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::db;
use super::db::{Doc, Written};
use super::prompts::{CHECK, FILES, OUTLINE, OVERVIEW, REQUEST, UPDATING, WRITING};
use super::{Brief, Sources, json_in};
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::settings::writer::Writer;
use crate::{AppState, live};

const PAGES: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
pub struct WriteDocs {
    pub team_id: String,
    pub project_id: String,
    pub run: i64,
    pub branch: String,
    pub user_id: String,
}

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

impl WriteDocs {
    pub async fn perform(self, state: &AppState) -> Result<()> {
        let writer = super::writer(state, &self.team_id, &self.user_id).await?;
        let sources = Sources::of(state, &self.team_id, &self.project_id, &self.branch).await?;
        let existing = db::list(&state.pool, &self.project_id).await?;

        self.progress(state, "Planning the pages").await?;
        let plan = self.plan(state, &writer, &sources, &existing).await?;
        let planned: Vec<(&str, &Planned)> = [("works", &plan.works), ("built", &plan.built)]
            .into_iter()
            .flat_map(|(section, pages)| pages.iter().take(PAGES).map(move |page| (section, page)))
            .collect();

        let own = |path: &&String| {
            let inside =
                |folder: &String| path.starts_with(&format!("{}/", folder.trim_matches('/')));
            !plan.left_out.iter().any(inside)
        };
        let mut pages = Vec::new();
        for (index, (section, page)) in planned.iter().enumerate() {
            let message = format!("Writing page {} of {}", index + 1, planned.len());
            self.progress(state, &message).await?;
            let files: Vec<String> = page.files.iter().filter(own).cloned().collect();
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
            pages.push(super::write_page(state, &writer, &sources, brief).await?);
        }

        self.progress(state, "Writing the overview").await?;
        let overview = overview(state, &writer, &sources, &existing, &pages).await?;
        pages.insert(0, overview);

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
        let is_planned = |doc: &Doc| {
            let same = |(section, page): &(&str, &Planned)| {
                doc.section == *section && doc.title == page.title
            };
            planned.iter().any(same)
        };
        for doc in existing.iter().filter(|doc| !is_planned(doc)) {
            let files: Vec<String> = serde_json::from_str(&doc.files).unwrap_or_default();
            let gone = files.iter().all(|path| !sources.texts.contains_key(path));
            if !files.is_empty() && gone {
                pages.push(removal(doc));
            }
        }

        let left_out = json!(plan.left_out).to_string();
        let from = Some((self.branch.as_str(), left_out.as_str()));
        let (project_id, commit) = (&self.project_id, &sources.commit);
        db::save_written(
            &state.pool,
            project_id,
            self.run,
            commit,
            from,
            None,
            &pages,
        )
        .await?;
        live::project(state, &self.project_id, Topic::Docs).await;
        Ok(())
    }

    pub async fn failed(self, state: &AppState, why: String) {
        super::fail_run(state, &self.project_id, self.run, &why).await;
    }

    async fn progress(&self, state: &AppState, message: &str) -> Result<()> {
        super::progress(state, &self.project_id, self.run, message).await
    }

    async fn plan(
        &self,
        state: &AppState,
        writer: &Writer,
        sources: &Sources,
        existing: &[Doc],
    ) -> Result<Outline> {
        let mut prompt = format!(
            "README:\n{}\n\nRepository outline:\n{}",
            sources.readme(),
            sources.outline
        );
        if !existing.is_empty() {
            prompt.push_str(
                "\nThe pages that exist now. Plan a page that still covers the same thing under \
                 the same title, in the same set:\n",
            );
            for doc in existing {
                writeln!(prompt, "{}: {}", doc.section, doc.title).ok();
            }
        }
        let answer = writer.ask(state, OUTLINE, &prompt).await?;
        serde_json::from_str(json_in(&answer, '{', '}')).map_err(|error| {
            ApiError::invalid(format!("The writer's plan couldn't be read: {error}"))
        })
    }
}

async fn overview(
    state: &AppState,
    writer: &Writer,
    sources: &Sources,
    existing: &[Doc],
    pages: &[Written],
) -> Result<Written> {
    let written: String = pages
        .iter()
        .map(|page| {
            let markdown = page.markdown.as_deref().unwrap_or_default();
            format!("=== {} ===\n{markdown}\n\n", page.title)
        })
        .collect();
    let mut prompt = format!("README:\n{}\n\nThe pages:\n{written}", sources.readme());
    let mut system = format!("{OVERVIEW}\n\n{WRITING}");
    let current = existing.iter().find(|doc| doc.section == "overview");
    if let Some(current) = current {
        write!(system, "\n\n{UPDATING}").ok();
        write!(prompt, "\nThe page as it is now:\n{}", current.markdown).ok();
    }
    let overview = writer.ask(state, &system, &prompt).await?;
    Ok(Written {
        doc: current.map(|doc| (doc.id.clone(), doc.version.clone())),
        section: "overview".to_owned(),
        title: "Overview".to_owned(),
        markdown: Some(overview),
        files: "[]".into(),
        unread: "[]".into(),
        notes: "[]".into(),
        reason: "Written again from the pages as they would be".into(),
    })
}

fn removal(doc: &Doc) -> Written {
    Written {
        doc: Some((doc.id.clone(), doc.version.clone())),
        section: doc.section.clone(),
        title: doc.title.clone(),
        markdown: None,
        files: "[]".into(),
        unread: "[]".into(),
        notes: "[]".into(),
        reason: "The files it was written from are no longer in the repository".into(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AnswerDocsRequest {
    pub team_id: String,
    pub project_id: String,
    pub run: i64,
    pub user_id: String,
    pub request: Request,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub text: String,
    pub page: Option<String>,
    pub branch: String,
    pub earlier: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    page: Option<String>,
    section: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    files: Option<Vec<String>>,
}

impl AnswerDocsRequest {
    pub async fn perform(self, state: &AppState) -> Result<()> {
        let (project_id, request) = (&self.project_id, &self.request);
        let writer = super::writer(state, &self.team_id, &self.user_id).await?;
        super::progress(state, project_id, self.run, "Reading the request").await?;
        let sources = Sources::of(state, &self.team_id, project_id, &request.branch).await?;
        let existing = db::list(&state.pool, project_id).await?;
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
        let target: Target = serde_json::from_str(json_in(&found, '{', '}')).map_err(|error| {
            ApiError::invalid(format!("The writer's answer couldn't be read: {error}"))
        })?;

        super::progress(state, project_id, self.run, "Writing the page").await?;
        let current = target.page.as_ref().and_then(open);
        let new = match target.section.as_deref() {
            Some("works") => "works",
            _ => "built",
        };
        let title = current.map(|doc| &doc.title).or(target.title.as_ref());
        let untitled = || ApiError::invalid("The writer didn't say which page to write");
        let brief = Brief {
            section: current.map_or(new, |doc| &doc.section),
            title: title.ok_or_else(untitled)?,
            covers: target.summary.as_deref().unwrap_or(&asked),
            files: target.files.as_deref().unwrap_or_default(),
            left_out: &[],
            current,
            request: Some(&asked),
        };
        let page = super::write_page(state, &writer, &sources, brief).await?;
        if current.map(|doc| &doc.markdown) == page.markdown.as_ref() {
            return Err(ApiError::invalid("The writer found nothing to change"));
        }
        let asked = Some(request.text.trim());
        let (commit, pages) = (&sources.commit, [page]);
        db::save_written(
            &state.pool,
            project_id,
            self.run,
            commit,
            None,
            asked,
            &pages,
        )
        .await?;
        live::project(state, project_id, Topic::Docs).await;
        Ok(())
    }

    pub async fn failed(self, state: &AppState, why: String) {
        super::fail_run(state, &self.project_id, self.run, &why).await;
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CheckPage {
    pub team_id: String,
    pub project_id: String,
    pub doc_id: String,
    pub user_id: String,
}

#[derive(Deserialize, Default)]
struct Files {
    #[serde(default)]
    files: Vec<String>,
}

impl CheckPage {
    pub async fn perform(self, state: &AppState) -> Result<()> {
        let pages = db::list(&state.pool, &self.project_id).await?;
        let Some(page) = pages.iter().find(|page| page.id == self.doc_id) else {
            return Ok(());
        };
        let (files, notes) = match self.check(state, page).await {
            Ok((files, notes)) => (Some(files), Some(notes)),
            Err(error) => {
                tracing::warn!(
                    "a page couldn't be checked against the code: {}",
                    error.message
                );
                (None, None)
            }
        };
        let ended = db::end_check(
            &state.pool,
            &self.doc_id,
            &page.version,
            files.as_deref(),
            notes.as_deref(),
        );
        ended.await?;
        live::project(state, &self.project_id, Topic::Docs).await;
        Ok(())
    }

    pub async fn failed(self, state: &AppState) {
        db::stop_check(&state.pool, &self.doc_id).await.ok();
        live::project(state, &self.project_id, Topic::Docs).await;
    }

    async fn check(&self, state: &AppState, page: &Doc) -> Result<(String, String)> {
        if page.written.as_ref() == Some(&page.markdown) {
            return Ok((page.files.clone(), "[]".to_owned()));
        }
        let writer = super::writer(state, &self.team_id, &self.user_id).await?;
        let branch = db::check_branch(&state.pool, &self.project_id).await?;
        let branch = branch.ok_or_else(|| ApiError::invalid("No branch is mapped yet"))?;
        let sources = Sources::of(state, &self.team_id, &self.project_id, &branch).await?;
        let mut files: Vec<String> = serde_json::from_str(&page.files).unwrap_or_default();
        files.retain(|path| sources.texts.contains_key(path));
        let shown = format!("The page, {}:\n{}", page.title, page.markdown);
        if files.is_empty() {
            let prompt = format!("{shown}\n\nRepository outline:\n{}", sources.outline);
            let answer = writer.ask(state, FILES, &prompt).await?;
            let named: Files = serde_json::from_str(json_in(&answer, '{', '}')).unwrap_or_default();
            files = named.files;
            files.retain(|path| sources.texts.contains_key(path));
        }
        let (code, unread) = sources.numbered(&files);
        files.retain(|path| !unread.contains(path));
        let mut prompt = format!("{shown}\n\nThe files it is about:\n{code}");
        if let Some(written) = &page.written {
            write!(prompt, "\nThe page as you last wrote it:\n{written}").ok();
        }
        let answer = writer.ask(state, CHECK, &prompt).await?;
        Ok((json!(files).to_string(), super::notes(&answer)))
    }
}
