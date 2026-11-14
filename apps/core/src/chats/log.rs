//! A chat's log: its committed events as JSON lines, in the order they were committed.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use weacon_model::ChatRuns;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatAction, ChatEntry, ChatEvent, ChatId, MAX_FRAME, Outcome, RequestId, RunEvent, RunUpdate,
    code,
};

use crate::platform;

// An entry, and a window of entries, each fit one frame with room for what travels with them.
const ENTRY_MAX: usize = MAX_FRAME - (32 << 10);
const WINDOW_MAX: usize = MAX_FRAME - (16 << 10);
// More missed entries than this are lost to a subscriber, which then takes a snapshot.
const REPLAY_MAX: usize = 8 * MAX_FRAME;

/// One chat's journal. Its owner keeps one per chat and awaits each call to the end: calls
/// aren't safe to cancel or to run side by side.
#[derive(Debug)]
pub(crate) struct Log {
    file: File,
    /// Where each entry starts in the file; the entry `seq` starts at `starts[seq - 1]`.
    starts: Vec<u64>,
    end: u64,
    requests: HashMap<RequestId, Asked>,
    /// Set by a failed write, which may have left half an entry; opening the log again mends it.
    broken: bool,
    runs: ChatRuns,
}

/// What the log holds of one command.
#[derive(Debug, Default)]
struct Asked {
    /// The place of its prompt, when it was one.
    prompt: Option<u64>,
    receipted: bool,
    /// Whether a receipt other than `accepted` ended it.
    ended: bool,
}

/// Whether a command was seen before under the same request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    Never,
    /// The same prompt, committed but without a receipt yet: it may not have been queued.
    Pending,
    /// With the same content: nothing more to do.
    Same,
    /// With other content.
    Other,
}

fn unreadable(error: io::Error) -> Fault {
    Fault::new(code!("chat.log_unreadable"), "The chat's log can't be read").because(error)
}

impl Log {
    /// Opens `chat`'s journal under its workspace's own folder, `workspace_root`, created
    /// private when missing and kept through a power loss. An entry a crash cut short is dropped.
    ///
    /// # Errors
    /// When the journal can't be read or an entry before its last is damaged.
    pub(crate) async fn open(workspace_root: &Path, chat: &ChatId) -> Result<Self, Fault> {
        let dir = workspace_root.join("chats");
        let path = dir.join(super::file_stem(chat) + ".jsonl");
        let workspace_root = workspace_root.to_path_buf();
        blocking(move || {
            platform::private_dir(&dir)?;
            let mut log = Self {
                file: weacon_appkit::private_append(&path)?,
                starts: Vec::new(),
                end: 0,
                requests: HashMap::new(),
                broken: false,
                runs: ChatRuns::default(),
            };
            weacon_appkit::sync_dir(&dir)?;
            weacon_appkit::sync_dir(&workspace_root)?;
            let len = log.file.metadata()?.len();
            log.replay()?;
            if log.end != len {
                // Not through `file`: on Windows a handle opened to append can't truncate.
                OpenOptions::new()
                    .write(true)
                    .open(&path)?
                    .set_len(log.end)?;
            }
            Ok(log)
        })
        .await
        .map_err(unreadable)
    }

