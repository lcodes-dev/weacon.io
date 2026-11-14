//! Removing a chat's worktree: it goes to the Trash, never away, and the journal says where,
//! so it can come back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{ChatId, WorkspaceId, code};

use crate::platform::Env;
use crate::platform::trash::{self, Trashed};

/// A worktree in the Trash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Removal {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    /// Where the worktree was, and comes back to.
    pub(crate) worktree: PathBuf,
    pub(crate) trashed: Trashed,
    /// Unix seconds, for showing when it was removed.
    pub(crate) at: u64,
}

/// One workspace's journal of the worktrees it has in the Trash, a JSON file written whole.
/// Every call blocks, and callers make one call at a time per file.
#[derive(Debug)]
pub(crate) struct Journal {
    workspace: WorkspaceId,
    file: PathBuf,
}

impl Journal {
    pub(crate) const fn new(workspace: WorkspaceId, file: PathBuf) -> Self {
        Self { workspace, file }
    }

    /// Moves `chat`'s worktree to the Trash; a person has asked for it. A chat whose worktree
    /// is already in the Trash is refused, so one never hides another.
    pub(crate) fn remove(
        &self,
        chat: &ChatId,
        worktree: &Path,
        env: Env<'_>,
        now: SystemTime,
    ) -> Result<Removal, Fault> {
        let mut removals = self.load()?;
        if self.find(&removals, chat).is_some() {
            return Err(Fault::new(
                code!("worktree.already_removed"),
                "The chat's worktree is already in the Trash",
            ));
        }
        let trashed = trash::trash(worktree, env, now).map_err(|error| {
            Fault::new(
                code!("worktree.trash_failed"),
                "The worktree couldn't be moved to the Trash",
            )
            .because(error)
        })?;
        let removal = Removal {
            workspace: self.workspace.clone(),
            chat: chat.clone(),
            worktree: worktree.to_owned(),
            trashed,
            at: now
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        };
        removals.push(removal.clone());
        if let Err(error) = self.save(&removals) {
            // Unjournaled, it couldn't be undone here: put it back.
            return Err(match trash::restore(&removal.trashed, worktree) {
                Ok(()) => error,
                Err(_) => Fault::new(
                    code!("worktree.unjournaled"),
                    "The worktree is in the Trash, but Weacon couldn't note it; restore it from there",
                ),
            });
        }
        Ok(removal)
    }

    /// Brings `chat`'s worktree back from the Trash to where it was. The journal forgets it
    /// first, so a worktree that's back is never still listed.
    pub(crate) fn restore(&self, chat: &ChatId) -> Result<Removal, Fault> {
        let mut removals = self.load()?;
        let Some(at) = self.find(&removals, chat) else {
            return Err(Fault::new(
                code!("worktree.not_removed"),
                "The chat's worktree isn't in the Trash",
            ));
        };
        let removal = removals.remove(at);
        let gone = matches!(
            removal.trashed.item.symlink_metadata(),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        );
        self.save(&removals)?;
        if gone {
            return Err(Fault::new(
                code!("worktree.gone_from_trash"),
                "The worktree is no longer in the Trash",
            ));
        }
        if let Err(error) = trash::restore(&removal.trashed, &removal.worktree) {
            removals.push(removal);
            drop(self.save(&removals));
            let fault = if error.kind() == io::ErrorKind::PermissionDenied {
                Fault::new(code!("worktree.restore_refused"), trash::REFUSED)
            } else {
                Fault::new(
                    code!("worktree.restore_failed"),
                    "The worktree couldn't be brought back from the Trash",
                )
            };
            return Err(fault.because(error));
        }
        Ok(removal)
    }

    /// `chat`'s worktree, if it's in the Trash.
    pub(crate) fn removed(&self, chat: &ChatId) -> Result<Option<Removal>, Fault> {
        let removals = self.load()?;
        Ok(self
            .find(&removals, chat)
            .and_then(|at| removals.get(at).cloned()))
    }

    // Where `chat`'s removal is; another workspace's never count, even in this file.
    fn find(&self, removals: &[Removal], chat: &ChatId) -> Option<usize> {
        removals
            .iter()
            .position(|removal| removal.workspace == self.workspace && removal.chat == *chat)
    }

