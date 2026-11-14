//! The session files a person's own agents keep, found without opening anything else.

use std::cmp::Reverse;
use std::ffi::OsString;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use weacon_protocol::{AgentKind, AgentSession};

use crate::platform;

/// What finding and reading session files needs from a file system.
pub trait Files {
    /// The entries of `dir`, each with a link's own kind, never its target's.
    ///
    /// # Errors
    /// When `dir` can't be listed.
    fn entries(&self, dir: &Path) -> io::Result<Vec<Entry>>;

    /// At most `limit` bytes of `file` from byte `from`.
    ///
    /// # Errors
    /// When `file` can't be read, or isn't a regular file with one link.
    fn read(&self, file: &Path, from: u64, limit: u64) -> io::Result<Vec<u8>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File {
        len: u64,
        modified: SystemTime,
    },
    /// A link, or anything else that is neither.
    Other,
}

/// This machine's own file system. Blocking: async callers use `spawn_blocking`.
#[derive(Clone, Copy, Debug)]
pub struct Disk;

impl Files for Disk {
    fn entries(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        Ok(std::fs::read_dir(dir)?
            .filter_map(|entry| entry_of(&entry.ok()?).ok())
            .collect())
    }

    fn read(&self, file: &Path, from: u64, limit: u64) -> io::Result<Vec<u8>> {
        let mut opened = platform::open_plain(file)?;
        opened.seek(SeekFrom::Start(from))?;
        let mut bytes = Vec::new();
        opened.take(limit).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

fn entry_of(entry: &std::fs::DirEntry) -> io::Result<Entry> {
    let kind = entry.file_type()?;
    let kind = if kind.is_dir() {
        Kind::Dir
    } else if kind.is_file() {
        let metadata = entry.metadata()?;
        Kind::File {
            len: metadata.len(),
            modified: metadata.modified()?,
        }
    } else {
        Kind::Other
    };
    Ok(Entry {
        name: entry.file_name(),
        kind,
    })
}

/// A session the person ran with their own agent, outside Weacon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    agent: AgentKind,
    session: AgentSession,
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

impl SessionFile {
    #[must_use]
    pub const fn agent(&self) -> AgentKind {
        self.agent
    }

    #[must_use]
    pub const fn session(&self) -> &AgentSession {
        &self.session
    }

    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn modified(&self) -> SystemTime {
        self.modified
    }

    /// At most `limit` bytes of the session's file from byte `from`.
    ///
    /// # Errors
    /// When the file can't be read, or is no longer a regular file with one link.
    pub fn read(&self, files: &impl Files, from: u64, limit: u64) -> io::Result<Vec<u8>> {
        files.read(&self.path, from, limit)
    }
}

/// The sessions `agent` keeps in `config_dir`, the person's own config folder for it, newest
/// first. Only the vendor's session folders are listed, and only files named as sessions are
/// kept; folders that can't be listed are left out.
pub fn find(files: &impl Files, agent: AgentKind, config_dir: &Path) -> Vec<SessionFile> {
    let mut found = match agent {
        AgentKind::Claude => dirs(files, &config_dir.join("projects"), |name| {
            !name.starts_with('.')
        })
        .iter()
        .flat_map(|project| sessions(files, agent, project, claude_session))
        .collect(),
        AgentKind::Codex => dirs(files, &config_dir.join("sessions"), digits(4))
            .iter()
            .flat_map(|year| dirs(files, year, digits(2)))
            .flat_map(|month| dirs(files, &month, digits(2)))
            .chain([config_dir.join("archived_sessions")])
            .flat_map(|day| sessions(files, agent, &day, codex_session))
            .collect(),
        AgentKind::Newer => Vec::new(),
    };
    found.sort_by_key(|file| Reverse(file.modified));
    found
}

fn dirs(files: &impl Files, dir: &Path, named: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    entries(files, dir)
        .filter(|entry| entry.kind == Kind::Dir)
        .filter_map(|entry| Some(dir.join(entry.name.to_str().filter(|name| named(name))?)))
        .collect()
}

fn sessions(
    files: &impl Files,
    agent: AgentKind,
    dir: &Path,
    session_of: fn(&str) -> Option<AgentSession>,
) -> Vec<SessionFile> {
    entries(files, dir)
        .filter_map(|entry| {
            let Kind::File { len, modified } = entry.kind else {
                return None;
            };
            let name = entry.name.to_str()?;
            Some(SessionFile {
                agent,
                session: session_of(name)?,
                path: dir.join(name),
                len,
                modified,
            })
        })
        .collect()
}

fn entries(files: &impl Files, dir: &Path) -> impl Iterator<Item = Entry> {
    files.entries(dir).unwrap_or_default().into_iter()
}

fn digits(count: usize) -> impl Fn(&str) -> bool {
    move |name| name.len() == count && name.bytes().all(|byte| byte.is_ascii_digit())
}

// `<session>.jsonl`.
fn claude_session(name: &str) -> Option<AgentSession> {
    AgentSession::try_from(name.strip_suffix(".jsonl")?.to_owned()).ok()
}

const SESSION_LEN: usize = 36;

// `rollout-<time>-<session>.jsonl`, the time in digits, `-` and `T`.
fn codex_session(name: &str) -> Option<AgentSession> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let at = stem.len().checked_sub(SESSION_LEN)?;
    let time = stem.get(..at)?.strip_suffix('-')?;
    let is_time = |byte: u8| byte.is_ascii_digit() || matches!(byte, b'-' | b'T');
    if time.is_empty() || !time.bytes().all(is_time) {
        return None;
    }
    AgentSession::try_from(stem.get(at..)?.to_owned()).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::time::Duration;

    use weacon_testing::TempDir;

    use super::*;

    const ONE: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
    const TWO: &str = "1c8d7b63-4a2f-4e3b-8d9c-6f5a4b3c2d1e";

    /// The real disk, recording every folder listed and every file read.
    #[derive(Default)]
    struct Recording {
        listed: RefCell<Vec<PathBuf>>,
        read: RefCell<Vec<PathBuf>>,
    }

    impl Files for Recording {
        fn entries(&self, dir: &Path) -> io::Result<Vec<Entry>> {
            self.listed.borrow_mut().push(dir.to_path_buf());
            Disk.entries(dir)
        }

        fn read(&self, file: &Path, from: u64, limit: u64) -> io::Result<Vec<u8>> {
            self.read.borrow_mut().push(file.to_path_buf());
            Disk.read(file, from, limit)
        }
    }

    fn write(file: &Path, text: &str, age: u64) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age);
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    fn sessions_of(found: &[SessionFile]) -> Vec<&str> {
        found.iter().map(|file| file.session().as_str()).collect()
    }