    fn replay(&mut self) -> io::Result<()> {
        let mut lines = BufReader::new(self.file.try_clone()?);
        let mut line = Vec::new();
        loop {
            line.clear();
            if lines.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            let entry = serde_json::from_slice::<ChatEntry>(&line)
                .ok()
                .filter(|entry| line.ends_with(b"\n") && entry.seq == self.seq() + 1);
            match entry {
                Some(entry) => self.keep(&entry, line.len()),
                // Only the last entry can be one a crash cut short.
                None if lines.fill_buf()?.is_empty() => return Ok(()),
                None => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    }

    fn keep(&mut self, entry: &ChatEntry, len: usize) {
        self.runs.apply(entry);
        match &entry.event {
            ChatEvent::Prompt { request, .. } => {
                self.requests.entry(request.clone()).or_default().prompt = Some(entry.seq);
            }
            ChatEvent::Receipt { receipt } => {
                let asked = self.requests.entry(receipt.request.clone()).or_default();
                asked.receipted = true;
                asked.ended |= receipt.outcome != Outcome::Accepted;
            }
            ChatEvent::Run { .. } | ChatEvent::RunStatus { .. } | ChatEvent::Newer => {}
        }
        self.starts.push(self.end);
        self.end += u64::try_from(len).unwrap_or(u64::MAX);
    }

    /// Where the chat's runs stand, as its entries say.
    pub(crate) const fn runs(&self) -> &ChatRuns {
        &self.runs
    }

    /// The last entry's place, 0 while the log is empty.
    pub(crate) fn seq(&self) -> u64 {
        u64::try_from(self.starts.len()).unwrap_or(u64::MAX)
    }

    /// Commits `event` as the next entry, on disk before it is returned.
    ///
    /// # Errors
    /// `chat.delta_not_logged` for a text delta and `chat.event_too_large` for an event that
    /// doesn't fit one frame, which both leave the log as it was; `chat.log_not_written` when
    /// the write fails, after which the log must be opened again.
    pub(crate) async fn append(&mut self, event: ChatEvent) -> Result<ChatEntry, Fault> {
        let not_written = || {
            Fault::new(
                code!("chat.log_not_written"),
                "The chat's log can't be written",
            )
        };
        if matches!(
            event,
            ChatEvent::Run {
                update: RunUpdate {
                    event: RunEvent::TextDelta { .. },
                    ..
                }
            }
        ) {
            return Err(Fault::new(
                code!("chat.delta_not_logged"),
                "A text delta is never committed",
            ));
        }
        let entry = ChatEntry {
            seq: self.seq() + 1,
            event,
        };
        let mut line = serde_json::to_vec(&entry).map_err(|error| not_written().because(error))?;
        line.push(b'\n');
        if line.len() > ENTRY_MAX {
            return Err(Fault::new(
                code!("chat.event_too_large"),
                "The event doesn't fit one frame",
            ));
        }
        let len = line.len();
        let written = match self.usable().and_then(|()| self.file.try_clone()) {
            Ok(mut file) => {
                blocking(move || file.write_all(&line).and_then(|()| file.sync_data())).await
            }
            Err(error) => Err(error),
        };
        if let Err(error) = written {
            self.broken = true;
            return Err(not_written().because(error));
        }
        self.keep(&entry, len);
        Ok(entry)
    }

    /// The latest entries before `before`: at most `limit`, and no more than fit one frame.
    pub(crate) async fn window(&self, before: u64, limit: u32) -> Result<Vec<ChatEntry>, Fault> {
        let last = before.saturating_sub(1).min(self.seq());
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let first = (1..=last)
            .rev()
            .take(limit)
            .take_while(|first| self.bytes(*first, last) <= WINDOW_MAX)
            .last()
            .unwrap_or(last + 1);
        self.read(first, last).await
    }

    /// Every entry after `after`, or `None` when they are too many to send again or the log
    /// never got as far as `after`.
    pub(crate) async fn since(&self, after: u64) -> Result<Option<Vec<ChatEntry>>, Fault> {
        if after > self.seq() || self.bytes(after + 1, self.seq()) > REPLAY_MAX {
            return Ok(None);
        }
        self.read(after + 1, self.seq()).await.map(Some)
    }

    /// Whether a command under `request` was committed before, and with the same content. Only
    /// a prompt's content is kept, in the prompt entry its owner commits before any receipt, so
    /// any other command counts as the same.
    pub(crate) async fn seen(
        &self,
        request: &RequestId,
        action: &ChatAction,
    ) -> Result<Seen, Fault> {
        let Some(asked) = self.requests.get(request) else {
            return Ok(Seen::Never);
        };
        let Some(seq) = asked.prompt else {
            let same = !matches!(action, ChatAction::Prompt { .. });
            return Ok(if same { Seen::Same } else { Seen::Other });
        };
        let earlier = self.read(seq, seq).await?.pop().map(|entry| entry.event);
        let same = matches!(
            (earlier, action),
            (
                Some(ChatEvent::Prompt { text: was_text, repo: was_repo, .. }),
                ChatAction::Prompt { text, repo },
            ) if was_text == *text && was_repo == *repo
        );
        Ok(match (same, asked.receipted) {
            (true, true) => Seen::Same,
            (true, false) => Seen::Pending,
            (false, _) => Seen::Other,
        })
    }

    /// Whether a receipt that ends the command under `request` is in the log.
    pub(crate) fn has_ended(&self, request: &RequestId) -> bool {
        self.requests.get(request).is_some_and(|asked| asked.ended)
    }

    async fn read(&self, first: u64, last: u64) -> Result<Vec<ChatEntry>, Fault> {
        if first > last {
            return Ok(Vec::new());
        }
        let (start, len) = (self.start_of(first), self.bytes(first, last));
        let file = self.usable().and_then(|()| self.file.try_clone());
        blocking(move || {
            let mut file = file?;
            let mut bytes = vec![0; len];
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut bytes)?;
            bytes
                .split_inclusive(|byte| *byte == b'\n')
                .map(|line| serde_json::from_slice(line).map_err(io::Error::from))
                .collect()
        })
        .await
        .map_err(unreadable)
    }

    /// Makes the log behave as after a failed write.
    #[cfg(test)]
    pub(crate) const fn jam(&mut self) {
        self.broken = true;
    }

    fn usable(&self) -> io::Result<()> {
        if self.broken {
            return Err(io::Error::other("a write to the log failed"));
        }
        Ok(())
    }

    // How many bytes the entries `first..=last` take in the file.
    fn bytes(&self, first: u64, last: u64) -> usize {
        usize::try_from(self.start_of(last + 1).saturating_sub(self.start_of(first)))
            .unwrap_or(usize::MAX)
    }

    fn start_of(&self, seq: u64) -> u64 {
        usize::try_from(seq.saturating_sub(1))
            .ok()
            .and_then(|at| self.starts.get(at))
            .copied()
            .unwrap_or(self.end)
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(io::Error::other(error)))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use weacon_protocol::{
        ChatAnswer, ChatAnswerKind, ChatUpdate, Envelope, Message, Outcome, PermissionAnswer,
        Receipt, RepoId, RunId, WorkspaceId, encode_line,
    };
    use weacon_testing::TempDir;

    use super::*;

    fn chat(id: &str) -> ChatId {
        ChatId::try_from(id.to_owned()).unwrap()
    }

    fn request(id: &str) -> RequestId {
        RequestId::try_from(id.to_owned()).unwrap()
    }

    fn run() -> RunId {
        RunId::try_from("run-1".to_owned()).unwrap()
    }

    fn longest_workspace() -> WorkspaceId {
        WorkspaceId::try_from("w".repeat(64)).unwrap()
    }

    fn prompt(id: &str, text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: request(id),
            text: text.to_owned(),
            repo: None,
        }
    }

