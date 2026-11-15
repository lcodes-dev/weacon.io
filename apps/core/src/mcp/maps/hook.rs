//! The code map a run's hooks ask its core for over the core's socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use weacon_observability::Logger;
use weacon_protocol::{MapQuery, MapQueryKind, McpCall};

use super::Maps;
use crate::codemap::{locate, off_runtime};
use crate::mcp::grants::Grants;
use crate::mcp::tools::or_empty;

impl Maps {
    /// The text a hook's `query` gets: the stored map of its folder, in the workspace of the
    /// live run in `grants` whose token it holds; none for any other token or folder, or a map
    /// not read in time.
    pub(crate) async fn for_hook(
        &self,
        grants: &Grants,
        query: MapQuery,
        log: &Logger,
    ) -> Option<String> {
        let MapQueryKind::Map(args) = query.query else {
            return None;
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
        let call = McpCall::Map(args);
        let stored = self.stored(&call, (run.workspace.clone(), folder), permit, &log);
        let text = stored.await.ok().flatten().map(or_empty);
        log.info("hook map answered", &[("stored", text.is_some().into())]);
        text
    }
}
