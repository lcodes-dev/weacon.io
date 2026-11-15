//! The code map a run's hooks ask its core for over the core's socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use weacon_codemap::fit;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{ChatTask, MapQuery, MapQueryKind, McpCall, WorkspaceId};

use super::{Maps, Stores};
use crate::codemap::{locate, off_runtime, repo_map};
use crate::db::Looking;
use crate::mcp::grants::Grants;
use crate::mcp::tools::{budget, or_empty};

/// The share of a session start's bytes its task's flows may take, in tenths.
const FLOWS_TENTHS: usize = 3;
/// How many of the docs that best match a task's title give their flows.
const DOCS: usize = 5;
/// The most bytes of a task's title a session start quotes.
const TITLE_MAX: usize = 200;
/// The most words of a task's title its docs are searched by, each of 3 characters or more.
const WORDS: usize = 8;

impl Maps {
    /// The text a hook's `query` gets: the stored map of its folder, in the workspace of the
    /// live run in `grants` whose token it holds, after the flows of the task of the run's chat
    /// for a session start; none for any other token or folder, or a map not read in time.
    pub(crate) async fn for_hook(
        &self,
        grants: &Grants,
        query: MapQuery,
        log: &Logger,
    ) -> Option<String> {
        let (args, starts) = match query.query {
            MapQueryKind::Map(args) => (args, false),
            MapQueryKind::SessionStart(args) => (args, true),
            MapQueryKind::Newer => return None,
        };
        let run = grants.find(query.token.expose())?;
        let permit = Arc::clone(&run.calls).try_acquire_owned().ok()?;
        let log = log.child(&[
            ("workspace_id", run.workspace.as_str().into()),
            ("run_id", run.run.as_str().into()),
        ]);
        let folder = PathBuf::from(query.folder);
        let git = self.stores(&log).await?.git;
        let (own, asked) = (run.folder.clone(), folder.clone());
        let worktree = move |folder: &Path| locate(&git, folder).ok().map(|(_, worktree)| worktree);
        // A token reads only its own run's worktree, as its MCP calls do.
        let same =
            off_runtime(
                move || Ok(worktree(&own).is_some_and(|own| worktree(&asked) == Some(own))),
            );
        if !same.await.ok()? {
            return None;
        }
        let (task, workspace) = (run.task.clone().filter(|_| starts), run.workspace.clone());
        let flows = task.is_some();
        let read = move |stores: Stores| async move {
            match task {
                Some(task) => {
                    let bytes = budget(args.budget);
                    stores.session_start(workspace, folder, &task, bytes).await
                }
                None => stores.answer(&McpCall::Map(args), workspace, folder).await,
            }
        };
        let text = self
            .stored(read, permit, &log)
            .await
            .ok()
            .flatten()
            .map(or_empty);
        let fields = [("stored", text.is_some().into()), ("task", flows.into())];
        log.info("hook map answered", &fields);
        text
    }
}

impl Stores {
    /// The flows pinned by the docs that best match `task`'s title, within a fixed share of
    /// `bytes`, then the map of `folder` in the rest; none when [`Self::index`] finds no map.
    async fn session_start(
        &self,
        workspace: WorkspaceId,
        folder: PathBuf,
        task: &ChatTask,
        bytes: usize,
    ) -> Result<Option<String>, Fault> {
        let Some((watched, index)) = self.index(workspace, folder).await? else {
            return Ok(None);
        };
        let store = self.db.workspace(&watched.workspace);
        let map = repo_map(&store, &self.git, watched.main);
        let words = title_words(&task.title);
        let hits = map.search(Some(&watched.worktree), &words, Looking::DocsWithAnyWord);
        let hits = hits.await?;
        let docs: Vec<&str> = hits
            .iter()
            .take(DOCS)
            .map(|hit| hit.path.as_str())
            .collect();
        let flows = task_flows(
            &task.title,
            index.flows_of(&docs),
            bytes * FLOWS_TENTHS / 10,
        );
        let rest = bytes.saturating_sub(flows.len());
        let start = if index.crates.is_empty() {
            String::new()
        } else {
            index.start_here(rest)
        };
        Ok(Some(flows + &start))
    }
}

/// The words of `title` a doc may match: those of 3 characters or more, once each.
fn title_words(title: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    let long = title
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().nth(2).is_some());
    for word in long {
        if words.len() < WORDS && !words.contains(&word) {
            words.push(word);
        }
    }
    words.join(" ")
}

/// `flows` under a line that quotes `title`, a person's words, within `share` bytes and ending
/// in a blank line; nothing without flows.
fn task_flows(title: &str, flows: Vec<String>, share: usize) -> String {
    if flows.is_empty() {
        return String::new();
    }
    let title = quoted(title);
    let head = [
        format!("This chat is for the task a person titled {title}."),
        format!(
            "Flows pinned in the docs that best match that title: {}.",
            flows.len()
        ),
    ];
    fit(head.into_iter().chain(flows), share.saturating_sub(1)) + "\n"
}

/// `title` as a quoted string on one line, cut to [`TITLE_MAX`] bytes between characters.
fn quoted(title: &str) -> String {
    let mut kept = String::new();
    let flat = title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch });
    for ch in flat {
        if kept.len() + ch.len_utf8() > TITLE_MAX {
            kept.push('…');
            break;
        }
        kept.push(ch);
    }
    serde_json::to_string(&kept).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_titles_longer_words_are_searched_once_each_and_at_most_eight() {
        assert_eq!(
            title_words("Fix the sign-in page: the page"),
            "Fix the sign page"
        );
        assert_eq!(title_words("a to be"), "");
        let many = "one two three four five six seven eight nine";
        assert_eq!(title_words(many), "one two three four five six seven eight");
        assert_eq!(title_words("née déjà vu"), "née déjà");
    }

    #[test]
    fn a_title_is_quoted_on_one_line_and_cut_between_characters() {
        assert_eq!(quoted("Say \"hi\"\nthen go"), r#""Say \"hi\" then go""#);
        let long = "é".repeat(150);
        let cut = quoted(&long);
        assert_eq!(cut, format!("\"{}…\"", "é".repeat(100)));
    }

    #[test]
    fn a_tasks_flows_fit_their_share_whole_lines_first() {
        let flows: Vec<String> = (0..60)
            .map(|n| format!("- docs/a.md:{n}  a::x -> a::y: holds"))
            .collect();

        let text = task_flows("Speed up the build", flows.clone(), 1200);

        assert!(text.len() <= 1200, "{}", text.len());
        assert!(text.len() > 1100, "{}", text.len());
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("This chat is for the task a person titled \"Speed up the build\".")
        );
        assert_eq!(
            lines.next(),
            Some("Flows pinned in the docs that best match that title: 60.")
        );
        assert_eq!(lines.next(), Some("- docs/a.md:0  a::x -> a::y: holds"));
        assert!(text.ends_with(" more lines\n\n"), "{text}");
        let few = task_flows("Go", flows[..1].to_vec(), 1200);
        assert!(few.ends_with(": holds\n\n"), "{few}");
        assert_eq!(task_flows("Go", Vec::new(), 1200), "");
    }
}