    fn session(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.jsonl"))
    }

    #[test]
    fn claude_sessions_are_found_newest_first_and_nothing_else_is_opened() {
        let dir = TempDir::new().unwrap();
        let projects = dir.join("projects");
        let (app, web) = (projects.join("-work-app"), projects.join("-work-web"));
        write(&dir.join(".credentials.json"), "secret", 0);
        write(&projects.join(".credentials.json"), "secret", 0);
        write(&session(&app, ONE), "one\n", 9);
        write(&session(&web, TWO), "two\n", 1);
        write(&session(&web, "notes"), "", 0);
        write(&session(&web, &ONE.to_uppercase()), "", 0);
        write(&web.join(TWO).join("sub.json"), "", 0);
        write(&session(&projects.join(".hidden"), TWO), "", 0);
        let files = Recording::default();

        let found = find(&files, AgentKind::Claude, &dir);

        assert_eq!(sessions_of(&found), [TWO, ONE]);
        assert_eq!(found.first().unwrap().read(&files, 1, 2).unwrap(), b"wo");
        let listed = files.listed.borrow();
        assert_eq!(listed.first(), Some(&projects));
        assert!(
            listed
                .iter()
                .skip(1)
                .all(|dir| dir.parent() == Some(&*projects))
        );
        assert_eq!(listed.len(), 3);
        assert_eq!(*files.read.borrow(), [session(&web, TWO)]);
    }

    #[test]
    fn a_found_session_says_what_its_file_is() {
        let dir = TempDir::new().unwrap();
        write(&session(&dir.join("projects").join("-w"), ONE), "one\n", 9);

        let found = find(&Disk, AgentKind::Claude, &dir);

        let file = found.first().unwrap();
        assert_eq!(
            (file.agent(), file.len(), file.is_empty()),
            (AgentKind::Claude, 4, false)
        );
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(999_991);
        assert_eq!(file.modified(), modified);
    }

