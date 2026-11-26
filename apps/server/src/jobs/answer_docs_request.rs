use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::docs::prompts::REQUEST;
use crate::docs::{self, Brief, Sources, json_in};
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db};

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
    pub async fn handle(self, state: &AppState) -> Result<()> {
        let (project_id, request) = (&self.project_id, &self.request);
        let writer = docs::writer(state, &self.team_id, &self.user_id).await?;
        docs::progress(state, project_id, self.run, "Reading the request").await?;
        let sources = Sources::of(state, &self.team_id, project_id, &request.branch).await?;
        let existing = db::docs::list(&state.pool, project_id).await?;
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

        docs::progress(state, project_id, self.run, "Writing the page").await?;
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
        let page = docs::write_page(state, &writer, &sources, brief).await?;
        if current.map(|doc| &doc.markdown) == page.markdown.as_ref() {
            return Err(ApiError::invalid("The writer found nothing to change"));
        }
        let asked = Some(request.text.trim());
        let (commit, pages) = (&sources.commit, [page]);
        db::docs::save_written(
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
        docs::fail_run(state, &self.project_id, self.run, &why).await;
    }
}
