//! What a machine sends of the chats it runs: their events, stored in the order they come.

use serde_json::Value;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentOwner, AgentRef, ChatEntry, ChatEvent, ChatId, ChatUpdate, Code, MachineId,
    PermissionAnswer, RunEvent, RunStatus, RunUpdate, Wants, WorkspaceId, code,
};

use crate::db::{Incoming, Leg, turns};
use crate::shared::Shared;
use crate::socket::Closed;

/// What the server offers to cores whose chats' events it stores.
pub(crate) const UPLOAD: Code = code!("chat.upload");

/// Stores what is [`kept`] of `update` from `machine`, named as its leg's member's, and tells
/// the sockets that follow its chat. An event of a chat `machine` holds no leg of, or whose
/// member no longer owns it, is dropped. When the database fails, the link closes, so nothing
/// later is stored before it and the core, connecting again, sends it once more.
pub(crate) async fn store(
    shared: &Shared,
    machine: &MachineId,
    update: ChatUpdate,
) -> Result<(), Closed> {
    let ChatUpdate {
        workspace,
        chat,
        entry,
    } = update;
    let fields = [
        ("machine_id", machine.as_str().into()),
        ("workspace_id", workspace.as_str().into()),
        ("chat_id", chat.as_str().into()),
    ];
    match append(shared, machine, (&workspace, &chat), entry).await {
        Ok(Stored::Kept { turned }) => {
            // Someone writing, or a turn's end, changes the chat's summary in the list.
            if turned {
                shared.wake_feeds(&workspace);
            }
            shared.chats.stored(workspace, chat);
        }
        Ok(Stored::Dropped) => {}
        Ok(Stored::Refused) => shared.log.warn("chat upload refused", &fields),
        Err(fault) => {
            // A chat a limit just filled is news to the list.
            let full = [code!("chat.quota_full"), code!("chat.too_many_waiting")];
            if full.contains(fault.code()) {
                shared.wake_feeds(&workspace);
            }
            let fields = [&fields[..], &[("err", Field::error(&fault))]].concat();
            shared.log.warn("chat upload failed", &fields);
            if *fault.code() == code!("db.failed") {
                return Err(Closed);
            }
        }
    }
    Ok(())
}

/// What became of an event a machine sent.
enum Stored {
    /// Kept, and whether it [`turns`] its chat.
    Kept { turned: bool },
    /// Not one the server keeps.
    Dropped,
    /// Not the machine's to send.
    Refused,
}

/// What the server keeps of a chat event from `leg`'s machine: its messages, never a tool's
/// name or input, with who wrote a prompt and whose agent runs, and where, as the leg says.
fn kept(event: ChatEvent, leg: &Leg) -> Option<ChatEvent> {
    match event {
        ChatEvent::Run { update } => {
            let update = kept_run(update, leg)?;
            Some(ChatEvent::Run { update })
        }
        ChatEvent::Prompt {
            request,
            text,
            pick,
            repo,
            agent,
            task,
            ..
        } => Some(ChatEvent::Prompt {
            request,
            text,
            pick,
            author: Some(leg.member.clone()),
            repo,
            agent,
            task,
        }),
        ChatEvent::RunStatus { status } => Some(ChatEvent::RunStatus {
            status: RunStatus {
                run: leg.chat_run(&status.run)?,
                agent: Some(AgentRef {
                    kind: leg.agent,
                    owner: AgentOwner::Member {
                        member: leg.member.clone(),
                    },
                    machine: Some(leg.machine_name.clone()),
                }),
                ..status
            },
        }),
        ChatEvent::OutsidePrompt { .. } | ChatEvent::Receipt { .. } => Some(event),
        // Only the server writes a person's message, and what a newer core sends may hold
        // anything.
        ChatEvent::Message { .. } | ChatEvent::Newer => None,
    }
}

// The most bytes of a prompt's tool's name that are kept.
const TOOL_MAX: usize = 128;

