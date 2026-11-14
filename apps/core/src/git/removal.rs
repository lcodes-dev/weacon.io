//! Removing a chat's worktree: it goes to the Trash, never away, and the journal says where,
//! so it can come back.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use weacon_observability::Fault;
use weacon_protocol::{ChatId, WorkspaceId, code};

use super::Git;
use crate::platform::trash::{self, Trashed};

/// A worktree in the Trash, or on its way there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Removal {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    /// Where the worktree was, and comes back to.
    pub(crate) worktree: PathBuf,
    /// Where it went; none while the move is under way, so a removal cut short there left the
    /// worktree in place, or in the Trash with only the Trash's own record of it.
    pub(crate) trashed: Option<Trashed>,
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
    git: Git,
}

impl Journal {
    pub(crate) const fn new(workspace: WorkspaceId, file: PathBuf, git: Git) -> Self {
        Self {
            workspace,
            file,
            git,
        }
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
        if let Some(at) = self.find(&removals, chat)
            && removals.get(at).is_some_and(stale)
        {
            removals.remove(at);
        }
        if self.find(&removals, chat).is_some() {
            return Err(Fault::new(
                code!("worktree.already_removed"),
                "The chat's worktree is already in the Trash",
            ));
        }
        let git_file = worktree_git_file(worktree)?;
        let removal = Removal {
            workspace: self.workspace.clone(),
            chat: chat.clone(),
            worktree: worktree.to_owned(),
            trashed: None,
            git_file,
            at: now
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        };
        self.git.lock(worktree).map_err(|error| {
            Fault::new(
                code!("worktree.lock_failed"),
                "The worktree couldn't be locked, so it wasn't moved",
            )
            .because(error)
        })?;
        let moved = self.move_locked(removals, removal, worktree, to_trash);
        if moved.is_err() {
            // Only one still in place can be unlocked; git can't reach one in the Trash.
            self.git.unlock(worktree);
        }
        moved
    }

    // `remove` once the worktree is locked: on record as pending, moved, then on record where.
    fn move_locked(
        &self,
        mut removals: Vec<Removal>,
        mut removal: Removal,
        worktree: &Path,
        to_trash: impl FnOnce(&Path) -> io::Result<Trashed>,
    ) -> Result<Removal, Fault> {
        removals.push(removal.clone());
        self.save(&removals)?;
        let trashed = match to_trash(worktree) {
            Ok(trashed) => trashed,
            Err(error) => {
                removals.pop();
                drop(self.save(&removals));
                return Err(Fault::new(
                    code!("worktree.trash_failed"),
                    "The worktree couldn't be moved to the Trash",
                )
                .because(error));
            }
        };
        removal.trashed = Some(trashed.clone());
        if let Some(last) = removals.last_mut() {
            *last = removal.clone();
        }
        if let Err(error) = self.save(&removals) {
            // Unjournaled, it couldn't be undone here: put it back.
            if trash::restore(&trashed, worktree).is_err() {
                return Err(unjournaled());
            }
            removals.pop();
            drop(self.save(&removals));
            return Err(error);
        }
        Ok(removal)
    }

    /// Brings `chat`'s worktree back from the Trash to where it was. Its entry stays until it's
    /// back, and one whose worktree left the Trash, or was replaced there, is forgotten.
    pub(crate) fn restore(&self, chat: &ChatId) -> Result<Removal, Fault> {
        let mut removals = self.load()?;
        let Some(at) = self.find(&removals, chat) else {
            return Err(not_removed());
        };
        let removal = removals.remove(at);
        let Some(trashed) = &removal.trashed else {
            // Cut short: the move either never happened or isn't on record.
            self.save(&removals)?;
            return Err(if stale(&removal) {
                self.git.unlock(&removal.worktree);
                not_removed()
            } else {
                unjournaled()
            });
        };
        let gone = match gone(read_git_file(&trashed.item), &removal.git_file) {
            Ok(gone) => gone,
            // Can't tell, so the entry stays for another try.
            Err(error) => return Err(restore_failed().because(error)),
        };
        if gone {
            self.save(&removals)?;
            return Err(Fault::new(
                code!("worktree.gone_from_trash"),
                "The worktree is no longer in the Trash",
            ));
        }
        trash::restore(trashed, &removal.worktree).map_err(|error| {
            let fault = if error.kind() == io::ErrorKind::PermissionDenied {
                Fault::new(code!("worktree.restore_refused"), trash::REFUSED)
            } else {
                restore_failed()
            };
            fault.because(error)
        })?;
        self.git.unlock(&removal.worktree);
        // If this save fails, the next restore finds the worktree gone and drops the entry.
        self.save(&removals)?;
        Ok(removal)
    }

