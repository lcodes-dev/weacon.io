use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatEntry, ChatEvent, ChatId, ChatState, ChatSummary, MachineName, MemberId, RepoId, RequestId,
    code,
};

use super::{Workspace, failed, stored, unreadable};

/// The most bytes of chat events a workspace keeps: 512 MiB.
pub(crate) const CHAT_QUOTA: i64 = 512 << 20;

/// A chat as its first prompt makes it.
#[derive(Debug)]
pub(crate) struct NewChat<'a> {
    pub(crate) id: &'a ChatId,
    pub(crate) owner: &'a MemberId,
    pub(crate) title: &'a str,
    pub(crate) repo: Option<&'a RepoId>,
    pub(crate) machine: &'a MachineName,
    pub(crate) at: i64,
}

/// A command a member sent a chat, as the ledger keeps it.
#[derive(Debug)]
pub(crate) struct NewRequest<'a> {
    pub(crate) id: &'a RequestId,
    pub(crate) chat: &'a ChatId,
    pub(crate) member: &'a MemberId,
    /// Tells a retry from another command under the same id.
    pub(crate) action_hash: &'a [u8],
    pub(crate) at: i64,
}

/// What the server knows of a chat beyond its summary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Chat {
    pub(crate) owner: MemberId,
    pub(crate) state: ChatState,
    pub(crate) seq: u64,
    /// The last of its machine's events it has.
    pub(crate) core_seq: u64,
}

/// Whether a request was seen before.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Recorded {
    New,
    /// The same command again; `open` until its receipt ended it.
    Same {
        open: bool,
    },
    /// Another command under the same id.
    Conflict,
}

/// An event to store, with its place in its machine's log, when it came from there.
#[derive(Debug)]
pub(crate) struct Incoming<'a> {
    pub(crate) core_seq: Option<u64>,
    pub(crate) event: &'a ChatEvent,
    pub(crate) at: i64,
}

type ChatRow = (String, String, i64, i64);
type SummaryRow = (String, String, String, String, String, i64);

