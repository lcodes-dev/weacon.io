//! Removing a chat's worktree: it goes to the Trash, never away, and the journal says where,
//! so it can come back.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{ChatId, WorkspaceId, code};

use super::state;
use crate::platform::trash::{self, Trashed};

/// A worktree in the Trash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Removal {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    /// Where the worktree was, and comes back to.
    pub(crate) worktree: PathBuf,
    pub(crate) trashed: Trashed,
    /// The worktree's `.git` file, which names it alone, to know it again in the Trash.
    pub(crate) git_file: String,
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

    /// Moves `chat`'s worktree to the Trash with `to_trash`, as `platform::trash::trash`; a
    /// person has asked for it. A chat whose worktree is already in the Trash is refused, so
    /// one never hides another.
    pub(crate) fn remove(
        &self,
        chat: &ChatId,
        worktree: &Path,
        to_trash: impl FnOnce(&Path) -> io::Result<Trashed>,
        now: SystemTime,
    ) -> Result<Removal, Fault> {
        let mut removals = self.load()?;
        if self.find(&removals, chat).is_some() {
            return Err(Fault::new(
                code!("worktree.already_removed"),
                "The chat's worktree is already in the Trash",
            ));
        }
        let git_file = read_git_file(worktree).map_err(|error| {
            Fault::new(
                code!("worktree.not_a_worktree"),
                "That folder isn't a git worktree",
            )
            .because(error)
        })?;
        let trashed = to_trash(worktree).map_err(|error| {
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
            git_file,
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

    /// Brings `chat`'s worktree back from the Trash to where it was. Its entry stays until it's
    /// back, and one whose worktree left the Trash, or was replaced there, is forgotten.
    pub(crate) fn restore(&self, chat: &ChatId) -> Result<Removal, Fault> {
        let mut removals = self.load()?;
        let Some(at) = self.find(&removals, chat) else {
            return Err(Fault::new(
                code!("worktree.not_removed"),
                "The chat's worktree isn't in the Trash",
            ));
        };
        let removal = removals.remove(at);
        // A Trash that won't be read, as macOS's may not, is left to the move to refuse.
        let gone = match read_git_file(&removal.trashed.item) {
            Ok(git_file) => git_file != removal.git_file,
            Err(error) => error.kind() != io::ErrorKind::PermissionDenied,
        };
        if gone {
            self.save(&removals)?;
            return Err(Fault::new(
                code!("worktree.gone_from_trash"),
                "The worktree is no longer in the Trash",
            ));
        }
        trash::restore(&removal.trashed, &removal.worktree).map_err(|error| {
            let fault = if error.kind() == io::ErrorKind::PermissionDenied {
                Fault::new(code!("worktree.restore_refused"), trash::REFUSED)
            } else {
                Fault::new(
                    code!("worktree.restore_failed"),
                    "The worktree couldn't be brought back from the Trash",
                )
            };
            fault.because(error)
        })?;
        // If this save fails, the next restore finds the worktree gone and drops the entry.
        self.save(&removals)?;
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
        state::load(&self.file).map_err(|error| journal_failed().because(error))
    }

    fn save(&self, removals: &[Removal]) -> Result<(), Fault> {
        state::save(&self.file, removals).map_err(|error| journal_failed().because(error))
    }
}

// The `.git` file naming a worktree's git folder: a plain file, never a link, and short.
fn read_git_file(worktree: &Path) -> io::Result<String> {
    const MAX: u64 = 4096;
    let file = worktree.join(".git");
    let meta = file.symlink_metadata()?;
    if !meta.is_file() || meta.len() > MAX {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut text = String::new();
    fs::File::open(&file)?.take(MAX).read_to_string(&mut text)?;
    Ok(text)
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
    fn a_folder_that_isnt_a_worktree_is_never_trashed() {
        let dir = TempDir::new().unwrap();
        let journal = Journal::new(workspace("w-1"), dir.join("removed.json"));
        fs::create_dir_all(dir.join("plain")).unwrap();

        let error = journal
            .remove(&chat("c-1"), &dir.join("plain"), bin(&dir), UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.not_a_worktree");
        assert!(dir.join("plain").exists());
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
            git_file: "gitdir: /repo/.git/worktrees/c-1\n".to_owned(),
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

    fn worktree(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join("trees").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("work.txt"), name).unwrap();
        let gitdir = format!("gitdir: /repo/.git/worktrees/{name}\n");
        fs::write(path.join(".git"), gitdir).unwrap();
        path
    }

    // A Trash of the test's own, so nothing ever reaches the system's.
    fn bin(dir: &Path) -> impl FnOnce(&Path) -> io::Result<Trashed> + use<> {
        let bin = dir.join("bin");
        move |path: &Path| {
            let item = bin.join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?);
            fs::create_dir_all(&bin)?;
            fs::rename(path, &item)?;
            Ok(Trashed { item, info: None })
        }
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

        let removal = journal.remove(&one, &path, bin(&dir), at).unwrap();

        assert!(!path.exists());
        let moved = removal.trashed.item.join("work.txt");
        assert_eq!(fs::read_to_string(moved).unwrap(), "c-1");
        assert_eq!((removal.at, &removal.worktree), (9, &path));
        assert_eq!(removal.workspace, workspace("w-1"));
        assert_eq!(journal.removed(&one).unwrap(), Some(removal.clone()));
        let again = journal.remove(&one, &path, bin(&dir), at).unwrap_err();
        assert_eq!(code(&again), "worktree.already_removed");

        assert_eq!(journal.restore(&one).unwrap(), removal);

        assert_eq!(fs::read_to_string(path.join("work.txt")).unwrap(), "c-1");
        assert_eq!(journal.removed(&one).unwrap(), None);
        journal.remove(&one, &path, bin(&dir), at).unwrap();
    }

    #[test]
    fn removals_never_mix_workspaces_or_chats() {
        let dir = TempDir::new().unwrap();
        let path = worktree(&dir, "c-1");
        journal(&dir, "w-1")
            .remove(&chat("c-1"), &path, bin(&dir), UNIX_EPOCH)
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
            .remove(&chat("c-1"), &path, bin(&dir), UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.journal_failed");
        assert_eq!(fs::read_to_string(path.join("work.txt")).unwrap(), "c-1");
    }

    #[test]
    fn a_worktree_the_trash_refuses_stays_where_it_is() {
        let dir = TempDir::new().unwrap();
        let path = worktree(&dir, "c-1");
        let refuse = |_: &Path| Err(io::ErrorKind::Unsupported.into());

        let error = journal(&dir, "w-1")
            .remove(&chat("c-1"), &path, refuse, UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.trash_failed");
        assert!(path.join("work.txt").exists());
        assert_eq!(journal(&dir, "w-1").removed(&chat("c-1")).unwrap(), None);
    }

    #[test]
    fn a_worktree_replaced_in_the_trash_is_forgotten_not_restored() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        // The Trash emptied, and another folder trashed under the same name.
        fs::write(removal.trashed.item.join(".git"), "gitdir: /other\n").unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert!(!path.exists());
        assert!(removal.trashed.item.exists());
        assert_eq!(journal.removed(&one).unwrap(), None);
    }

    #[test]
    fn a_restore_cut_short_after_the_move_is_tidied_next_time() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        // Moved back, but stopped before the journal was saved.
        trash::restore(&removal.trashed, &path).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert!(path.join("work.txt").exists());
        assert_eq!(journal.removed(&one).unwrap(), None);
    }

    #[test]
    fn a_git_file_that_isnt_a_short_plain_file_is_no_worktree() {
        let dir = TempDir::new().unwrap();
        let journal = journal(&dir, "w-1");
        // A repository's own folder, whose `.git` is a folder.
        let clone = worktree(&dir, "c-1");
        fs::remove_file(clone.join(".git")).unwrap();
        fs::create_dir(clone.join(".git")).unwrap();
        let long = worktree(&dir, "c-2");
        fs::write(long.join(".git"), "g".repeat(4097)).unwrap();
        let longest = worktree(&dir, "c-3");
        fs::write(longest.join(".git"), "g".repeat(4096)).unwrap();
        journal
            .remove(&chat("c-3"), &longest, bin(&dir), UNIX_EPOCH)
            .unwrap();

        for (chat_id, path) in [("c-1", &clone), ("c-2", &long)] {
            let error = journal
                .remove(&chat(chat_id), path, bin(&dir), UNIX_EPOCH)
                .unwrap_err();
            assert_eq!(code(&error), "worktree.not_a_worktree");
            assert!(path.exists());
        }
    }

    #[test]
    fn another_repository_in_the_trash_is_never_restored() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        // The Trash emptied, and a clone, whose `.git` is a folder, trashed under the name.
        fs::remove_file(removal.trashed.item.join(".git")).unwrap();
        fs::create_dir(removal.trashed.item.join(".git")).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert!(!path.exists());
    }

    #[test]
    fn another_workspaces_removals_stay_in_the_file() {
        let dir = TempDir::new().unwrap();
        let theirs = journal(&dir, "w-2")
            .remove(&chat("c-1"), &worktree(&dir, "w-2"), bin(&dir), UNIX_EPOCH)
            .unwrap();
        let ours = journal(&dir, "w-1");

        ours.remove(&chat("c-1"), &worktree(&dir, "c-1"), bin(&dir), UNIX_EPOCH)
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
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        fs::remove_dir_all(&removal.trashed.item).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert_eq!(journal.removed(&one).unwrap(), None);
        assert!(!path.exists());
        journal
            .remove(&one, &worktree(&dir, "c-1"), bin(&dir), UNIX_EPOCH)
            .unwrap();
    }

    #[test]
    fn a_worktree_whose_place_is_taken_stays_in_the_trash() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        worktree(&dir, "c-1");

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.restore_failed");
        assert!(removal.trashed.item.join("work.txt").exists());
        assert_eq!(journal.removed(&one).unwrap(), Some(removal));
    }
}