    fn load(&self) -> Result<Vec<Removal>, Fault> {
        match fs::read(&self.file) {
            Ok(json) => {
                serde_json::from_slice(&json).map_err(|error| journal_failed().because(error))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(journal_failed().because(error)),
        }
    }

    fn save(&self, removals: &[Removal]) -> Result<(), Fault> {
        let json = serde_json::to_vec(removals).map_err(|error| journal_failed().because(error))?;
        weacon_appkit::write_whole(&self.file, &json)
            .map_err(|error| journal_failed().because(error))
    }
}

fn journal_failed() -> Fault {
    Fault::new(
        code!("worktree.journal_failed"),
        "The journal of removed worktrees couldn't be read or written",
    )
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::env;

    fn workspace(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    #[test]
    fn a_chat_with_nothing_in_the_trash_has_nothing_to_restore() {
        let dir = TempDir::new().unwrap();
        let journal = Journal::new(workspace("w-1"), dir.join("removed.json"));

        assert_eq!(journal.removed(&chat("c-1")).unwrap(), None);
        let error = journal.restore(&chat("c-1")).unwrap_err();
        assert_eq!(code(&error), "worktree.not_removed");
    }

    #[test]
    fn a_worktree_the_trash_refuses_stays_where_it_is() {
        let dir = TempDir::new().unwrap();
        let journal = Journal::new(workspace("w-1"), dir.join("removed.json"));

        let error = journal
            .remove(&chat("c-1"), Path::new("trees/c-1"), &env(&[]), UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.trash_failed");
        assert!(!dir.join("removed.json").exists());
    }

    #[test]
    fn another_workspaces_removals_never_count() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("removed.json");
        let removal = Removal {
            workspace: workspace("w-2"),
            chat: chat("c-1"),
            worktree: dir.join("trees/c-1"),
            trashed: Trashed {
                item: dir.join("Trash/files/c-1"),
                info: None,
            },
            at: 7,
        };
        fs::write(&file, serde_json::to_vec(&[&removal]).unwrap()).unwrap();

        let ours = Journal::new(workspace("w-1"), file.clone());
        let theirs = Journal::new(workspace("w-2"), file);

        assert_eq!(ours.removed(&chat("c-1")).unwrap(), None);
        assert_eq!(theirs.removed(&chat("c-1")).unwrap(), Some(removal));
    }

    #[test]
    fn a_journal_that_doesnt_parse_is_an_error_not_an_empty_one() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("removed.json");
        fs::write(&file, "[{").unwrap();

        let error = Journal::new(workspace("w-1"), file)
            .removed(&chat("c-1"))
            .unwrap_err();

        assert_eq!(code(&error), "worktree.journal_failed");
    }

    #[test]
    fn a_journal_that_cant_be_read_is_an_error_not_an_empty_one() {
        let dir = TempDir::new().unwrap();
        // A folder where the journal's file should be.
        fs::create_dir_all(dir.join("removed.json")).unwrap();
        let journal = Journal::new(workspace("w-1"), dir.join("removed.json"));

        assert_eq!(
            code(&journal.removed(&chat("c-1")).unwrap_err()),
            "worktree.journal_failed"
        );
        assert_eq!(
            code(&journal.restore(&chat("c-1")).unwrap_err()),
            "worktree.journal_failed"
        );
    }

    #[cfg(target_os = "linux")]
    mod on_linux {
        use super::*;

        fn worktree(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join("trees").join(name);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("work.txt"), name).unwrap();
            path
        }

        fn vars(dir: &Path) -> impl Fn(&str) -> Option<std::ffi::OsString> + use<> {
            env(&[("XDG_DATA_HOME", dir.join("data").to_str().unwrap())])
        }

        fn journal(dir: &Path, id: &str) -> Journal {
            Journal::new(workspace(id), dir.join("removed.json"))
        }

        #[test]
        fn a_removed_worktree_is_in_the_trash_until_it_is_restored() {
            let dir = TempDir::new().unwrap();
            let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
            let path = worktree(&dir, "c-1");
            let at = UNIX_EPOCH + std::time::Duration::from_secs(9);

            let removal = journal.remove(&one, &path, &vars(&dir), at).unwrap();

            assert!(!path.exists());
            let moved = removal.trashed.item.join("work.txt");
            assert_eq!(fs::read_to_string(moved).unwrap(), "c-1");
            assert_eq!((removal.at, &removal.worktree), (9, &path));
            assert_eq!(removal.workspace, workspace("w-1"));
            assert_eq!(journal.removed(&one).unwrap(), Some(removal.clone()));
            let again = journal.remove(&one, &path, &vars(&dir), at).unwrap_err();
            assert_eq!(code(&again), "worktree.already_removed");

            assert_eq!(journal.restore(&one).unwrap(), removal);

            assert_eq!(fs::read_to_string(path.join("work.txt")).unwrap(), "c-1");
            assert_eq!(journal.removed(&one).unwrap(), None);
            journal.remove(&one, &path, &vars(&dir), at).unwrap();
        }

        #[test]
        fn removals_never_mix_workspaces_or_chats() {
            let dir = TempDir::new().unwrap();
            let path = worktree(&dir, "c-1");
            journal(&dir, "w-1")
                .remove(&chat("c-1"), &path, &vars(&dir), UNIX_EPOCH)
                .unwrap();

            for (space, other) in [("w-2", "c-1"), ("w-1", "c-2")] {
                let elsewhere = journal(&dir, space);
                assert_eq!(elsewhere.removed(&chat(other)).unwrap(), None);
                let error = elsewhere.restore(&chat(other)).unwrap_err();
                assert_eq!(code(&error), "worktree.not_removed");
            }
            assert!(!path.exists());
        }

        #[test]
        fn a_worktree_that_cant_be_journaled_is_put_back() {
            let dir = TempDir::new().unwrap();
            // A folder where the journal's file should be.
            fs::create_dir_all(dir.join("removed.json")).unwrap();
            let path = worktree(&dir, "c-1");

            let error = journal(&dir, "w-1")
                .remove(&chat("c-1"), &path, &vars(&dir), UNIX_EPOCH)
                .unwrap_err();

            assert_eq!(code(&error), "worktree.journal_failed");
            assert_eq!(fs::read_to_string(path.join("work.txt")).unwrap(), "c-1");
        }

        #[test]
        fn another_workspaces_removals_stay_in_the_file() {
            let dir = TempDir::new().unwrap();
            let theirs = journal(&dir, "w-2")
                .remove(
                    &chat("c-1"),
                    &worktree(&dir, "w-2"),
                    &vars(&dir),
                    UNIX_EPOCH,
                )
                .unwrap();
            let ours = journal(&dir, "w-1");

            ours.remove(
                &chat("c-1"),
                &worktree(&dir, "c-1"),
                &vars(&dir),
                UNIX_EPOCH,
            )
            .unwrap();
            ours.restore(&chat("c-1")).unwrap();

            assert_eq!(
                journal(&dir, "w-2").removed(&chat("c-1")).unwrap(),
                Some(theirs)
            );
        }

        #[test]
        fn a_worktree_gone_from_the_trash_is_forgotten() {
            let dir = TempDir::new().unwrap();
            let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
            let path = worktree(&dir, "c-1");
            let removal = journal
                .remove(&one, &path, &vars(&dir), UNIX_EPOCH)
                .unwrap();
            fs::remove_dir_all(&removal.trashed.item).unwrap();

            let error = journal.restore(&one).unwrap_err();

            assert_eq!(code(&error), "worktree.gone_from_trash");
            assert_eq!(journal.removed(&one).unwrap(), None);
            assert!(!path.exists());
            journal
                .remove(&one, &worktree(&dir, "c-1"), &vars(&dir), UNIX_EPOCH)
                .unwrap();
        }

        #[test]
        fn a_worktree_whose_place_is_taken_stays_in_the_trash() {
            let dir = TempDir::new().unwrap();
            let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
            let path = worktree(&dir, "c-1");
            let removal = journal
                .remove(&one, &path, &vars(&dir), UNIX_EPOCH)
                .unwrap();
            worktree(&dir, "c-1");

            let error = journal.restore(&one).unwrap_err();

            assert_eq!(code(&error), "worktree.restore_failed");
            assert!(removal.trashed.item.join("work.txt").exists());
            assert_eq!(journal.removed(&one).unwrap(), Some(removal));
        }
    }
}