impl Workspace {
    /// Makes the chat `chat` with the request that made it. A chat id already in this
    /// workspace is `db.exists`.
    pub(crate) async fn add_chat(
        &self,
        chat: &NewChat<'_>,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.writer.begin().await.map_err(failed)?;
        sqlx::query(
            "INSERT INTO chats (workspace_id, chat_id, owner_member, title, repo, machine_name,
                 state, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 'open', ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.id.as_str())
        .bind(chat.owner.as_str())
        .bind(chat.title)
        .bind(chat.repo.map(RepoId::as_str))
        .bind(chat.machine.as_str())
        .bind(chat.at)
        .bind(chat.at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.insert_request(&mut transaction, request).await?;
        transaction.commit().await.map_err(failed)
    }

    /// The chat `id`, if this workspace has it.
    pub(crate) async fn chat(&self, id: &ChatId) -> Result<Option<Chat>, Fault> {
        sqlx::query_as::<_, ChatRow>(
            "SELECT owner_member, state, seq, core_seq FROM chats
             WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(|(owner, state, seq, core_seq)| {
            Ok(Chat {
                owner: stored(owner)?,
                state: stored_state(&state)?,
                seq: count(seq)?,
                core_seq: count(core_seq)?,
            })
        })
        .transpose()
    }

    /// The workspace's chats, the latest updated first.
    pub(crate) async fn chats(&self) -> Result<Vec<ChatSummary>, Fault> {
        sqlx::query_as::<_, SummaryRow>(
            "SELECT chat_id, title, owner_member, machine_name, state, updated_at FROM chats
             WHERE workspace_id = ? ORDER BY updated_at DESC, chat_id",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, title, owner, machine, state, updated_at)| {
            Ok(ChatSummary {
                id: stored(id)?,
                title,
                owner: stored(owner)?,
                machine: stored(machine)?,
                state: stored_state(&state)?,
                updated_at,
            })
        })
        .collect()
    }

    pub(crate) async fn set_chat_state(&self, id: &ChatId, state: ChatState) -> Result<(), Fault> {
        sqlx::query("UPDATE chats SET state = ? WHERE workspace_id = ? AND chat_id = ?")
            .bind(state_text(state)?)
            .bind(self.id.as_str())
            .bind(id.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// Records `request` in the ledger, unless it is there already.
    pub(crate) async fn record_request(&self, request: &NewRequest<'_>) -> Result<Recorded, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let earlier = sqlx::query_as::<_, (String, Vec<u8>, i64)>(
            "SELECT chat_id, action_hash, open FROM chat_requests
             WHERE workspace_id = ? AND request_id = ?",
        )
        .bind(self.id.as_str())
        .bind(request.id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let recorded = match earlier {
            None => {
                self.insert_request(&mut transaction, request).await?;
                Recorded::New
            }
            Some((chat, hash, open))
                if chat == request.chat.as_str() && hash == request.action_hash =>
            {
                Recorded::Same { open: open != 0 }
            }
            Some(_) => Recorded::Conflict,
        };
        transaction.commit().await.map_err(failed)?;
        Ok(recorded)
    }

    /// Closes `request`, whose receipt ended it.
    pub(crate) async fn end_request(&self, request: &RequestId) -> Result<(), Fault> {
        sqlx::query("UPDATE chat_requests SET open = 0 WHERE workspace_id = ? AND request_id = ?")
            .bind(self.id.as_str())
            .bind(request.as_str())
            .execute(&self.db.writer)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The requests to `chat` no receipt has ended yet, oldest first, with when each was sent.
    pub(crate) async fn open_requests(
        &self,
        chat: &ChatId,
    ) -> Result<Vec<(RequestId, i64)>, Fault> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT request_id, sent_at FROM chat_requests
             WHERE workspace_id = ? AND chat_id = ? AND open = 1 ORDER BY sent_at, rowid",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(|(id, sent_at)| Ok((stored(id)?, sent_at)))
        .collect()
    }

    async fn insert_request(
        &self,
        transaction: &mut SqliteConnection,
        request: &NewRequest<'_>,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO chat_requests
                 (workspace_id, request_id, chat_id, member_id, action_hash, sent_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(request.id.as_str())
        .bind(request.chat.as_str())
        .bind(request.member.as_str())
        .bind(request.action_hash)
        .bind(request.at)
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Stores `events` after the chat's last, in order, and gives the chat's seq after them.
    /// Those from its machine at or before the last one stored are replays and are skipped.
    /// Events that would take the workspace past [`CHAT_QUOTA`] make the chat `full` and are
    /// `store.full`, with none of them stored.
    pub(crate) async fn append_events(
        &self,
        chat: &ChatId,
        events: &[Incoming<'_>],
    ) -> Result<u64, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let (mut seq, mut core_seq, used) = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT c.seq, c.core_seq, w.chat_bytes FROM chats AS c
             JOIN workspaces AS w ON w.workspace_id = c.workspace_id
             WHERE c.workspace_id = ? AND c.chat_id = ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(chat_not_found)?;
        let mut rows = Vec::new();
        for incoming in events {
            let from_core = incoming.core_seq.map(number).transpose()?;
            if from_core.is_some_and(|at| at <= core_seq) {
                continue;
            }
            core_seq = from_core.unwrap_or(core_seq);
            seq = seq.checked_add(1).ok_or_else(unreadable)?;
            let body = serde_json::to_string(incoming.event).map_err(|_| unstorable())?;
            rows.push((seq, from_core, body, incoming.at));
        }
        let bytes = rows.iter().try_fold(0_i64, |sum, (.., body, _)| {
            i64::try_from(body.len()).map(|len| sum.saturating_add(len))
        });
        let bytes = bytes.map_err(|_| unstorable())?;
        if used.saturating_add(bytes) > CHAT_QUOTA {
            drop(transaction);
            self.set_chat_state(chat, ChatState::Full).await?;
            return Err(Fault::new(
                code!("store.full"),
                "The workspace has no room for more",
            ));
        }
        for (seq, from_core, body, at) in &rows {
            sqlx::query(
                "INSERT INTO chat_events (workspace_id, chat_id, seq, core_seq, body, at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .bind(seq)
            .bind(from_core)
            .bind(body)
            .bind(at)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        }
        self.advance(&mut transaction, chat, seq, core_seq, bytes)
            .await?;
        transaction.commit().await.map_err(failed)?;
        count(seq)
    }

    async fn advance(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        seq: i64,
        core_seq: i64,
        bytes: i64,
    ) -> Result<(), Fault> {
        sqlx::query(
            "UPDATE chats SET seq = ?, core_seq = ? WHERE workspace_id = ? AND chat_id = ?",
        )
        .bind(seq)
        .bind(core_seq)
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        sqlx::query("UPDATE workspaces SET chat_bytes = chat_bytes + ? WHERE workspace_id = ?")
            .bind(bytes)
            .bind(self.id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        Ok(())
    }

    /// The chat's events after `after`, oldest first: at most `limit`, and only as many as
    /// fit `max_bytes`, though always one when there is one.
    pub(crate) async fn events_after(
        &self,
        chat: &ChatId,
        after: u64,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<ChatEntry>, Fault> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT seq, body FROM chat_events WHERE workspace_id = ? AND chat_id = ? AND seq > ?
             ORDER BY seq LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(number(after)?)
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?;
        entries(rows, max_bytes)
    }

    /// The chat's latest events before `before`, oldest first, limited as in
    /// [`Self::events_after`].
    pub(crate) async fn events_before(
        &self,
        chat: &ChatId,
        before: u64,
        limit: u32,
        max_bytes: usize,
    ) -> Result<Vec<ChatEntry>, Fault> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT seq, body FROM chat_events WHERE workspace_id = ? AND chat_id = ? AND seq < ?
             ORDER BY seq DESC LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(i64::try_from(before).unwrap_or(i64::MAX))
        .bind(limit)
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?;
        let mut latest = entries(rows, max_bytes)?;
        latest.reverse();
        Ok(latest)
    }
}

// Takes rows in their order while they fit `max_bytes`, and the first one whatever its size.
fn entries(rows: Vec<(i64, String)>, max_bytes: usize) -> Result<Vec<ChatEntry>, Fault> {
    let mut used = 0_usize;
    let mut taken = Vec::new();
    for (seq, body) in rows {
        used = used.saturating_add(body.len());
        if used > max_bytes && !taken.is_empty() {
            break;
        }
        let event = serde_json::from_str(&body).map_err(|_| unreadable())?;
        taken.push(ChatEntry {
            seq: count(seq)?,
            event,
        });
    }
    Ok(taken)
}

pub(crate) fn chat_not_found() -> Fault {
    Fault::new(code!("chat.not_found"), "There is no such chat")
}

fn unstorable() -> Fault {
    Fault::new(code!("chat.unstorable"), "The chat's event can't be stored")
}

fn count(stored: i64) -> Result<u64, Fault> {
    u64::try_from(stored).map_err(|_| unreadable())
}

fn number(seq: u64) -> Result<i64, Fault> {
    i64::try_from(seq).map_err(|_| unstorable())
}

fn state_text(state: ChatState) -> Result<&'static str, Fault> {
    match state {
        ChatState::Open => Ok("open"),
        ChatState::Full => Ok("full"),
        ChatState::Detached => Ok("detached"),
        ChatState::Newer => Err(unstorable()),
    }
}

fn stored_state(text: &str) -> Result<ChatState, Fault> {
    match text {
        "open" => Ok(ChatState::Open),
        "full" => Ok(ChatState::Full),
        "detached" => Ok(ChatState::Detached),
        _ => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Outcome, Receipt, RunEvent, RunId, RunUpdate};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{member_id, two_teams};

    const NOW: i64 = 1_762_077_600;

    fn id<T: TryFrom<String>>(text: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(text.to_owned()).unwrap()
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    async fn make(workspace: &Workspace, chat: &str, owner: &str, at: i64) -> Result<(), Fault> {
        let request = id::<RequestId>(&format!("r-{chat}"));
        let (chat, owner) = (id::<ChatId>(chat), member_id(owner));
        let machine = id::<MachineName>("Ada's laptop");
        let repo = id::<RepoId>("site");
        let new = NewChat {
            id: &chat,
            owner: &owner,
            title: "Fix the build",
            repo: Some(&repo),
            machine: &machine,
            at,
        };
        let first = NewRequest {
            id: &request,
            chat: &chat,
            member: &owner,
            action_hash: b"h0",
            at,
        };
        workspace.add_chat(&new, &first).await
    }

    fn said(text: &str) -> ChatEvent {
        ChatEvent::Run {
            update: RunUpdate {
                run: id::<RunId>("run-1"),
                event: RunEvent::Text {
                    text: text.to_owned(),
                },
            },
        }
    }

    async fn append(
        workspace: &Workspace,
        chat: &str,
        events: &[(Option<u64>, ChatEvent)],
    ) -> Result<u64, Fault> {
        let incoming: Vec<_> = events
            .iter()
            .map(|(core_seq, event)| Incoming {
                core_seq: *core_seq,
                event,
                at: NOW,
            })
            .collect();
        workspace.append_events(&id(chat), &incoming).await
    }

    fn seqs(entries: &[ChatEntry]) -> Vec<u64> {
        entries.iter().map(|entry| entry.seq).collect()
    }

    #[tokio::test]
    async fn a_chat_is_listed_in_its_own_workspace_only_and_its_id_is_its_workspaces() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m3", NOW + 5).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();

        let listed = one.chats().await.unwrap();
        let summary = ChatSummary {
            id: id("c-2"),
            title: "Fix the build".to_owned(),
            owner: member_id("m3"),
            machine: id("Ada's laptop"),
            state: ChatState::Open,
            updated_at: NOW + 5,
        };
        assert_eq!(listed.first(), Some(&summary));
        assert_eq!(seqs_of_ids(&listed), ["c-2", "c-1"]);
        assert_eq!(seqs_of_ids(&two.chats().await.unwrap()), ["c-1"]);
        let again = make(&one, "c-1", "m1", NOW).await.unwrap_err();
        assert_eq!(code(&again), "db.exists");
        let stranger = make(&one, "c-3", "m2", NOW).await.unwrap_err();
        assert_eq!(code(&stranger), "db.failed");
        let chat = Chat {
            owner: member_id("m1"),
            state: ChatState::Open,
            seq: 0,
            core_seq: 0,
        };
        assert_eq!(one.chat(&id("c-1")).await.unwrap(), Some(chat));
        assert_eq!(two.chat(&id("c-2")).await.unwrap(), None);
        one.set_chat_state(&id("c-1"), ChatState::Detached)
            .await
            .unwrap();
        let state = one.chat(&id("c-1")).await.unwrap().map(|chat| chat.state);
        assert_eq!(state, Some(ChatState::Detached));
        assert_eq!(
            two.chat(&id("c-1")).await.unwrap().unwrap().state,
            ChatState::Open
        );
        let newer = one.set_chat_state(&id("c-1"), ChatState::Newer).await;
        assert_eq!(code(&newer.unwrap_err()), "chat.unstorable");
    }

    fn seqs_of_ids(list: &[ChatSummary]) -> Vec<&str> {
        list.iter().map(|chat| chat.id.as_str()).collect()
    }

    #[tokio::test]
    async fn the_ledger_tells_a_retry_from_another_command_and_keeps_what_is_open() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&one, "c-2", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let (chat, other, member) = (id::<ChatId>("c-1"), id::<ChatId>("c-2"), member_id("m1"));
        let record =
            async |workspace: &Workspace, request: &str, chat: &ChatId, hash: &[u8], at| {
                let (request, member) = (id::<RequestId>(request), member.clone());
                let new = NewRequest {
                    id: &request,
                    chat,
                    member: &member,
                    action_hash: hash,
                    at,
                };
                workspace.record_request(&new).await.unwrap()
            };

        assert_eq!(
            record(&one, "r-1", &chat, b"h1", NOW + 2).await,
            Recorded::New
        );
        assert_eq!(
            record(&one, "r-2", &chat, b"h2", NOW + 1).await,
            Recorded::New
        );
        let same = Recorded::Same { open: true };
        assert_eq!(record(&one, "r-1", &chat, b"h1", NOW + 9).await, same);
        assert_eq!(
            record(&one, "r-1", &chat, b"hx", NOW).await,
            Recorded::Conflict
        );
        assert_eq!(
            record(&one, "r-1", &other, b"h1", NOW).await,
            Recorded::Conflict
        );
        let opened = |list: Vec<(RequestId, i64)>| {
            list.into_iter()
                .map(|(request, at)| (request.as_str().to_owned(), at))
                .collect::<Vec<_>>()
        };
        let open = opened(one.open_requests(&chat).await.unwrap());
        let expected = [("r-c-1", NOW), ("r-2", NOW + 1), ("r-1", NOW + 2)];
        assert_eq!(open, expected.map(|(request, at)| (request.to_owned(), at)));

        one.end_request(&id("r-1")).await.unwrap();
        assert_eq!(
            record(&one, "r-1", &chat, b"h1", NOW).await,
            Recorded::Same { open: false }
        );
        let open = opened(one.open_requests(&chat).await.unwrap());
        assert_eq!(open.len(), 2);
        assert_eq!(opened(two.open_requests(&chat).await.unwrap()).len(), 1);
        let member = member_id("m2");
        let elsewhere = NewRequest {
            id: &id("r-1"),
            chat: &chat,
            member: &member,
            action_hash: b"h1",
            at: NOW,
        };
        assert_eq!(two.record_request(&elsewhere).await.unwrap(), Recorded::New);
    }

    #[tokio::test]
    async fn events_are_numbered_by_the_server_and_a_replay_from_the_machine_is_skipped() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let unknown = ChatEvent::Receipt {
            receipt: Receipt {
                request: id("r-c-1"),
                outcome: Outcome::Unknown,
            },
        };

        let first = [(Some(2), said("a")), (Some(5), said("b")), (None, unknown)];
        assert_eq!(append(&one, "c-1", &first).await.unwrap(), 3);
        let replayed = [
            (Some(5), said("b")),
            (Some(4), said("x")),
            (Some(6), said("c")),
        ];
        assert_eq!(append(&one, "c-1", &replayed).await.unwrap(), 4);
        assert_eq!(append(&one, "c-1", &[]).await.unwrap(), 4);

        let chat = one.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!((chat.seq, chat.core_seq), (4, 6));
        let all = one
            .events_after(&id("c-1"), 0, 100, usize::MAX)
            .await
            .unwrap();
        assert_eq!(seqs(&all), [1, 2, 3, 4]);
        assert_eq!(all.get(3).map(|entry| &entry.event), Some(&said("c")));
        assert_eq!(
            seqs(
                &one.events_after(&id("c-1"), 2, 1, usize::MAX)
                    .await
                    .unwrap()
            ),
            [3]
        );
        let latest = one.events_before(&id("c-1"), u64::MAX, 2, usize::MAX).await;
        assert_eq!(seqs(&latest.unwrap()), [3, 4]);
        assert_eq!(
            seqs(
                &one.events_before(&id("c-1"), 3, 9, usize::MAX)
                    .await
                    .unwrap()
            ),
            [1, 2]
        );
        assert!(
            two.events_after(&id("c-1"), 0, 100, usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        let missing = append(&two, "c-2", &first).await.unwrap_err();
        assert_eq!(code(&missing), "chat.not_found");
    }

    #[tokio::test]
    async fn a_window_takes_only_what_fits_but_always_one_event() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let events = [
            (None, said("aaaa")),
            (None, said("bbbb")),
            (None, said("cccc")),
        ];
        append(&one, "c-1", &events).await.unwrap();
        let size = serde_json::to_string(&said("aaaa")).unwrap().len();

        let fits_two = one.events_after(&id("c-1"), 0, 9, 2 * size).await.unwrap();
        assert_eq!(seqs(&fits_two), [1, 2]);
        let fits_none = one.events_after(&id("c-1"), 0, 9, size - 1).await.unwrap();
        assert_eq!(seqs(&fits_none), [1]);
        let latest = one.events_before(&id("c-1"), 9, 9, 2 * size).await.unwrap();
        assert_eq!(seqs(&latest), [2, 3]);
    }

    #[tokio::test]
    async fn events_past_the_quota_fill_the_chat_and_none_of_them_is_stored() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let size = i64::try_from(serde_json::to_string(&said("a")).unwrap().len()).unwrap();
        append(&one, "c-1", &[(Some(1), said("a"))]).await.unwrap();
        let used: i64 =
            sqlx::query_scalar("SELECT chat_bytes FROM workspaces WHERE workspace_id = 'w1'")
                .fetch_one(&one.db.readers)
                .await
                .unwrap();
        assert_eq!(used, size);
        sqlx::query("UPDATE workspaces SET chat_bytes = ? WHERE workspace_id = 'w1'")
            .bind(CHAT_QUOTA - size)
            .execute(&one.db.writer)
            .await
            .unwrap();

        assert_eq!(
            append(&one, "c-1", &[(Some(2), said("a"))]).await.unwrap(),
            2
        );
        let over = append(&one, "c-1", &[(Some(3), said("a"))])
            .await
            .unwrap_err();
        assert_eq!(code(&over), "store.full");
        let chat = one.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(
            (chat.state, chat.seq, chat.core_seq),
            (ChatState::Full, 2, 2)
        );
        assert_eq!(
            append(&two, "c-1", &[(Some(1), said("a"))]).await.unwrap(),
            1
        );
    }
}
