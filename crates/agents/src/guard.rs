//! The guardrail: the operations a person is always asked about, whatever the access level.
//!
//! A command runs without asking only when the reader reads it exactly and every part of it is
//! on the table of what is known safe (`safe`), working inside the chat's worktree; anything
//! else asks. The table is an allow-list at each level: only listed programs run, each with the
//! option forms it may take, the variable names that may precede it, and paths inside the
//! worktree; an unlisted program, option, variable or path asks. Git writes only in the chat's
//! own checkout.
//!
//! It is not a sandbox: what a listed build or test tool runs, or a script written earlier,
//! isn't inspected. Git itself runs programs named in a repository's config and hooks, so a
//! write to any `.git` path asks (`check_word`); but a config already placed by other means is
//! outside what this reader can see.

mod git;
mod rules;
mod safe;
mod shell;
mod words;

use std::path::{Path, PathBuf};

pub use crate::claude::hook::{claude_ask, claude_hook};

use crate::access::resolve;
use crate::{claude, codex};
use shell::Word;

/// Where an agent works and what it must never touch without asking.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// The folder the command runs in.
    pub cwd: PathBuf,
    /// The chat's worktree, the only folder the agent changes freely.
    pub worktree: PathBuf,
    /// The repository's main checkout, when the worktree belongs to one.
    pub main_checkout: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// Weacon's own folders, which no agent writes to.
    pub weacon: Vec<PathBuf>,
    /// The vendors' credential folders and files, which no agent reads or writes.
    pub credentials: Vec<PathBuf>,
    /// Whether `CDPATH` is set, so `cd` may search other folders.
    pub cdpath: bool,
    /// The workspace's additions: each the first words of a command to ask about.
    pub additions: Vec<Vec<String>>,
}

/// Why a person is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Weacon,
    WeaconFolder,
    DeleteOutside,
    WriteOutside,
    ForcePush,
    DeleteBranch,
    ResetMainCheckout,
    CleanMainCheckout,
    Credentials,
    Addition,
    NotListed,
    Unsure,
}

impl Reason {
    /// A short sentence for the person asked.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Weacon => "It runs Weacon itself.",
            Self::WeaconFolder => "It changes Weacon's own files.",
            Self::DeleteOutside => "It deletes files outside the chat's worktree.",
            Self::WriteOutside => "It writes outside the chat's worktree.",
            Self::ForcePush => "It force-pushes.",
            Self::DeleteBranch => "It deletes or overwrites a branch.",
            Self::ResetMainCheckout => "It hard-resets the main checkout.",
            Self::CleanMainCheckout => "It cleans the main checkout.",
            Self::Credentials => "It reaches an agent's credentials.",
            Self::Addition => "This workspace asks about it.",
            Self::NotListed => "It isn't on the list of commands that run without asking.",
            Self::Unsure => "Weacon can't tell what it does.",
        }
    }
}

// Far longer than any command an agent runs; past it, the guard asks rather than read on.
const MAX_LINE: usize = 64 << 10;

/// Whether `line`, a shell command run in `context.cwd`, needs a person first.
#[must_use]
pub fn check_command(line: &str, context: &Context) -> Option<Reason> {
    let Some(commands) = (line.len() <= MAX_LINE)
        .then(|| shell::parse(line))
        .flatten()
    else {
        return Some(Reason::Unsure);
    };
    let context = &Context {
        cdpath: context.cdpath || line.contains("CDPATH"),
        ..context.clone()
    };
    let mut cwd = context.cwd.clone();
    commands
        .iter()
        .find_map(|simple| rules::judge(simple, context, &mut cwd))
}

/// The vendors' folders and files under `home` that hold a sign-in.
#[must_use]
pub fn credential_folders(home: &Path) -> Vec<PathBuf> {
    claude::CREDENTIALS
        .iter()
        .chain(&codex::CREDENTIALS)
        .chain(&GH_CREDENTIALS)
        .map(|name| home.join(name))
        .collect()
}

// The GitHub CLI's sign-in, which agents use too.
const GH_CREDENTIALS: [&str; 1] = [".config/gh"];

// Asks unless `file` is inside the worktree and outside Weacon's and the vendors' folders.
pub(crate) fn check_path(file: &Path, context: &Context, outside: Reason) -> Option<Reason> {
    let Some(file) = resolve(file) else {
        return Some(Reason::Unsure);
    };
    if under_any(&file, &context.weacon) {
        return Some(Reason::WeaconFolder);
    }
    if under_any(&file, &context.credentials) {
        return Some(Reason::Credentials);
    }
    // Inside the worktree only by its exact name: a folder that differs in case may be another.
    let inside = resolve(&context.worktree).is_some_and(|worktree| file.starts_with(worktree));
    if !inside {
        return Some(outside);
    }
    // Writing a file with another name writes it wherever that name is.
    let aliased = outside == Reason::WriteOutside && has_other_name(&file, true);
    aliased.then_some(Reason::Unsure)
}

// Whether `path` is a file reachable under more than one name, so touching it touches wherever
// its other names are; `follow` resolves a final symlink first, as a write would.
fn has_other_name(path: &Path, follow: bool) -> bool {
    let meta = if follow {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    };
    meta.is_ok_and(|meta| meta.is_file())
        && crate::platform::links(path).is_ok_and(|links| links > 1)
}

