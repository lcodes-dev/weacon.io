//! What a member reads of a chat: a snapshot, and windows of older events.

use weacon_observability::Fault;
use weacon_protocol::{ChatAnswerKind, ChatId, ChatQueryKind, MAX_FRAME, code};

use crate::db::{Workspace, chat_not_found};

/// The events a snapshot carries, when they fit.
const SNAPSHOT_EVENTS: u32 = 50;
// A snapshot's or window's events leave room in their frame for what travels with them.
const EVENTS_MAX: usize = MAX_FRAME - (16 << 10);

/// Answers a snapshot or a window of `chat` in `workspace`. A subscription isn't answered here.
pub(crate) async fn answer(
    workspace: &Workspace,
    chat: &ChatId,
    query: &ChatQueryKind,
) -> Result<ChatAnswerKind, Fault> {
    let found = workspace.chat(chat).await?.ok_or_else(chat_not_found)?;
    match *query {
        ChatQueryKind::Snapshot => {
            let runs = workspace.chat_runs(chat).await?;
            let events = workspace
                .events_before(chat, u64::MAX, SNAPSHOT_EVENTS, EVENTS_MAX)
                .await?;
            Ok(ChatAnswerKind::Snapshot {
                seq: found.seq,
                waiting: runs.waiting_seqs(),
                runs: runs.runs,
                events,
            })
        }
        ChatQueryKind::Window { before, limit } => Ok(ChatAnswerKind::Window {
            events: workspace
                .events_before(chat, before, limit, EVENTS_MAX)
                .await?,
        }),
        ChatQueryKind::Subscribe { .. } | ChatQueryKind::Newer => Err(Fault::new(
            code!("message.unsupported"),
            "This build doesn't know that message",
        )),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatEvent, RunEvent, RunId, RunState, RunStatus, RunUpdate};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{NOW, append, id, make, said};
    use crate::test_support::two_teams;

    fn running() -> ChatEvent {
        let status = RunStatus {
            run: id::<RunId>("run-1"),
            state: RunState::Waiting,
            agent: None,
        };
        ChatEvent::RunStatus { status }
    }

    fn asks() -> ChatEvent {
        ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event: RunEvent::PermissionAsked {
                    run: id("run-1"),
                    prompt: 1,
                    tool: "Write".to_owned(),
                    input: serde_json::Value::Null,
                },
            },
        }
    }

    #[tokio::test]
    async fn a_snapshot_tells_the_runs_the_waiting_prompts_and_the_latest_events() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        let mut events = vec![(Some(1), running()), (Some(2), asks())];
        events.extend((3..60).map(|seq| (Some(seq), said("more"))));
        append(&one, "c-1", &events).await.unwrap();

        let snapshot = answer(&one, &id("c-1"), &ChatQueryKind::Snapshot).await;
        let ChatAnswerKind::Snapshot {
            seq,
            runs,
            waiting,
            events,
        } = snapshot.unwrap()
        else {
            panic!("not a snapshot");
        };
        assert_eq!((seq, waiting), (59, vec![2]));
        assert_eq!(runs.first().map(|run| run.state), Some(RunState::Waiting));
        let seqs: Vec<_> = events.iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs, (10..60).collect::<Vec<_>>());

        let window = ChatQueryKind::Window {
            before: 10,
            limit: 3,
        };
        let ChatAnswerKind::Window { events } = answer(&one, &id("c-1"), &window).await.unwrap()
        else {
            panic!("not a window");
        };
        assert_eq!(
            events.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [7, 8, 9]
        );
        let elsewhere = answer(&two, &id("c-1"), &ChatQueryKind::Snapshot).await;
        assert_eq!(elsewhere.unwrap_err().code().as_str(), "chat.not_found");
        let subscribe = ChatQueryKind::Subscribe { after: 0 };
        let refused = answer(&one, &id("c-1"), &subscribe).await.unwrap_err();
        assert_eq!(refused.code().as_str(), "message.unsupported");
    }
}
