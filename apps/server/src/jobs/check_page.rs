use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::docs::Doc;
use crate::docs::prompts::{CHECK, FILES};
use crate::docs::{self, Sources, json_in};
use crate::error::{ApiError, Result};
use crate::live::{self, Topic};
use crate::{AppState, db};

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
    pub async fn handle(self, state: &AppState) -> Result<()> {
        let pages = db::docs::list(&state.pool, &self.project_id).await?;
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
        let ended = db::docs::end_check(
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
        db::docs::stop_check(&state.pool, &self.doc_id).await.ok();
        live::project(state, &self.project_id, Topic::Docs).await;
    }

    async fn check(&self, state: &AppState, page: &Doc) -> Result<(String, String)> {
        if page.written.as_ref() == Some(&page.markdown) {
            return Ok((page.files.clone(), "[]".to_owned()));
        }
        let writer = docs::writer(state, &self.team_id, &self.user_id).await?;
        let branch = db::docs::check_branch(&state.pool, &self.project_id).await?;
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
        Ok((json!(files).to_string(), docs::notes(&answer)))
    }
}