// What is kept of a run's update, its run under the name the chat knows it by.
fn kept_run(RunUpdate { run, event }: RunUpdate, leg: &Leg) -> Option<RunUpdate> {
    let event = match event {
        RunEvent::Text { .. } | RunEvent::Finished { .. } => event,
        // Kept with what it says the tool wants, never the tool's input.
        RunEvent::PermissionAsked {
            run,
            prompt,
            mut tool,
            wants,
            ..
        } => RunEvent::PermissionAsked {
            run: leg.chat_run(&run)?,
            prompt,
            tool: {
                tool.truncate(tool.floor_char_boundary(TOOL_MAX));
                tool
            },
            input: Value::Null,
            wants: wants.map(Wants::bounded),
        },
        RunEvent::PermissionWithdrawn { run, prompt } => RunEvent::PermissionWithdrawn {
            run: leg.chat_run(&run)?,
            prompt,
        },
        RunEvent::PermissionAnswered { answer } => RunEvent::PermissionAnswered {
            answer: PermissionAnswer {
                run: leg.chat_run(&answer.run)?,
                ..answer
            },
        },
        RunEvent::Started { .. }
        | RunEvent::TextDelta { .. }
        | RunEvent::ToolCall { .. }
        | RunEvent::ToolDone { .. }
        | RunEvent::Newer => return None,
    };
    let run = leg.chat_run(&run)?;
    Some(RunUpdate { run, event })
}