    fn accepted(id: &str) -> ChatEvent {
        ChatEvent::Receipt {
            receipt: Receipt {
                request: request(id),
                outcome: Outcome::Accepted,
            },
        }
    }

    fn said(text: String) -> ChatEvent {
        ChatEvent::Run {
            update: RunUpdate {
                run: run(),
                event: RunEvent::Text { text },
            },
        }
    }

    // A text event whose entry at `seq` takes exactly `len` bytes in the log.
    fn sized(seq: u64, len: usize) -> ChatEvent {
        let empty = ChatEntry {
            seq,
            event: said(String::new()),
        };
        let overhead = serde_json::to_vec(&empty).unwrap().len() + 1;
        said("a".repeat(len - overhead))
    }

    fn asks(text: &str) -> ChatAction {
        ChatAction::Prompt {
            text: text.to_owned(),
            repo: None,
        }
    }

    fn journal(root: &Path) -> PathBuf {
        root.join("chats/c-c-1.jsonl")
    }

    async fn open(root: &Path) -> Log {
        Log::open(root, &chat("c-1")).await.unwrap()
    }

    async fn since(log: &Log, after: u64) -> Option<Vec<u64>> {
        let found = log.since(after).await.unwrap();
        found.map(|found| seqs(&found))
    }

    async fn seen(log: &Log, id: &str, action: ChatAction) -> Seen {
        log.seen(&request(id), &action).await.unwrap()
    }

    async fn window(log: &Log, before: u64, limit: u32) -> Vec<u64> {
        seqs(&log.window(before, limit).await.unwrap())
    }

    fn seqs(entries: &[ChatEntry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.seq).collect()
    }

    #[tokio::test]
    async fn entries_are_numbered_from_one_and_survive_a_restart() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        assert_eq!(log.seq(), 0);
        assert_eq!(log.window(9, 9).await.unwrap(), []);

        let first = log.append(prompt("r-1", "Hi")).await.unwrap();
        let second = log.append(accepted("r-1")).await.unwrap();
        drop(log);
        let mut reopened = open(&dir).await;
        let third = reopened.append(said("Hello.".to_owned())).await.unwrap();

