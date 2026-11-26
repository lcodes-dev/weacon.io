use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::docs::{Doc, Written};
use crate::docs::prompts::{OUTLINE, OVERVIEW, UPDATING, WRITING};
use crate::docs::{self, Brief, Sources, json_in};
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::writer::Writer;
use crate::{AppState, db};

/// At most this many pages in each of "How it works" and "How it's built".
const PAGES: usize = 8;

/// Writes the docs of a branch: as pages when the project has none, else as proposals to
/// change them. `run` is the run that holds the project while it writes.
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
    pub async fn handle(self, state: &AppState) -> Result<()> {
        let writer = docs::writer(state, &self.team_id, &self.user_id).await?;
        let sources = Sources::of(state, &self.team_id, &self.project_id, &self.branch).await?;
        let existing = db::docs::list(&state.pool, &self.project_id).await?;

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
            pages.push(docs::write_page(state, &writer, &sources, brief).await?);
        }

        self.progress(state, "Writing the overview").await?;
        let overview = overview(state, &writer, &sources, &existing, &pages).await?;
        pages.insert(0, overview);

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
        let is_planned = |doc: &Doc| {
            let same = |(section, page): &(&str, &Planned)| {
                doc.section == *section && doc.title == page.title
            };
            planned.iter().any(same)
        };
        for doc in existing.iter().filter(|doc| !is_planned(doc)) {
            let files: Vec<String> = serde_json::from_str(&doc.files).unwrap_or_default();
            let gone = files.iter().all(|path| !sources.texts.contains_key(path));
            // A page a person made has no files, and is never proposed for removal.
            if !files.is_empty() && gone {
                pages.push(removal(doc));
            }
        }

        let left_out = json!(plan.left_out).to_string();
        let from = Some((self.branch.as_str(), left_out.as_str()));
        let (project_id, commit) = (&self.project_id, &sources.commit);
        db::docs::save_written(
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
        docs::fail_run(state, &self.project_id, self.run, &why).await;
    }

    async fn progress(&self, state: &AppState, message: &str) -> Result<()> {
        docs::progress(state, &self.project_id, self.run, message).await
    }

    /// Asks the writer which pages to write, from which files, and which folders aren't the
    /// project's own code.
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

/// The overview, written again from the pages as they would be.
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