async fn append(
    shared: &Shared,
    machine: &MachineId,
    (workspace, chat): (&WorkspaceId, &ChatId),
    entry: ChatEntry,
) -> Result<Stored, Fault> {
    let Some(place) = shared.db.find_workspace(workspace).await? else {
        return Ok(Stored::Refused);
    };
    let Some(leg) = place.leg_on(chat, machine).await? else {
        return Ok(Stored::Refused);
    };
    let Some(event) = kept(entry.event, &leg) else {
        return Ok(Stored::Dropped);
    };
    let incoming = Incoming {
        core_seq: Some(entry.seq),
        event: &event,
        at: shared.now(),
    };
    let from = (&leg.member, machine);
    match place.append_from(from, chat, &[incoming]).await {
        Err(fault) if *fault.code() == code!("chat.not_found") => Ok(Stored::Refused),
        appended => appended.map(|_| Stored::Kept {
            turned: turns(&event),
        }),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatEntry, ChatEvent, Outcome, Receipt, WANTS_MAX, WorkspaceId};

    use super::*;
    use crate::chats::test_support::{Ada, ada, core_named};
    use crate::db::test_support::{NOW, append, asks, core_seq, id, make, said};
    use crate::test_support::{Fixture, member_id};

    fn update(workspace: &str, chat: &str, seq: u64, event: ChatEvent) -> ChatUpdate {
        ChatUpdate {
            workspace: id(workspace),
            chat: id(chat),
            entry: ChatEntry { seq, event },
        }
    }

    // Ada, with chat c-1 in w1 on her machine.
    async fn ada_chatting(fixture: &Fixture) -> Ada {
        let ada = ada(fixture).await;
        make(&ada.member.workspace, "c-1", "m1", NOW).await.unwrap();
        ada.member.workspace.move_chats_to(&ada.machine).await;
        ada
    }

    #[tokio::test]
    async fn only_the_chats_own_machine_adds_to_it_and_its_followers_hear_of_it() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let (workspace, ours) = (&ada.member.workspace, &ada.machine);
        let mut notices = shared.chats.notices();
        let other = id::<MachineId>("mc-2");

        store(shared, ours, update("w1", "c-1", 4, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, &other, update("w1", "c-1", 5, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, ours, update("w1", "c-9", 6, said("MARKER-TEXT")))
            .await
            .unwrap();
        store(shared, ours, update("w-9", "c-1", 7, said("MARKER-TEXT")))
            .await
            .unwrap();
        workspace.leave(&member_id("m1")).await.unwrap();
        store(shared, ours, update("w1", "c-1", 8, said("MARKER-TEXT")))
            .await
            .unwrap();

        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        assert_eq!(events.unwrap().len(), 1);
        assert_eq!(core_seq(workspace, "c-1").await, 4);
        assert_eq!(notices.try_recv().unwrap(), (id("w1"), id("c-1")));
        notices.try_recv().unwrap_err();
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert_eq!(logged.matches("chat upload refused").count(), 4);
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_turn_ending_wakes_the_workspaces_feeds_and_text_does_not() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let mut feeds = shared.changes.subscribe();

        store(
            shared,
            &ada.machine,
            update("w1", "c-1", 1, said("working")),
        )
        .await
        .unwrap();
        feeds.try_recv().unwrap_err();
        let receipt = Receipt {
            request: id("r-c-1"),
            outcome: Outcome::Completed,
        };
        let ended = update("w1", "c-1", 2, ChatEvent::Receipt { receipt });
        store(shared, &ada.machine, ended).await.unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }

    #[tokio::test]
    async fn a_chat_the_quota_fills_wakes_the_workspaces_feeds() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        ada.member.workspace.fill_quota().await;
        let mut feeds = shared.changes.subscribe();

        store(shared, &ada.machine, update("w1", "c-1", 1, said("a")))
            .await
            .unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        let logged = crate::test_support::messages(&fixture.records);
        assert!(logged.contains(&"chat upload failed"), "{logged:?}");
    }

    #[tokio::test]
    async fn a_failed_store_closes_the_link_and_keeps_the_chats_place() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let workspace = &ada.member.workspace;
        store(shared, &ada.machine, update("w1", "c-1", 1, said("a")))
            .await
            .unwrap();
        workspace.break_chat_events().await;

        let failed = store(shared, &ada.machine, update("w1", "c-1", 2, said("b"))).await;

        failed.unwrap_err();
        assert_eq!(core_seq(workspace, "c-1").await, 1);
        // Refused for good, not failed: the link goes on.
        workspace.fill_quota().await;
        store(shared, &ada.machine, update("w1", "c-1", 3, said("c")))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_unpaired_machine_stores_nothing_in_the_chats_it_ran() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let (workspace, chat) = (&ada.member.workspace, id("c-1"));
        let event = said("a");
        let incoming = || Incoming {
            core_seq: Some(1),
            event: &event,
            at: NOW,
        };
        let owner = member_id("m1");
        let elsewhere = workspace
            .append_from((&owner, &id("mc-2")), &chat, &[incoming()])
            .await;
        assert_eq!(elsewhere.unwrap_err().code(), &code!("chat.not_found"));
        let unpaired = shared.db.unpair(&ada.asker.account, &ada.machine).await;
        assert!(unpaired.unwrap());

        let stored = workspace
            .append_from((&owner, &ada.machine), &chat, &[incoming()])
            .await;

        assert_eq!(stored.unwrap_err().code(), &code!("chat.not_found"));
        let events = workspace.events_after(&chat, 0, 10, usize::MAX).await;
        assert!(events.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_chat_too_many_prompts_fill_wakes_the_workspaces_feeds() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let waiting: Vec<_> = (1..=1000).map(|prompt| (None, asks(prompt))).collect();
        append(&ada.member.workspace, "c-1", &waiting)
            .await
            .unwrap();
        let mut feeds = shared.changes.subscribe();

        let one_more = update("w1", "c-1", 1, asks(1001));
        store(shared, &ada.machine, one_more).await.unwrap();

        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
    }

    #[tokio::test]
    async fn messages_are_stored_and_a_prompts_tool_with_what_it_wants_but_no_tools_input() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let workspace = &ada.member.workspace;
        let run = |event| ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event,
            },
        };
        let marked = || serde_json::json!({ "path": "MARKER-INPUT" });
        let call = RunEvent::ToolCall {
            call: "call-1".to_owned(),
            tool: "MARKER-TOOL".to_owned(),
            input: marked(),
        };
        let long = format!("MARKER{}", "é".repeat(WANTS_MAX));
        let asks = |tool: &str, input, text: &str| RunEvent::PermissionAsked {
            run: id("run-1"),
            prompt: 1,
            tool: tool.to_owned(),
            input,
            wants: Some(Wants {
                text: text.to_owned(),
                cut: false,
            }),
        };
        let asked = asks(&"é".repeat(TOOL_MAX), marked(), &long);

        for (seq, event) in [(1, run(call)), (2, run(asked)), (3, said("MARKER-TEXT"))] {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
        }

        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        let stored: Vec<_> = events
            .unwrap()
            .into_iter()
            .map(|entry| entry.event)
            .collect();
        // A core that sent more than a prompt may say has it cut here.
        let fits = format!("MARKER{}", "é".repeat(WANTS_MAX / 2 - 3));
        let mut kept = asks(&"é".repeat(TOOL_MAX / 2), Value::Null, &fits);
        if let RunEvent::PermissionAsked { wants, .. } = &mut kept {
            *wants = wants.take().map(|wants| Wants { cut: true, ..wants });
        }
        let stored = core_named(&stored);
        assert_eq!(stored, [run(kept), said("MARKER-TEXT")]);
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("chat upload refused"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[test]
    fn what_a_newer_core_sends_or_a_tool_does_is_not_kept() {
        let run = |event| ChatEvent::Run {
            update: RunUpdate {
                run: id("run-1"),
                event,
            },
        };
        let done = RunEvent::ToolDone {
            call: "call-1".to_owned(),
            failed: false,
        };
        let finished = RunEvent::Finished {
            outcome: Outcome::Completed,
        };
        let leg = leg();
        assert_eq!(kept(ChatEvent::Newer, &leg), None);
        assert_eq!(kept(run(RunEvent::Newer), &leg), None);
        assert_eq!(kept(run(done), &leg), None);
        let ended = kept(run(finished.clone()), &leg).map(|kept| core_named(&kept));
        assert_eq!(ended, Some(run(finished)));
        let outside = ChatEvent::OutsidePrompt {
            text: "Go".to_owned(),
        };
        assert_eq!(kept(outside.clone(), &leg), Some(outside));
    }

    #[test]
    fn a_legs_runs_get_a_name_of_their_own_in_the_chat_which_only_that_leg_turns_back() {
        let other = Leg { number: 2, ..leg() };
        let (leg, run) = (leg(), id::<weacon_protocol::RunId>("run-1"));
        let named = id::<weacon_protocol::RunId>("run-1-l1");
        let answer = |run: &weacon_protocol::RunId| PermissionAnswer {
            run: run.clone(),
            prompt: 1,
            allow: true,
        };
        let events = |run: &weacon_protocol::RunId| {
            let (asked, prompt) = (run.clone(), 1);
            [
                RunEvent::PermissionAsked {
                    run: asked.clone(),
                    prompt,
                    tool: String::new(),
                    input: Value::Null,
                    wants: None,
                },
                RunEvent::PermissionWithdrawn { run: asked, prompt },
                RunEvent::PermissionAnswered {
                    answer: answer(run),
                },
            ]
        };
        let updates = |run: &weacon_protocol::RunId| {
            events(run).map(|event| RunUpdate {
                run: run.clone(),
                event,
            })
        };

        let kept = updates(&run).map(|update| kept_run(update, &leg));

        assert_eq!(kept, updates(&named).map(Some));
        assert_eq!(leg.chat_run(&run), Some(named.clone()));
        assert_eq!(leg.own_run(&named), Some(run));
        assert_eq!(other.own_run(&named), None);
        assert_eq!(leg.own_run(&id("run-1")), None);
        assert_eq!(leg.chat_run(&id(&"r".repeat(64))), None);
    }

    fn leg() -> Leg {
        Leg {
            member: member_id("m5"),
            machine: id("mc-5"),
            machine_name: id("Studio"),
            agent: weacon_protocol::AgentKind::Codex,
            repo: None,
            core_seq: 0,
            detached: false,
            quoted_seq: 0,
            number: 1,
        }
    }

    #[test]
    fn a_prompt_and_a_runs_status_are_named_by_the_leg_whatever_the_machine_says_and_no_machine_writes_a_message()
     {
        let leg = leg();
        let forged = |member: &str| AgentRef {
            kind: weacon_protocol::AgentKind::Claude,
            owner: AgentOwner::Member {
                member: member_id(member),
            },
            machine: Some(id("Not mine")),
        };
        let prompt = |author: Option<&str>| ChatEvent::Prompt {
            pick: None,
            request: id("r-1"),
            text: "Go".to_owned(),
            author: author.map(member_id),
            repo: None,
            agent: None,
            task: None,
        };
        let status = |agent| ChatEvent::RunStatus {
            status: RunStatus {
                run: id("run-1"),
                state: weacon_protocol::RunState::Running,
                agent,
            },
        };
        let named = AgentRef {
            kind: weacon_protocol::AgentKind::Codex,
            owner: AgentOwner::Member {
                member: member_id("m5"),
            },
            machine: Some(id("Studio")),
        };

        for author in [None, Some("m1")] {
            assert_eq!(kept(prompt(author), &leg), Some(prompt(Some("m5"))));
        }
        for agent in [None, Some(forged("m1"))] {
            let stored = kept(status(agent), &leg).map(|kept| core_named(&kept));
            assert_eq!(stored, Some(status(Some(named.clone()))));
        }
        let message = ChatEvent::Message {
            author: member_id("m1"),
            text: "Hi".to_owned(),
        };
        assert_eq!(kept(message, &leg), None);
    }
}