        assert_eq!(
            first,
            ChatEntry {
                seq: 1,
                event: prompt("r-1", "Hi")
            }
        );
        assert_eq!(seqs(&[second.clone(), third.clone()]), [2, 3]);
        assert_eq!(reopened.seq(), 3);
        assert_eq!(
            open(&dir).await.window(4, 9).await.unwrap(),
            [first, second, third]
        );
    }

    #[tokio::test]
    async fn chats_keep_apart_by_workspace_and_by_case_in_private_files() {
        let dir = TempDir::new().unwrap();
        let (one, other) = (dir.join("w-one"), dir.join("w-other"));
        let mut upper = Log::open(&one, &chat("Ab")).await.unwrap();
        upper.append(prompt("r-1", "Hi")).await.unwrap();

        let lower = Log::open(&one, &chat("ab")).await.unwrap();
        let elsewhere = Log::open(&other, &chat("Ab")).await.unwrap();

        assert_eq!((lower.seq(), elsewhere.seq()), (0, 0));
        assert!(one.join("chats/c-.ab.jsonl").exists());
        assert!(one.join("chats/c-ab.jsonl").exists());
        assert!(other.join("chats/c-.ab.jsonl").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&one.join("chats")), 0o700);
            assert_eq!(mode(&one.join("chats/c-ab.jsonl")), 0o600);
        }
    }

    #[tokio::test]
    async fn an_entry_cut_short_is_dropped_and_a_damaged_one_before_it_stops_the_open() {
        let dir = TempDir::new().unwrap();
        let path = journal(&dir);
        let mut log = open(&dir).await;
        log.append(prompt("r-1", "Hi")).await.unwrap();
        drop(log);
        let whole = fs::read(&path).unwrap();

        for torn in [&b"{\"seq\":2,\"eve"[..], b"{\"seq\":2}\n", b"\n", &whole] {
            fs::write(&path, [&whole[..], torn].concat()).unwrap();
            let mut reopened = open(&dir).await;
            assert_eq!(reopened.seq(), 1);
            assert_eq!(fs::read(&path).unwrap(), whole);
            assert_eq!(reopened.append(accepted("r-1")).await.unwrap().seq, 2);
            assert_eq!(window(&open(&dir).await, 9, 9).await, [1, 2]);
        }

        // The second entry has the wrong place and another follows it.
        fs::write(&path, [&whole[..], &whole[..], &whole[..]].concat()).unwrap();
        let damaged = Log::open(&dir, &chat("c-1")).await.unwrap_err();
        assert_eq!(damaged.code().as_str(), "chat.log_unreadable");
    }

    #[tokio::test]
    async fn a_failed_write_breaks_the_log_until_it_is_opened_again() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        log.append(prompt("r-1", "Hi")).await.unwrap();
        log.file = File::open(journal(&dir)).unwrap();

        let failed = log.append(accepted("r-1")).await.unwrap_err();
        let after = log.append(accepted("r-1")).await.unwrap_err();
        let read = log.window(9, 9).await.unwrap_err();

        assert_eq!(failed.code().as_str(), "chat.log_not_written");
        assert_eq!(after.code().as_str(), "chat.log_not_written");
        assert_eq!(read.code().as_str(), "chat.log_unreadable");
        assert_eq!(log.seq(), 1);
        let mut reopened = open(&dir).await;
        assert_eq!(reopened.append(accepted("r-1")).await.unwrap().seq, 2);
    }

    #[tokio::test]
    async fn a_text_delta_or_an_event_too_large_for_a_frame_is_never_committed() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        let delta = ChatEvent::Run {
            update: RunUpdate {
                run: run(),
                event: RunEvent::TextDelta {
                    text: "Hel".to_owned(),
                },
            },
        };

        let refused = log.append(delta).await.unwrap_err();
        let too_large = log.append(sized(1, ENTRY_MAX + 1)).await.unwrap_err();
        let largest = log.append(sized(1, ENTRY_MAX)).await.unwrap();

        assert_eq!(refused.code().as_str(), "chat.delta_not_logged");
        assert_eq!(too_large.code().as_str(), "chat.event_too_large");
        assert_eq!(open(&dir).await.seq(), 1);
        let update = Envelope {
            id: u32::MAX,
            message: Message::Chat(ChatUpdate {
                workspace: longest_workspace(),
                chat: chat(&"c".repeat(64)),
                entry: largest,
            }),
        };
        encode_line(&update).unwrap();
    }

    #[tokio::test]
    async fn a_window_holds_the_latest_entries_before_a_place_up_to_a_count_and_a_frame() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        for _ in 0..5 {
            log.append(said("Hello.".to_owned())).await.unwrap();
        }

        assert_eq!(window(&log, 4, 2).await, [2, 3]);
        assert_eq!(window(&log, 4, 9).await, [1, 2, 3]);
        assert_eq!(window(&log, 99, 2).await, [4, 5]);
        assert_eq!(window(&log, 6, 1).await, [5]);
        assert!(window(&log, 1, 9).await.is_empty());
        assert!(window(&log, 0, 9).await.is_empty());
        assert!(window(&log, 4, 0).await.is_empty());

        let small = serde_json::to_vec(&log.window(2, 1).await.unwrap())
            .unwrap()
            .len()
            - 1;
        log.append(sized(6, WINDOW_MAX / 2)).await.unwrap();
        log.append(sized(7, WINDOW_MAX - WINDOW_MAX / 2 - small))
            .await
            .unwrap();
        log.append(said("Hello.".to_owned())).await.unwrap();

        assert_eq!(window(&log, 9, 9).await, [6, 7, 8]);
        assert_eq!(window(&log, 8, 9).await, [5, 6, 7]);
        log.append(said("Hello!".to_owned())).await.unwrap();
        assert_eq!(window(&log, 10, 9).await, [7, 8, 9]);
        let answer = Envelope {
            id: u32::MAX,
            message: Message::ChatAnswer(ChatAnswer {
                workspace: longest_workspace(),
                chat: chat(&"c".repeat(64)),
                answer: ChatAnswerKind::Window {
                    events: log.window(9, 9).await.unwrap(),
                },
            }),
        };
        encode_line(&answer).unwrap();
    }

    #[tokio::test]
    async fn entries_since_a_place_are_all_there_or_lost() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        for _ in 0..4 {
            log.append(said("Hello.".to_owned())).await.unwrap();
        }

        assert_eq!(since(&log, 0).await, Some(vec![1, 2, 3, 4]));
        assert_eq!(since(&log, 2).await, Some(vec![3, 4]));
        assert_eq!(since(&log, 4).await, Some(vec![]));
        assert_eq!(since(&log, 5).await, None);

        let small = log.bytes(4, 4);
        for seq in 5..=20 {
            log.append(sized(seq, MAX_FRAME / 2)).await.unwrap();
        }
        assert_eq!(log.bytes(4, 20), REPLAY_MAX + small);
        assert_eq!(since(&log, 3).await, None);
        assert_eq!(since(&log, 4).await.map(|found| found.len()), Some(16));
    }

    #[tokio::test]
    async fn a_repeated_request_is_the_same_or_another_command() {
        let dir = TempDir::new().unwrap();
        let mut log = open(&dir).await;
        let allow = ChatAction::Permission {
            answer: PermissionAnswer {
                run: run(),
                prompt: 1,
                allow: true,
            },
        };
        assert_eq!(seen(&log, "r-1", asks("Hi")).await, Seen::Never);
        log.append(said("Hello.".to_owned())).await.unwrap();
        log.append(prompt("r-1", "Hi")).await.unwrap();
        assert_eq!(seen(&log, "r-1", asks("Hi")).await, Seen::Pending);
        assert_eq!(seen(&log, "r-1", asks("Bye")).await, Seen::Other);
        log.append(accepted("r-1")).await.unwrap();
        log.append(accepted("r-2")).await.unwrap();
        log.append(prompt("r-4", "Hi")).await.unwrap();
        drop(log);

        let log = open(&dir).await;
        assert_eq!(seen(&log, "r-1", asks("Hi")).await, Seen::Same);
        assert_eq!(seen(&log, "r-1", asks("Bye")).await, Seen::Other);
        let elsewhere = ChatAction::Prompt {
            text: "Hi".to_owned(),
            repo: Some(RepoId::try_from("site".to_owned()).unwrap()),
        };
        assert_eq!(seen(&log, "r-1", elsewhere).await, Seen::Other);
        assert_eq!(seen(&log, "r-1", ChatAction::Interrupt).await, Seen::Other);
        assert_eq!(seen(&log, "r-2", ChatAction::Interrupt).await, Seen::Same);
        assert_eq!(seen(&log, "r-2", allow).await, Seen::Same);
        assert_eq!(seen(&log, "r-2", asks("Hi")).await, Seen::Other);
        assert_eq!(seen(&log, "r-3", ChatAction::Interrupt).await, Seen::Never);
        assert_eq!(seen(&log, "r-4", asks("Hi")).await, Seen::Pending);
    }
}