    #[test]
    fn a_session_with_nothing_written_yet_is_empty() {
        let dir = TempDir::new().unwrap();
        write(&session(&dir.join("projects").join("-w"), ONE), "", 0);

        let found = find(&Disk, AgentKind::Claude, &dir);

        let file = found.first().unwrap();
        assert_eq!((file.len(), file.is_empty()), (0, true));
    }

    fn rollout(dir: &Path, session: &str) -> PathBuf {
        dir.join(format!("rollout-2026-11-13T10-00-00-{session}.jsonl"))
    }

    #[test]
    fn codex_sessions_are_found_by_day_and_in_the_archive() {
        let dir = TempDir::new().unwrap();
        let day = dir.join("sessions").join("2026").join("11").join("13");
        write(&dir.join("auth.json"), "secret", 0);
        write(&day.join("auth.json"), "secret", 0);
        write(&rollout(&day, ONE), "", 5);
        write(&rollout(&dir.join("archived_sessions"), TWO), "", 7);
        for (year, month, day) in [
            ("202", "11", "13"),
            ("20261", "11", "13"),
            ("x026", "11", "13"),
            ("2026", "1", "13"),
            ("2026", "11", "1x"),
        ] {
            let folder = dir.join("sessions").join(year).join(month).join(day);
            write(&rollout(&folder, TWO), "", 0);
        }

        let found = find(&Disk, AgentKind::Codex, &dir);

        assert_eq!(sessions_of(&found), [ONE, TWO]);
        assert!(found.iter().all(|file| file.agent() == AgentKind::Codex));
    }

    #[test]
    fn only_files_named_as_codex_sessions_count() {
        let dir = TempDir::new().unwrap();
        let archive = dir.join("archived_sessions");
        for name in [
            format!("{ONE}.jsonl"),
            format!("rollout-{ONE}.jsonl"),
            format!("rollout--{ONE}.jsonl"),
            format!("rollout-2026x-{ONE}.jsonl"),
            format!("rollout-2026-11-13T10-00-00{ONE}.jsonl"),
            format!("rollout-2026-11-13T10-00-00-{ONE}.json"),
            format!("session-2026-11-13T10-00-00-{ONE}.jsonl"),
            "rollout-2026-11-13T10-00-00-x.jsonl".to_owned(),
        ] {
            write(&archive.join(name), "", 0);
        }

        assert_eq!(find(&Disk, AgentKind::Codex, &dir), []);
    }

    #[test]
    fn a_missing_folder_or_an_agent_this_build_doesnt_know_finds_nothing() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.join("projects").join("-w").join(format!("{ONE}.jsonl")),
            "",
            0,
        );

        assert_eq!(find(&Disk, AgentKind::Newer, &dir), []);
        assert_eq!(find(&Disk, AgentKind::Codex, &dir), []);
        assert_eq!(find(&Disk, AgentKind::Claude, &dir.join("missing")), []);
    }

    #[test]
    fn a_second_name_of_another_file_is_listed_but_never_read() {
        let dir = TempDir::new().unwrap();
        let secret = dir.join(".credentials.json");
        write(&secret, "secret", 0);
        let project = dir.join("projects").join("-w");
        fs::create_dir_all(&project).unwrap();
        fs::hard_link(&secret, project.join(format!("{ONE}.jsonl"))).unwrap();

        let found = find(&Disk, AgentKind::Claude, &dir);

        assert_eq!(sessions_of(&found), [ONE]);
        found.first().unwrap().read(&Disk, 0, 100).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn links_to_a_credential_are_neither_listed_nor_followed() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let secret = dir.join(".credentials.json");
        write(&secret, "secret", 0);
        let elsewhere = dir.join("elsewhere");
        write(&elsewhere.join(format!("{TWO}.jsonl")), "", 0);
        let projects = dir.join("projects");
        fs::create_dir_all(projects.join("-w")).unwrap();
        symlink(&secret, projects.join("-w").join(format!("{ONE}.jsonl"))).unwrap();
        symlink(&elsewhere, projects.join("-linked")).unwrap();

        assert_eq!(find(&Disk, AgentKind::Claude, &dir), []);
        let entries = Disk.entries(&projects.join("-w")).unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
            [Kind::Other]
        );
        Disk.read(&projects.join("-w").join(format!("{ONE}.jsonl")), 0, 100)
            .unwrap_err();
    }
}