    /// `chat`'s worktree, if it's in the Trash.
    pub(crate) fn removed(&self, chat: &ChatId) -> Result<Option<Removal>, Fault> {
        let removals = self.load()?;
        Ok(self
            .find(&removals, chat)
            .and_then(|at| removals.get(at))
            .filter(|removal| !stale(removal))
            .cloned())
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

// A removal cut short before its worktree moved: the worktree is still where it was.
fn stale(removal: &Removal) -> bool {
    removal.trashed.is_none()
        && read_git_file(&removal.worktree).is_ok_and(|git_file| git_file == removal.git_file)
}

fn worktree_git_file(worktree: &Path) -> Result<String, Fault> {
    read_git_file(worktree).map_err(|error| {
        Fault::new(
            code!("worktree.not_a_worktree"),
            "That folder isn't a git worktree",
        )
        .because(error)
    })
}

// Whether a trashed item, by its `.git` file as read, is no longer the worktree: nothing is
// there, or something else is. A Trash that won't be read, as macOS's may not, is left to the
// move to refuse; any other failure to read can't tell.
fn gone(read: io::Result<String>, git_file: &str) -> io::Result<bool> {
    use io::ErrorKind::{InvalidData, IsADirectory, NotADirectory, NotFound, PermissionDenied};
    match read {
        Ok(read) => Ok(read != git_file),
        Err(error)
            if matches!(
                error.kind(),
                NotFound | InvalidData | NotADirectory | IsADirectory
            ) =>
        {
            Ok(true)
        }
        Err(error) if error.kind() == PermissionDenied => Ok(false),
        Err(error) => Err(error),
    }
}

fn restore_failed() -> Fault {
    Fault::new(
        code!("worktree.restore_failed"),
        "The worktree couldn't be brought back from the Trash",
    )
}

fn not_removed() -> Fault {
    Fault::new(
        code!("worktree.not_removed"),
        "The chat's worktree isn't in the Trash",
    )
}

fn unjournaled() -> Fault {
    Fault::new(
        code!("worktree.unjournaled"),
        "Weacon lost track of the worktree on its way to the Trash; look for it there",
    )
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
    use crate::git::run;
    use crate::git::tests as git_tests;

    fn workspace(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    fn item(removal: &Removal) -> PathBuf {
        removal.trashed.as_ref().unwrap().item.clone()
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    #[test]
    fn a_chat_with_nothing_in_the_trash_has_nothing_to_restore() {
        let dir = TempDir::new().unwrap();
        let journal = Journal::new(
            workspace("w-1"),
            dir.join("removed.json"),
            git_tests::git(&dir),
        );

        assert_eq!(journal.removed(&chat("c-1")).unwrap(), None);
        let error = journal.restore(&chat("c-1")).unwrap_err();
        assert_eq!(code(&error), "worktree.not_removed");
    }

    #[test]
    fn a_folder_that_isnt_a_worktree_is_never_trashed() {
        let dir = TempDir::new().unwrap();
        let journal = Journal::new(
            workspace("w-1"),
            dir.join("removed.json"),
            git_tests::git(&dir),
        );
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
            trashed: Some(Trashed {
                item: dir.join("Trash/files/c-1"),
                info: None,
            }),
            git_file: "gitdir: /repo/.git/worktrees/c-1\n".to_owned(),
            at: 7,
        };
        fs::write(&file, serde_json::to_vec(&[&removal]).unwrap()).unwrap();

        let ours = Journal::new(workspace("w-1"), file.clone(), git_tests::git(&dir));
        let theirs = Journal::new(workspace("w-2"), file, git_tests::git(&dir));

        assert_eq!(ours.removed(&chat("c-1")).unwrap(), None);
        assert_eq!(theirs.removed(&chat("c-1")).unwrap(), Some(removal));
    }

    #[test]
    fn a_journal_that_doesnt_parse_is_an_error_not_an_empty_one() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("removed.json");
        fs::write(&file, "[{").unwrap();

        let error = Journal::new(workspace("w-1"), file, git_tests::git(&dir))
            .removed(&chat("c-1"))
            .unwrap_err();

        assert_eq!(code(&error), "worktree.journal_failed");
    }

    #[test]
    fn a_journal_that_cant_be_read_is_an_error_not_an_empty_one() {
        let dir = TempDir::new().unwrap();
        // A folder where the journal's file should be.
        fs::create_dir_all(dir.join("removed.json")).unwrap();
        let journal = Journal::new(
            workspace("w-1"),
            dir.join("removed.json"),
            git_tests::git(&dir),
        );

        assert_eq!(
            code(&journal.removed(&chat("c-1")).unwrap_err()),
            "worktree.journal_failed"
        );
        assert_eq!(
            code(&journal.restore(&chat("c-1")).unwrap_err()),
            "worktree.journal_failed"
        );
    }

    // A real worktree of the test's repository, made on first use, holding `work.txt`.
    fn worktree(dir: &Path, name: &str) -> PathBuf {
        let repo = dir.join("repo");
        if !repo.exists() {
            git_tests::repo(dir);
        }
        let path = dir.join("trees").join(name);
        git_tests::git(dir)
            .chat_worktree(&repo, &chat(name), &path)
            .unwrap();
        fs::write(path.join("work.txt"), name).unwrap();
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
        Journal::new(workspace(id), dir.join("removed.json"), git_tests::git(dir))
    }

    #[test]
    fn a_removed_worktree_is_in_the_trash_until_it_is_restored() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let at = UNIX_EPOCH + std::time::Duration::from_secs(9);

        let removal = journal.remove(&one, &path, bin(&dir), at).unwrap();

        assert!(!path.exists());
        let moved = item(&removal).join("work.txt");
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
    fn a_journal_that_cant_be_read_moves_nothing() {
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
        fs::write(item(&removal).join(".git"), "gitdir: /other\n").unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert!(!path.exists());
        assert!(item(&removal).exists());
        assert_eq!(journal.removed(&one).unwrap(), None);
    }

    #[test]
    fn a_restore_cut_short_after_the_move_is_tidied_next_time() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        // Moved back, but stopped before the journal was saved.
        trash::restore(removal.trashed.as_ref().unwrap(), &path).unwrap();

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
        assert_eq!(read_git_file(&longest).unwrap().len(), 4096);

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
        fs::remove_file(item(&removal).join(".git")).unwrap();
        fs::create_dir(item(&removal).join(".git")).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert!(!path.exists());
    }

    #[test]
    fn a_removal_is_on_record_before_the_worktree_moves() {
        let dir = TempDir::new().unwrap();
        let path = worktree(&dir, "c-1");
        let file = dir.join("removed.json");
        let to_bin = bin(&dir);
        let watched = |moving: &Path| {
            let pending: Vec<Removal> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].trashed, None);
            to_bin(moving)
        };

