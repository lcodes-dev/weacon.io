//! The code map a run's hooks ask its core for over the core's socket.

use std::path::{Path, PathBuf};

use weacon_codemap::fit;
use weacon_model::Title;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{ChatTask, MapQuery, MapQueryKind, McpCall, WorkspaceId};

use super::{Maps, Stores};
use crate::codemap::{locate, off_runtime};
use crate::mcp::grants::Grants;
use crate::mcp::tools::{budget, or_empty};

/// The share of a session start's bytes its task's flows may take, in tenths.
const FLOWS_TENTHS: usize = 3;
/// How many of the docs that best match a task's title give their flows.
const DOCS: usize = 5;
/// The most words of a task's title its docs are searched by, each of 4 characters or more,
/// so that short words such as "the" don't match every doc.
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
        let run = grants.find(query.token.expose())?;
        let (args, task) = match query.query {
            MapQueryKind::Map(args) => (args, None),
            MapQueryKind::SessionStart(args) => (args, run.task.clone()),
            MapQueryKind::Newer => return None,
        };
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
        let (for_task, workspace) = (task.is_some(), run.workspace.clone());
        let read = move |stores: Stores| async move {
            match task {
                Some(task) => {
                    let bytes = budget(args.budget);
                    stores.session_start(workspace, folder, &task, bytes).await
                }
                None => stores.answer(&McpCall::Map(args), workspace, folder).await,
            }
        };
        let text = self.stored(read, &log).await.ok().flatten().map(or_empty);
        let fields = [("stored", text.is_some().into()), ("task", for_task.into())];
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
        let Some((_, index)) = self.index(workspace, folder).await? else {
            return Ok(None);
        };
        // A title that isn't one a task may have gives no flows.
        let flows = Title::try_from(task.title.clone()).map_or_else(
            |_| String::new(),
            |title| {
                let found = index.flows_matching(&title_words(title.as_str()), DOCS);
                task_flows(&title, found, bytes * FLOWS_TENTHS / 10)
            },
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

/// The words of `title` a doc may match, in lower case: those of 4 characters or more, once each.
fn title_words(title: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    title
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().nth(3).is_some() && seen.insert(word.to_owned()))
        .take(WORDS)
        .map(str::to_owned)
        .collect()
}

/// `flows` under a line that quotes `title`, a person's words, within `share` bytes and ending
/// in a blank line; nothing without flows.
fn task_flows(title: &Title, flows: Vec<String>, share: usize) -> String {
    if flows.is_empty() {
        return String::new();
    }
    // A title is one line; quoted, it reads as the person's words.
    let title = serde_json::to_string(title.as_str()).unwrap_or_default();
    let head = [
        format!("This chat is for the task a person titled {title}."),
        format!(
            "Flows pinned in the docs that best match that title: {}.",
            flows.len()
        ),
    ];
    fit(head.into_iter().chain(flows), share.saturating_sub(1)) + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_titles_longer_words_are_searched_once_each_and_at_most_eight() {
        let words = |title| title_words(title).join(" ");
        assert_eq!(words("Fix the sign-in page: the page"), "sign page");
        assert_eq!(words("a to be and the"), "");
        let many = "alpha bravo charlie delta echoes foxtrot golfer hotel india";
        assert_eq!(
            words(many),
            "alpha bravo charlie delta echoes foxtrot golfer hotel"
        );
        assert_eq!(words("née déjà vu"), "déjà");
        assert_eq!(words("Build: speed up the BUILD"), "build speed");
    }

    fn title(text: &str) -> Title {
        Title::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn a_tasks_flows_fit_their_share_whole_lines_first() {
        let flows: Vec<String> = (0..60)
            .map(|n| format!("- docs/a.md:{n}  a::x -> a::y: holds"))
            .collect();

        let text = task_flows(&title("Speed up the build"), flows.clone(), 1200);

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
        let few = task_flows(&title("Say \"go\""), flows[..1].to_vec(), 1200);
        assert!(few.ends_with(": holds\n\n"), "{few}");
        assert!(few.starts_with("This chat is for the task a person titled \"Say \\\"go\\\"\"."));
        assert_eq!(task_flows(&title("Go"), Vec::new(), 1200), "");
    }
}