// Asks about a path inside a credential folder, or, for a `deep` read that goes through
// folders, above one.
pub(crate) fn reading(file: &Path, context: &Context, deep: bool) -> Option<Reason> {
    let Some(file) = resolve(file) else {
        return Some(Reason::Unsure);
    };
    let near = context.credentials.iter().any(|folder| {
        under(&file, folder)
            || (deep && resolve(folder).is_some_and(|folder| starts_with(&folder, &file)))
    });
    near.then_some(Reason::Credentials)
}

fn under_any(file: &Path, folders: &[PathBuf]) -> bool {
    folders.iter().any(|folder| under(file, folder))
}

fn under(file: &Path, folder: &Path) -> bool {
    resolve(folder).is_some_and(|folder| starts_with(file, &folder))
}

// Some file systems ignore case, so `.Claude` may be `.claude`.
fn starts_with(path: &Path, prefix: &Path) -> bool {
    let mut parts = path.components();
    prefix.components().all(|part| {
        parts
            .next()
            .is_some_and(|other| other.as_os_str().eq_ignore_ascii_case(part.as_os_str()))
    })
}

// What a person must be asked before `word`, a file the command changes, is changed.
fn check_word(word: &Word, context: &Context, cwd: &Path, outside: Reason) -> Option<Reason> {
    // Below a wildcard, a file it writes may have a second name the guard doesn't list.
    let deep_glob = word.glob
        && word
            .text
            .split('/')
            .skip_while(|part| !part.contains(['*', '?', '[']))
            .nth(1)
            .is_some();
    // A repository's settings and hooks name programs that git then runs.
    let in_git = word.text.split('/').any(|part| part == ".git");
    if (outside == Reason::WriteOutside && deep_glob) || in_git {
        return Some(Reason::Unsure);
    }
    path_of(word, context, cwd).map_or(Some(Reason::Unsure), |path| {
        check_path(&path, context, outside)
    })
}

// The path `word` names from `cwd`, `..` left for the file system to follow after any
// symlink, or `None` when the command line doesn't say.
fn path_of(word: &Word, context: &Context, cwd: &Path) -> Option<PathBuf> {
    if word.dynamic {
        return None;
    }
    let text = if word.glob {
        before_glob(&word.text)?
    } else {
        word.text.clone()
    };
    let path = if word.tilde {
        let rest = text.strip_prefix('~')?;
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        context.home.as_ref()?.join(rest.trim_start_matches('/'))
    } else {
        cwd.join(text)
    };
    // A wildcard may match a symlink that leads out, and only the file system knows which.
    (!(word.glob && leads_out(&path, context))).then_some(path)
}

// Whether `folder`, inside the worktree, holds a symlink to somewhere outside it or a file with a
// second name; one it can't list may. A folder outside is judged as outside already.
fn leads_out(folder: &Path, context: &Context) -> bool {
    let worktree = resolve(&context.worktree);
    let inside = |path: &Path| {
        resolve(path)
            .zip(worktree.as_deref())
            .is_some_and(|(path, worktree)| path.starts_with(worktree))
    };
    if !inside(folder) {
        return false;
    }
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    entries.into_iter().any(|entry| {
        entry.map_or(true, |entry| {
            let path = entry.path();
            let link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
            has_other_name(&path, false) || (link && !inside(&path))
        })
    })
}