        let removal = journal(&dir, "w-1")
            .remove(&chat("c-1"), &path, watched, UNIX_EPOCH)
            .unwrap();

        let saved: Vec<Removal> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(saved, [removal]);
    }

    #[test]
    fn a_removal_cut_short_is_tidied_where_the_worktree_is() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let pending = Removal {
            workspace: workspace("w-1"),
            chat: one.clone(),
            worktree: path.clone(),
            trashed: None,
            git_file: fs::read_to_string(path.join(".git")).unwrap(),
            at: 0,
        };
        fs::write(
            dir.join("removed.json"),
            serde_json::to_vec(&[&pending]).unwrap(),
        )
        .unwrap();

        // Stopped before the move: the worktree is where it was.
        assert_eq!(
            code(&journal.restore(&one).unwrap_err()),
            "worktree.not_removed"
        );
        assert_eq!(journal.removed(&one).unwrap(), None);
        assert!(path.join("work.txt").exists());

        // Stopped after it: the worktree is in the Trash with only the Trash's own record.
        fs::write(
            dir.join("removed.json"),
            serde_json::to_vec(&[&pending]).unwrap(),
        )
        .unwrap();
        bin(&dir)(&path).unwrap();
        assert_eq!(
            code(&journal.restore(&one).unwrap_err()),
            "worktree.unjournaled"
        );
        assert_eq!(journal.removed(&one).unwrap(), None);
    }

    #[test]
    fn a_worktree_moved_but_not_journaled_is_put_back() {
        let dir = TempDir::new().unwrap();
        let path = worktree(&dir, "c-1");
        let file = dir.join("removed.json");
        let to_bin = bin(&dir);
        // The move works, then the journal can't be written.
        let breaks = |moving: &Path| {
            let moved = to_bin(moving);
            fs::remove_file(&file).unwrap();
            fs::create_dir(&file).unwrap();
            moved
        };

        let error = journal(&dir, "w-1")
            .remove(&chat("c-1"), &path, breaks, UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.journal_failed");
        assert_eq!(fs::read_to_string(path.join("work.txt")).unwrap(), "c-1");
    }

    #[test]
    fn a_worktree_that_cant_be_put_back_is_said_to_be_lost() {
        let dir = TempDir::new().unwrap();
        let path = worktree(&dir, "c-1");
        let file = dir.join("removed.json");
        let to_bin = bin(&dir);
        let breaks = |moving: &Path| {
            let moved = to_bin(moving);
            fs::remove_file(&file).unwrap();
            fs::create_dir(&file).unwrap();
            // Something takes the worktree's place, so it can't come back.
            fs::create_dir_all(moving.join("new")).unwrap();
            moved
        };

        let error = journal(&dir, "w-1")
            .remove(&chat("c-1"), &path, breaks, UNIX_EPOCH)
            .unwrap_err();

        assert_eq!(code(&error), "worktree.unjournaled");
        assert!(dir.join("bin/c-1/work.txt").exists());
    }

    #[test]
    fn a_removal_cut_short_before_the_move_never_blocks_the_chat() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let pending = Removal {
            workspace: workspace("w-1"),
            chat: one.clone(),
            worktree: path.clone(),
            trashed: None,
            git_file: fs::read_to_string(path.join(".git")).unwrap(),
            at: 0,
        };
        fs::write(
            dir.join("removed.json"),
            serde_json::to_vec(&[&pending]).unwrap(),
        )
        .unwrap();

        assert_eq!(journal.removed(&one).unwrap(), None);
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();

        assert!(removal.trashed.is_some());
        assert_eq!(journal.removed(&one).unwrap(), Some(removal));
    }

    #[test]
    fn a_worktree_is_locked_while_in_the_trash_and_unlocked_once_back() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let git = git_tests::git(&dir);
        let locked = || {
            let listed =
                run(git
                    .command(&dir.join("repo"))
                    .args(["worktree", "list", "--porcelain"]));
            String::from_utf8(listed.unwrap())
                .unwrap()
                .contains("locked In the Trash")
        };

        journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        assert!(locked());

        journal.restore(&one).unwrap();
        assert!(!locked());
        // A removal that fails leaves no lock behind.
        let refuse = |_: &Path| Err(io::ErrorKind::Unsupported.into());
        journal.remove(&one, &path, refuse, UNIX_EPOCH).unwrap_err();
        assert!(!locked());
    }

    #[test]
    fn only_absence_or_replacement_forgets_a_trashed_worktree() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let removal = journal
            .remove(&one, &worktree(&dir, "c-1"), bin(&dir), UNIX_EPOCH)
            .unwrap();
        // A `.git` this long reads as nothing like the worktree's own.
        fs::write(item(&removal).join(".git"), "g".repeat(5000)).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        let failed = |kind: io::ErrorKind| gone(Err(kind.into()), "gitdir: x\n");
        assert!(gone(Ok("gitdir: x\n".to_owned()), "gitdir: x\n").is_ok_and(|gone| !gone));
        assert!(gone(Ok("gitdir: y\n".to_owned()), "gitdir: x\n").is_ok_and(|gone| gone));
        for absent in [
            io::ErrorKind::NotFound,
            io::ErrorKind::InvalidData,
            io::ErrorKind::IsADirectory,
        ] {
            assert!(failed(absent).is_ok_and(|gone| gone), "{absent:?}");
        }
        assert!(failed(io::ErrorKind::PermissionDenied).is_ok_and(|gone| !gone));
        assert_eq!(
            failed(io::ErrorKind::TimedOut).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
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
        fs::remove_dir_all(item(&removal)).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.gone_from_trash");
        assert_eq!(journal.removed(&one).unwrap(), None);
        assert!(!path.exists());
        journal
            .remove(&one, &worktree(&dir, "c-2"), bin(&dir), UNIX_EPOCH)
            .unwrap();
    }

    #[test]
    fn a_worktree_whose_place_is_taken_stays_in_the_trash() {
        let dir = TempDir::new().unwrap();
        let (journal, one) = (journal(&dir, "w-1"), chat("c-1"));
        let path = worktree(&dir, "c-1");
        let removal = journal.remove(&one, &path, bin(&dir), UNIX_EPOCH).unwrap();
        fs::create_dir_all(path.join("new")).unwrap();

        let error = journal.restore(&one).unwrap_err();

        assert_eq!(code(&error), "worktree.restore_failed");
        assert!(item(&removal).join("work.txt").exists());
        assert_eq!(journal.removed(&one).unwrap(), Some(removal));
    }
}
