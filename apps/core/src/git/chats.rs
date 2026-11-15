//! Which repository each chat works in, and where its branch began, kept per workspace.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{ChatId, RepoId, WorkspaceId, code};

use super::{Start, state};

/// A chat's repository and the start its changes count from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatRepo {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    pub(crate) repo: RepoId,
    pub(crate) start: Start,
}

/// One workspace's records of its chats' repositories, a JSON file written whole. Every call
/// blocks, and callers make one call at a time per file.
#[derive(Debug)]
pub(crate) struct ChatRepos {
    workspace: WorkspaceId,
    file: PathBuf,
}

impl ChatRepos {
    pub(crate) const fn new(workspace: WorkspaceId, file: PathBuf) -> Self {
        Self { workspace, file }
    }

    /// `chat`'s record; another workspace's never counts, even in this file.
    pub(crate) fn get(&self, chat: &ChatId) -> Result<Option<ChatRepo>, Fault> {
        Ok(self
            .load()?
            .into_iter()
            .find(|record| self.is(record, chat)))
    }

    /// Records `chat`'s repository and start once: a chat that has a record keeps it.
    pub(crate) fn record(
        &self,
        chat: &ChatId,
        repo: RepoId,
        start: Start,
    ) -> Result<ChatRepo, Fault> {
        let mut records = self.load()?;
        if let Some(kept) = records.iter().find(|record| self.is(record, chat)) {
            return Ok(kept.clone());
        }
        let record = ChatRepo {
            workspace: self.workspace.clone(),
            chat: chat.clone(),
            repo,
            start,
        };
        records.push(record.clone());
        state::save(&self.file, &records).map_err(|error| failed().because(error))?;
        Ok(record)
    }

    fn is(&self, record: &ChatRepo, chat: &ChatId) -> bool {
        record.workspace == self.workspace && record.chat == *chat
    }

    fn load(&self) -> Result<Vec<ChatRepo>, Fault> {
        state::load(&self.file).map_err(|error| failed().because(error))
    }
}

fn failed() -> Fault {
    Fault::new(
        code!("git.chats_failed"),
        "The record of chats' repositories couldn't be read or written",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    fn records(dir: &TempDir, workspace: &str) -> ChatRepos {
        let id = WorkspaceId::try_from(workspace.to_owned()).unwrap();
        ChatRepos::new(id, dir.join("chats.json"))
    }

    fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    fn repo(id: &str) -> RepoId {
        RepoId::try_from(id.to_owned()).unwrap()
    }

    fn start(commit: &str) -> Start {
        Start {
            commit: commit.repeat(40),
            branch: Some("main".to_owned()),
        }
    }

    #[test]
    fn a_chats_record_is_kept_as_first_written() {
        let dir = TempDir::new().unwrap();
        let ours = records(&dir, "w-1");

        assert_eq!(ours.get(&chat("c-1")).unwrap(), None);
        let first = ours.record(&chat("c-1"), repo("site"), start("a")).unwrap();
        let again = ours
            .record(&chat("c-1"), repo("other"), start("b"))
            .unwrap();

        assert_eq!(again, first);
        assert_eq!(ours.get(&chat("c-1")).unwrap(), Some(first.clone()));
        assert_eq!((first.repo, first.start), (repo("site"), start("a")));
        assert_eq!(ours.get(&chat("c-2")).unwrap(), None);
    }

    #[test]
    fn records_never_mix_workspaces() {
        let dir = TempDir::new().unwrap();
        records(&dir, "w-1")
            .record(&chat("c-1"), repo("site"), start("a"))
            .unwrap();
        let theirs = records(&dir, "w-2");

        assert_eq!(theirs.get(&chat("c-1")).unwrap(), None);
        let own = theirs
            .record(&chat("c-1"), repo("other"), start("b"))
            .unwrap();

        assert_eq!(own.repo, repo("other"));
        let first = records(&dir, "w-1").get(&chat("c-1")).unwrap().unwrap();
        assert_eq!(first.repo, repo("site"));
    }

    #[test]
    fn a_record_that_cant_be_read_is_an_error_not_an_empty_one() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.join("chats.json")).unwrap();
        let ours = records(&dir, "w-1");

        assert_eq!(
            ours.get(&chat("c-1")).unwrap_err().code().as_str(),
            "git.chats_failed"
        );
        let error = ours
            .record(&chat("c-1"), repo("site"), start("a"))
            .unwrap_err();
        assert_eq!(error.code().as_str(), "git.chats_failed");
    }
}