// The folders before the first part with a wildcard; `None` if a later part climbs with `..`.
fn before_glob(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split('/').collect();
    let wild = |part: &&str| part.contains(['*', '?', '[']);
    let at = parts.iter().position(&wild)?;
    // Past one wildcard part, the matches lie in folders it can't list ahead.
    let later = parts.get(at + 1..)?;
    if later.contains(&"..") || later.iter().any(wild) {
        return None;
    }
    let prefix = parts.get(..at)?.join("/");
    Some(if prefix.is_empty() && !text.starts_with('/') {
        ".".to_owned()
    } else if prefix.is_empty() {
        "/".to_owned()
    } else {
        prefix
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;

    use crate::test_support::guard_context as context;

    #[test]
    fn each_decision_in_a_rule_changes_an_answer() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let main = Context {
            cwd: dir.join("main"),
            ..context.clone()
        };
        let check = |line: &str| check_command(line, &context);
        let in_main = |line: &str| check_command(line, &main);

        // git.rs: the `switch --create` arm, by reason in the main checkout.
        assert_eq!(in_main("git switch -c feature"), Some(Reason::WriteOutside));
        assert_eq!(
            in_main("git switch -c -f feature"),
            Some(Reason::ResetMainCheckout)
        );
        assert_eq!(in_main("git switch other"), Some(Reason::ResetMainCheckout));

        // rules.rs goes_deep: find is never deep; a recursive read above credentials asks.
        assert_eq!(check("grep token ../home"), None);
        assert_eq!(
            check("grep --recursive token ../home"),
            Some(Reason::Credentials)
        );
        // rules.rs read: an option's non-empty value is read where it points.
        assert_eq!(
            check("cat @../home/.codex/auth.json"),
            Some(Reason::Credentials)
        );
        // rules.rs landing: a copy into a folder keeps the source's unknown name.
        assert_eq!(check("cp $X src"), Some(Reason::Unsure));
        // rules.rs changes_program: a non-display variable before a reader asks.
        assert_eq!(check("LC_ALL=C grep x notes.md"), None);
        assert_eq!(check("GScript=x grep x notes.md"), Some(Reason::NotListed));

        // safe.rs reader: uniq's one operand runs, a second is a file it writes.
        assert_eq!(check("uniq notes.md"), None);
        assert_eq!(check("uniq notes.md ../main/out"), Some(Reason::NotListed));
        // safe.rs builds: an option's attached path is checked.
        assert_eq!(
            check("cargo build --target-dir=/etc"),
            Some(Reason::WriteOutside)
        );
    }

    #[test]
    fn each_decision_in_the_parser_changes_an_answer() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        // words.rs: an unknown wrapper option asks.
        assert_eq!(check("env --foo rm -rf /"), Some(Reason::Unsure));
        // words.rs skip_options: a wrapper's valued options are stepped over to reach the command.
        assert_eq!(
            check("timeout -k 1 -s 9 60 rm -rf /"),
            Some(Reason::DeleteOutside)
        );

        // shell.rs: a `(` after a word is a definition, `((` is arithmetic — both can't be read.
        assert_eq!(check("f() { rm -rf /; }"), Some(Reason::Unsure));
        assert_eq!(check("((rm -rf /))"), Some(Reason::Unsure));

        // quoting.rs: $HOME at a word's start is the home folder.
        assert_eq!(
            check("cat $HOME/.claude/auth.json"),
            Some(Reason::Credentials)
        );
        assert_eq!(
            check("cat @$HOME/.codex/auth.json"),
            Some(Reason::Credentials)
        );
    }

    #[test]
    fn work_inside_the_worktree_needs_no_one() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);

        for line in [
            "cargo test --locked",
            "rm -rf target src/old.rs",
            "rm -rf target/* ./build/*.o",
            "echo hi > out.txt && cat out.txt | grep hi >> log.txt",
            "make 2>&1 | tee build.log > /dev/null",
            "mkdir -p a/b && touch a/b/c && cp a/b/c d && mv d e",
            "find . -name '*.o' -delete",
            "git status && git push origin feature && git push -u origin HEAD",
            "git reset --hard HEAD~1 && git clean -fdx",
            "git branch new-feature && git commit -m 'drop table users'",
            "ls ~ && cat README.md",
            "cat <<EOF > notes.md\nrm -rf /\nEOF",
            "cd src && rm -rf generated",
        ] {
            assert_eq!(check_command(line, &context), None, "{line}");
        }
    }

    #[test]
    fn each_operation_on_the_list_asks_whatever_the_access_level() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let state = dir.join("state/settings/w-w1.json");
        let in_state = format!("echo '{{}}' > '{}'", state.display());

        for (line, reason) in [
            ("rm -rf /", Reason::DeleteOutside),
            ("rm -rf ../main", Reason::DeleteOutside),
            ("cd .. && rm -rf main", Reason::DeleteOutside),
            ("sudo -u root rm -rf /var/lib", Reason::NotListed),
            ("find / -name x -delete", Reason::DeleteOutside),
            ("mv notes.md ../notes.md", Reason::DeleteOutside),
            ("echo x > /etc/hosts", Reason::WriteOutside),
            ("echo x >> ~/.bashrc", Reason::WriteOutside),
            ("cp notes.md /tmp/", Reason::WriteOutside),
            ("dd if=/dev/zero of=/dev/sda", Reason::NotListed),
            ("git config core.hooksPath /tmp/hooks", Reason::NotListed),
            ("git push --force", Reason::ForcePush),
            ("git push -fu origin main", Reason::ForcePush),
            ("git push --force-with-lease=main origin", Reason::ForcePush),
            ("git push origin +main", Reason::ForcePush),
            ("git push origin :old", Reason::DeleteBranch),
            ("git push --delete origin old", Reason::DeleteBranch),
            ("git branch -D old", Reason::DeleteBranch),
            ("git -C ../main reset --hard", Reason::ResetMainCheckout),
            ("cd ../main && git clean -fd", Reason::CleanMainCheckout),
            ("dropdb app", Reason::NotListed),
            ("psql -c 'DROP TABLE users'", Reason::NotListed),
            ("redis-cli FLUSHALL", Reason::NotListed),
            ("cat ~/.claude/.credentials.json", Reason::Credentials),
            ("grep token \"$HOME/.codex/auth.json\"", Reason::Credentials),
            ("weacon access w1 full", Reason::NotListed),
            (
                "env X=1 /usr/local/bin/weacon guard remove w1 x",
                Reason::NotListed,
            ),
            (in_state.as_str(), Reason::WeaconFolder),
            ("terraform destroy -auto-approve", Reason::Addition),
            ("bash -c 'rm -rf /'", Reason::NotListed),
            ("echo $(rm -rf /)", Reason::DeleteOutside),
        ] {
            assert_eq!(check_command(line, &context), Some(reason), "{line}");
        }
    }

    #[test]
    fn what_the_command_line_doesnt_say_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);

        for line in [
            "$EDITOR notes.md",
            "rm -rf \"$DIR\"",
            "echo 'open",
            "git --git-dir=/elsewhere/.git reset --hard",
            "git push origin \"$BRANCH\"",
        ] {
            assert_eq!(
                check_command(line, &context),
                Some(Reason::Unsure),
                "{line}"
            );
        }
        let unknown_main = Context {
            main_checkout: None,
            ..context
        };
        assert_eq!(
            check_command("git reset --hard", &unknown_main),
            Some(Reason::ResetMainCheckout)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_worktree_is_followed() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        std::os::unix::fs::symlink(dir.join("main"), dir.join("wt/link")).unwrap();

        assert_eq!(
            check_command("rm -rf link/src", &context),
            Some(Reason::DeleteOutside)
        );
    }

    #[test]
    fn writes_copies_links_and_deletes_follow_each_programs_own_arguments() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(check("touch /tmp/x"), Some(Reason::WriteOutside));
        assert_eq!(check("chmod 644 /etc/hosts"), Some(Reason::WriteOutside));
        assert_eq!(check("sed -i s/a/b/ /etc/hosts"), Some(Reason::NotListed));
        assert_eq!(
            check("sed --in-place=.bak s/a/b/ /etc/hosts"),
            Some(Reason::NotListed)
        );
        assert_eq!(check("sed -n p /etc/hosts"), Some(Reason::NotListed));
        assert_eq!(check("ln --symbolic / out"), Some(Reason::NotListed));
        assert_eq!(check("ln notes.md copy.md"), None);
        assert_eq!(check("cp -s notes.md copy.md"), Some(Reason::NotListed));
        assert_eq!(check("find / -name core"), None);
        assert_eq!(check("rm /*"), Some(Reason::DeleteOutside));
        assert_eq!(check("rm -rf ../*"), Some(Reason::DeleteOutside));
        assert_eq!(check("rm -rf /tmp/*"), Some(Reason::DeleteOutside));
        assert_eq!(check("rm -rf src/*/../../.."), Some(Reason::Unsure));
        assert_eq!(check("rm *.o"), None);
        assert_eq!(check("mysqladmin status"), Some(Reason::NotListed));
        assert_eq!(check("mysqladmin drop app"), Some(Reason::NotListed));
    }

    #[test]
    fn assignments_are_skipped_only_when_they_are_assignments() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(check("_X=1 make -C /"), Some(Reason::WriteOutside));
        assert_eq!(check("X_Y=1 make -C /"), Some(Reason::WriteOutside));
        assert_eq!(check("1X=1 rm /tmp/x"), Some(Reason::NotListed));
        let home = Context {
            cwd: dir.join("home"),
            ..context.clone()
        };
        assert_eq!(
            check_command("ls .claude", &home),
            Some(Reason::Credentials)
        );
    }

    #[test]
    fn git_subcommands_ask_only_for_what_they_change() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);
        let main = Context {
            cwd: dir.join("main"),
            ..context.clone()
        };
        let in_main = |line: &str| check_command(line, &main);

        assert_eq!(
            check("git worktree remove ../main"),
            Some(Reason::NotListed)
        );
        assert_eq!(
            check("git worktree remove --force ../main"),
            Some(Reason::NotListed)
        );
        // Weacon makes and removes the chats' worktrees itself.
        assert_eq!(check("git worktree remove src"), Some(Reason::NotListed));
        assert_eq!(check("git worktree add ../other"), Some(Reason::NotListed));
        assert_eq!(
            check("git update-ref -d refs/heads/old"),
            Some(Reason::NotListed)
        );
        assert_eq!(
            check("git update-ref refs/heads/main HEAD"),
            Some(Reason::NotListed)
        );
        assert_eq!(in_main("git reset HEAD~1"), Some(Reason::WriteOutside));
        assert_eq!(in_main("git clean -n"), None);
        assert_eq!(in_main("git clean --dry-run"), None);
        assert_eq!(check("git config --get user.name"), Some(Reason::NotListed));
        assert_eq!(check("git config user.name"), Some(Reason::NotListed));
        assert_eq!(check("git config --list"), Some(Reason::NotListed));
        assert_eq!(check("git config user.name Ada"), Some(Reason::NotListed));
        assert_eq!(
            check("git config --unset core.pager"),
            Some(Reason::NotListed)
        );
        assert_eq!(check("git config --add a.b c"), Some(Reason::NotListed));
        assert_eq!(check("git config set user.name"), Some(Reason::NotListed));
    }

    #[test]
    fn git_outside_the_worktree_only_looks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let main = Context {
            cwd: dir.join("main"),
            ..context.clone()
        };
        let check = |line: &str| check_command(line, &context);
        let in_main = |line: &str| check_command(line, &main);

        assert_eq!(in_main("git commit -am x"), Some(Reason::WriteOutside));
        assert_eq!(in_main("git push origin main"), Some(Reason::WriteOutside));
        assert_eq!(in_main("git checkout -b x"), Some(Reason::WriteOutside));
        assert_eq!(in_main("git stash"), Some(Reason::WriteOutside));
        assert_eq!(in_main("git log --oneline"), None);
        assert_eq!(in_main("git stash show"), None);
        assert_eq!(check("git push origin main"), None);
        assert_eq!(check("git stash pop"), Some(Reason::NotListed));
        assert_eq!(check("git branch -m main old"), Some(Reason::DeleteBranch));
        assert_eq!(check("git tag -d v1"), Some(Reason::DeleteBranch));
        assert_eq!(check("git tag v1"), None);
        assert_eq!(
            check("git remote set-url origin /x"),
            Some(Reason::NotListed)
        );
        assert_eq!(check("git remote -v"), None);
        assert_eq!(check("git grep -n fn"), None);
        assert_eq!(check("git diff --stat"), None);
    }

    #[test]
    fn every_reason_says_something_of_its_own() {
        let reasons = [
            Reason::Weacon,
            Reason::WeaconFolder,
            Reason::DeleteOutside,
            Reason::WriteOutside,
            Reason::ForcePush,
            Reason::DeleteBranch,
            Reason::ResetMainCheckout,
            Reason::CleanMainCheckout,
            Reason::NotListed,
            Reason::Credentials,
            Reason::Addition,
            Reason::Unsure,
        ];
        let texts: std::collections::BTreeSet<&str> =
            reasons.iter().map(|reason| reason.text()).collect();

        assert_eq!(texts.len(), reasons.len());
        assert!(texts.iter().all(|text| text.ends_with('.')));
        assert_eq!(Reason::ForcePush.text(), "It force-pushes.");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_file_cant_be_resolved() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("file"), "").unwrap();

        assert_eq!(resolve(&dir.join("file/inside")), None);
        assert!(resolve(&dir.join("missing/inside")).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_after_a_symlink_is_the_symlinks_parent() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        std::os::unix::fs::symlink(dir.join("main/src"), dir.join("wt/up")).unwrap();
        fs::create_dir_all(dir.join("main/src")).unwrap();

        assert_eq!(
            check_command("rm -rf up/../keep", &context),
            Some(Reason::DeleteOutside)
        );
    }

    #[test]
    fn code_hidden_in_expansions_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);
        let deep = format!("{}rm -rf /{}", "echo $(".repeat(5000), ")".repeat(5000));

        assert_eq!(check(&deep), Some(Reason::Unsure));
        let long = "a ".repeat(MAX_LINE / 2);
        assert_eq!(check(&long), Some(Reason::NotListed));
        assert_eq!(check(&format!("{long} ")), Some(Reason::Unsure));
        for line in [
            "rm -rf ${X:-$(echo /)}",
            "rm -rf ${X:-`echo /`}",
            "cat <<EOF > notes.md\n$(rm -rf /)\nEOF",
            "cat <<EOF > notes.md\n`rm -rf /`\nEOF",
            "rm -rf {/,x}",
            "rm -rf /{a..c}",
            "echo $(case x in x) rm -rf /;; esac)",
            "cd nowhere && rm -rf x",
            "env -C / rm -rf x",
            "env --chdir=/ rm -rf x",
            "env -S 'rm -rf /'",
            "GIT_DIR=/elsewhere git reset --hard",
            // Git runs its pager through `sh`, so the value is a command.
            "GIT_PAGER=cat git log",
            "GIT_PAGER=\"rm -rf ../main\" git log",
        ] {
            assert_eq!(check(line), Some(Reason::Unsure), "{line}");
        }
        assert_eq!(check("git -c user.name=me commit -m x"), None);
    }

    #[test]
    fn inline_code_and_git_settings_that_run_commands_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        for line in [
            "git -c alias.st='!rm -rf /' st",
            "git -c core.fsmonitor=evil status",
            "git --config-env=core.pager=X log",
        ] {
            assert_eq!(
                check_command(line, &context),
                Some(Reason::Unsure),
                "{line}"
            );
        }
    }

    #[test]
    fn wrappers_and_their_options_are_seen_through() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in ["find -D tree / -delete", "cd .. && rm -rf main"] {
            assert_eq!(check(line), Some(Reason::DeleteOutside), "{line}");
        }
        assert_eq!(check("echo x > /etc/hosts"), Some(Reason::WriteOutside));
        assert_eq!(
            check("terraform -chdir=infra destroy"),
            Some(Reason::Addition)
        );
    }

    #[test]
    fn reads_that_reach_credentials_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "cat < ~/.claude/token",
            "cat ~/.cl*/token",
            "cat ~/*/token",
            "X=~/.claude/token wc -c",
            "grep -r token ~",
            "cat ~/.CLAUDE/token",
        ] {
            assert_eq!(check(line), Some(Reason::Credentials), "{line}");
        }
        assert_eq!(check("ls ~"), None);
    }

    #[test]
    fn databases_drop_however_the_statement_is_spaced() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "psql -c 'DROP   DATABASE app'",
            "psql -c 'drop/* x */database app'",
            "mysql -e 'Drop\nTable users'",
            "dropdb app",
            "redis-cli FLUSHALL",
        ] {
            assert_eq!(check(line), Some(Reason::NotListed), "{line}");
        }
        assert_eq!(check("psql -c 'select 1'"), Some(Reason::NotListed));
        assert_eq!(check("psql < dump.sql"), Some(Reason::NotListed));
    }

    #[test]
    fn git_overwrites_and_discards_ask_in_the_main_checkout() {
        let dir = TempDir::new().unwrap();
        let worktree = context(&dir);
        let main = Context {
            cwd: dir.join("main"),
            ..worktree.clone()
        };

        for line in [
            "git checkout -B main",
            "git switch -C main",
            "git switch --force-create=main",
        ] {
            assert_eq!(
                check_command(line, &worktree),
                Some(Reason::DeleteBranch),
                "{line}"
            );
        }
        for line in [
            "git restore .",
            "git checkout -- notes.md",
            "git checkout .",
            "git checkout -f main",
        ] {
            assert_eq!(
                check_command(line, &main),
                Some(Reason::ResetMainCheckout),
                "{line}"
            );
            assert_eq!(check_command(line, &worktree), None, "{line}");
        }
        assert_eq!(check_command("git checkout -b feature", &worktree), None);
        assert_eq!(check_command("git stash list", &main), None);
    }

    #[test]
    fn options_and_their_values_are_read_where_they_are() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in ["nice -n 5 rm -rf /", "find -O3 / -delete"] {
            assert_eq!(check(line), Some(Reason::DeleteOutside), "{line}");
        }
        assert_eq!(check("cp notes.md -t /etc"), Some(Reason::WriteOutside));
        assert_eq!(check("ls -a ~"), None);
        assert_eq!(check("git switch main"), None);
        assert_eq!(
            credential_folders(Path::new("/h")),
            [".claude", ".claude.json", ".codex", ".config/gh"]
                .map(|name| Path::new("/h").join(name))
        );
    }

    #[test]
    fn git_discards_in_the_main_checkout_ask_however_written() {
        let dir = TempDir::new().unwrap();
        let worktree = context(&dir);
        let main = Context {
            cwd: dir.join("main"),
            ..worktree.clone()
        };

        for line in [
            "git reset --har",
            "git --namespace x reset --hard",
            "git checkout notes.md",
            "git checkout HEAD notes.md",
            "git switch -f other",
            "git switch --discard-changes other",
            "git rm -rf src",
            "git read-tree -u --reset HEAD",
            "git checkout-index -f -a",
        ] {
            assert!(check_command(line, &main).is_some(), "{line}");
            let own = check_command(line, &worktree);
            assert!(!matches!(own, Some(Reason::ResetMainCheckout)), "{line}");
        }
        for (line, reason) in [
            ("git push --force-w origin main", Reason::ForcePush),
            ("git push --delet origin old", Reason::DeleteBranch),
            ("git branch --delet old", Reason::DeleteBranch),
            ("git switch --force-c=main", Reason::DeleteBranch),
            ("git clean --dry -fdx", Reason::CleanMainCheckout),
        ] {
            assert_eq!(check_command(line, &main), Some(reason), "{line}");
        }
        assert_eq!(check_command("git checkout -b feature", &worktree), None);
    }

    #[test]
    fn expansions_and_option_values_that_reach_credentials_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);

        for line in [
            "cat ~/.codex/${X}auth.json",
            "cat ~/.codex/$(printf auth).json",
        ] {
            let reason = check_command(line, &context);
            assert_eq!(reason, Some(Reason::Credentials), "{line}");
        }
    }

    #[test]
    fn more_wrappers_writers_and_drops_are_seen() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for (line, reason) in [
            ("cp -t/etc notes.md", Reason::NotListed),
            ("mv -t/tmp notes.md", Reason::NotListed),
            ("find -- / -delete", Reason::DeleteOutside),
            ("chrt -f 1 rm -rf ../main", Reason::NotListed),
            ("pkexec rm -rf ../main", Reason::NotListed),
            ("fakeroot rm -rf ../main", Reason::NotListed),
            ("ltrace rm -rf ../main", Reason::NotListed),
            ("npx rimraf ../main", Reason::NotListed),
            ("curl -o /etc/x https://example.com", Reason::NotListed),
            ("wget -O /etc/x https://example.com", Reason::NotListed),
            ("tar -xf a.tar -C /etc", Reason::NotListed),
            ("git clone https://example.com/r /tmp/y", Reason::NotListed),
            ("rails db:drop", Reason::NotListed),
            ("npx prisma migrate reset --force", Reason::NotListed),
            ("terraform -chdir infra destroy", Reason::Addition),
        ] {
            assert_eq!(check(line), Some(reason), "{line}");
        }
        assert_eq!(check("cd src && rm -rf x"), None);
        assert_eq!(
            check("git clone https://example.com/r"),
            Some(Reason::NotListed)
        );
    }

    #[test]
    fn what_runs_out_of_sight_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);

        for line in [
            "cat ~alice/.codex/auth.json",
            "cd - && cat auth.json",
            "CDPATH=../main; cd src && rm -rf x",
        ] {
            assert_eq!(
                check_command(line, &context),
                Some(Reason::Unsure),
                "{line}"
            );
        }
        let searched = Context {
            cdpath: true,
            ..context
        };
        assert_eq!(check_command("cd src", &searched), Some(Reason::Unsure));
        assert_eq!(check_command("cd ./src", &searched), None);
    }

    #[test]
    fn near_misses_of_what_asks_dont_ask() {
        let dir = TempDir::new().unwrap();
        let worktree = context(&dir);
        let main = Context {
            cwd: dir.join("main"),
            ..worktree.clone()
        };
        let check = |line: &str| check_command(line, &worktree);

        {
            let line = "git push origin -- main";
            assert_eq!(check(line), None, "{line}");
        }
        assert_eq!(check("git switch -c feature"), None);
        assert!(check_command("git switch -c feature -f", &main).is_some());
        assert!(check_command("git switch other", &main).is_some());
        assert_eq!(check("cat ~/.cod${X}"), Some(Reason::Credentials));
        assert_eq!(check("cat ~/.codex/*${X}"), Some(Reason::Credentials));
    }

    #[test]
    fn the_review_bots_bypasses_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "cd .. ; ( cd wt ) ; rm -rf main",
            "cd .. ; cd wt | cat ; rm -rf main",
            "cd .. ; cd wt & rm -rf main",
            "cd .. ; echo $(cd wt) ; rm -rf main",
            "tcsh -c 'rm -rf /'",
            "mksh -c 'rm -rf /'",
            "ash -c 'rm -rf /'",
            "yash -c 'rm -rf /'",
            "pypy3 -c 'import shutil'",
            "nodejs -e 1",
            "/bin/r[m] -rf ../main",
            "env -C/tmp rm -rf x",
            "env -iC/tmp rm -rf x",
            "nice --adjustment 0 rm -rf ../main",
        ] {
            assert!(check(line).is_some(), "{line}");
        }
        for (line, reason) in [
            (
                "echo 'delete refs/heads/main' | git update-ref --stdin",
                Reason::NotListed,
            ),
            ("git fetch . +feature:main", Reason::DeleteBranch),
            ("git fetch -f origin x:main", Reason::DeleteBranch),
            ("ln ../main/src/lib.rs h", Reason::WriteOutside),
            ("cp -l ../main/src/lib.rs h", Reason::NotListed),
            ("cp --link ../main/src/lib.rs h", Reason::NotListed),
            ("cd ..; printf x >123", Reason::WriteOutside),
            ("echo x >&/etc/x", Reason::WriteOutside),
            ("echo `r\\m -rf ../main`", Reason::DeleteOutside),
        ] {
            assert_eq!(check(line), Some(reason), "{line}");
        }
        for line in [
            "git fetch origin main",
            "git fetch origin main:main",
            "cp notes.md copy.md",
            "make 2>&1 >build.log",
            "echo hi >&2",
            "cd src && ls",
            "cd ..; echo x >&-",
            "(echo a); cd src && rm -rf x",
        ] {
            assert_eq!(check(line), None, "{line}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_named_like_the_worktree_in_another_case_is_outside_it() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        fs::create_dir_all(dir.join("WT")).unwrap();
        // On a volume that ignores case, the two names are one folder.
        if fs::canonicalize(dir.join("WT")).unwrap() == fs::canonicalize(dir.join("wt")).unwrap() {
            return;
        }

        assert_eq!(
            check_command("rm -rf ../WT", &context),
            Some(Reason::DeleteOutside)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_wildcard_beside_a_symlink_that_leads_out_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(check("rm -rf src/*"), None);
        std::os::unix::fs::symlink(dir.join("wt/src"), dir.join("wt/inside")).unwrap();
        assert_eq!(check("rm -rf in*/x"), None);
        std::os::unix::fs::symlink(dir.join("main"), dir.join("wt/link-out")).unwrap();

        assert_eq!(check("rm -rf link*/victim"), Some(Reason::Unsure));
        assert_eq!(check("echo x > link*"), Some(Reason::Unsure));
        assert_eq!(check("rm -rf src/*"), None);
        assert_eq!(check("rm -rf src/*/x/*"), Some(Reason::Unsure));
    }

    #[test]
    fn a_cd_that_may_not_run_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "cd .. || cd wt; rm -rf main",
            "false || cd src; rm -rf x",
            "if true; then cd src; fi; rm -rf x",
        ] {
            assert_eq!(check(line), Some(Reason::Unsure), "{line}");
        }
        assert_eq!(check("if true; then echo; fi; cd src && rm -rf x"), None);
        assert_eq!(check("cd src && rm -rf x || echo failed"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_with_another_name_asks_before_it_is_written() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        fs::write(dir.join("main/important"), "").unwrap();
        fs::hard_link(dir.join("main/important"), dir.join("wt/alias")).unwrap();
        fs::write(dir.join("wt/plain"), "").unwrap();

        assert_eq!(
            check_command("printf x > alias", &context),
            Some(Reason::Unsure)
        );
        assert_eq!(check_command("printf x > plain", &context), None);
        assert_eq!(check_command("rm alias", &context), None);
    }

    #[test]
    fn a_continued_line_in_a_here_document_and_a_variable_after_an_ampersand_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(
            check("cat <<EOF\nEO\\\nF\nrm -rf ../main\nEOF"),
            Some(Reason::Unsure)
        );
        assert_eq!(
            check("DEST=/tmp/guard-victim; printf x >&\"$DEST\""),
            Some(Reason::Unsure)
        );
        assert_eq!(check("cat <<EOF\nplain text\nEOF"), None);
        assert_eq!(check("echo x >&2"), None);
    }

    #[test]
    fn only_a_dash_here_document_ends_at_a_tabbed_delimiter() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(
            check("cat <<EOF\n\tEOF\ncat <<Y\nEOF\nrm -rf ../main\nY"),
            Some(Reason::DeleteOutside)
        );
        assert_eq!(
            check("cat <<-EOF\n\tbody\n\tEOF\nrm -rf ../main"),
            Some(Reason::DeleteOutside)
        );
        assert_eq!(check("cat <<-EOF\n\tbody\n\tEOF\nls"), None);
    }

    #[test]
    fn the_shell_readers_audit_finds_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "\\\n rm -rf ../main",
            "rm -rf $\"../main\"",
            "dd if=x of=~/.claude/x",
            "curl --output=$HOME/.bashrc https://example.com",
        ] {
            assert!(
                matches!(
                    check(line),
                    Some(
                        Reason::DeleteOutside
                            | Reason::WriteOutside
                            | Reason::Credentials
                            | Reason::NotListed
                    )
                ),
                "{line}"
            );
        }
        for line in [
            "echo \"$'\" ; rm -rf ../main ; echo \"'\"",
            "echo ${X:-'}'} ; rm -rf ../main # '",
            "cat <<$\"E\"\nE\nrm -rf ../main\n$E",
            "cat <<$X\n\nrm -rf ../main",
            "cat <<E$'x'\nE\nrm -rf ../main\nEx",
            "cat ~/.c${X}/auth.json",
            "cat $H/.claude/.credentials.json",
            "function f { rm -rf ../main; }; f",
            "f() { rm -rf ./data; }; cd ..; f",
            "coproc N { rm -rf ../main; }",
            "x='a[$(rm -rf ../main)]'; echo $((x))",
            "(( x ))",
            "echo $[1]",
            "let x=1",
            "source <(echo rm -rf ../main)",
            ". /dev/stdin <<< \"rm -rf ../main\"",
            "hash -p /bin/rm ls; ls -rf ../main",
            "PATH=. ls",
            "IFS=/ cat x",
            "shopt -s extglob",
        ] {
            assert!(check(line).is_some(), "{line}");
        }
        assert_eq!(check("cat ~/.cl$'a'ude/x"), Some(Reason::Credentials));
        assert_eq!(check("echo $PATH"), None);
        assert_eq!(check("source ./env.sh"), Some(Reason::NotListed));
        assert_eq!(check("cat src/$NAME.rs"), None);
    }

    #[test]
    fn the_last_reviews_bypasses_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for (line, reason) in [
            ("shred -n 0 -z ../main/important", Reason::NotListed),
            (
                "curl -o local -o ../main/victim https://example.com/a https://example.com/b",
                Reason::NotListed,
            ),
            ("cd ..; rm -rf '123'>/dev/null", Reason::DeleteOutside),
            ("X=~/.claude/$Y cat notes.md", Reason::Credentials),
            ("stdbuf -oL rm -rf ../main", Reason::NotListed),
        ] {
            assert_eq!(check(line), Some(reason), "{line}");
        }
        for line in [
            "FILE=$(printf '%s/%s' \"$HOME\" '.codex/auth.json'); cat \"$FILE\"",
            "x='a[$(rm -rf ../main)]'\ncat <<EOF\n$[x]\nEOF",
            "cat <<EOF\n${a[x]}\nEOF",
            "echo ${a[x]}",
            "time -o ../main/important true",
        ] {
            assert_eq!(check(line), Some(Reason::Unsure), "{line}");
        }
        for line in [
            "X=$(date)",
            "echo $PATH",
            "git commit -m \"$MSG\"",
            "git commit -m \"$(cat <<'EOF'\nAdd it\n\nCo-Authored-By: someone\nEOF\n)\"",
            "cd ..; rm -rf 123>/dev/null && ls",
            "cp notes.md src",
        ] {
            assert_eq!(check(line), None, "{line}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_into_a_folder_checks_where_each_file_lands() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        fs::write(dir.join("main/important"), "").unwrap();
        fs::write(dir.join("wt/input"), "").unwrap();
        fs::create_dir_all(dir.join("wt/out")).unwrap();
        std::os::unix::fs::symlink(dir.join("main/important"), dir.join("wt/out/input")).unwrap();

        assert_eq!(
            check_command("cp input out", &context),
            Some(Reason::WriteOutside)
        );
        assert_eq!(check_command("cp input src", &context), None);
        assert_eq!(
            check_command("cp src/$NAME out", &context),
            Some(Reason::Unsure)
        );
        assert_eq!(check_command("cp in* out", &context), Some(Reason::Unsure));
    }

    #[test]
    fn reading_mistakes_from_the_branch_review_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        for line in [
            "false && cd ..; rm -rf main",
            "{ cd ..; } & rm -rf main",
            "{ cd ..; }; rm -rf main",
            "env$X rm -rf ../main",
            "git $ACTION --force",
            "git p* --force",
            "printf x > */notes.md",
        ] {
            assert_eq!(check(line), Some(Reason::Unsure), "{line}");
        }
        assert_eq!(
            check("cp -- -t notes.md ../main/x"),
            Some(Reason::WriteOutside)
        );
        assert_eq!(check("cd src && rm -rf x"), None);
        assert_eq!(check("rm -rf */node_modules"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_wildcard_write_beside_a_file_with_two_names_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        fs::write(dir.join("main/important"), "").unwrap();
        fs::hard_link(dir.join("main/important"), dir.join("wt/src/alias")).unwrap();

        assert_eq!(
            check_command("printf x > src/ali*", &context),
            Some(Reason::Unsure)
        );
        assert_eq!(check_command("printf x > wt*", &context), None);
    }

    #[test]
    fn a_copy_or_move_out_of_or_into_another_folder_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let check = |line: &str| check_command(line, &context);

        assert_eq!(check("mv notes.md -t /etc"), Some(Reason::WriteOutside));
        assert_eq!(check("cp -a ~ backup"), Some(Reason::WriteOutside));
        assert_eq!(check("dd of=$HOME/.bashrc"), Some(Reason::NotListed));
    }

    // One command a line; a written `\n` is a line break inside it and `/HOME` the home folder.
    fn cases(file: &str, home: &Path) -> Vec<String> {
        let home = home.display().to_string();
        file.lines()
            .map(|line| line.replace("\\n", "\n").replace("/HOME", &home))
            .collect()
    }

    #[test]
    fn the_findings_of_four_review_rounds_all_ask() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);

        for line in cases(include_str!("guard/cases/findings.txt"), &dir.join("home")) {
            assert!(check_command(&line, &context).is_some(), "{line}");
        }
    }

    #[test]
    fn everyday_work_runs_and_what_isnt_on_the_table_asks() {
        let dir = TempDir::new().unwrap();
        let context = context(&dir);
        let home = dir.join("home");

        for line in cases(include_str!("guard/cases/daily-runs.txt"), &home) {
            assert_eq!(check_command(&line, &context), None, "{line}");
        }
        for line in cases(include_str!("guard/cases/daily-asks.txt"), &home) {
            let reason = check_command(&line, &context);
            assert_eq!(reason, Some(Reason::NotListed), "{line}");
        }
    }
}
