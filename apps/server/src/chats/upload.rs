//! What a machine sends of the chats it runs: their events, stored in the order they come.

use serde_json::Value;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentOwner, AgentRef, ChatEntry, ChatEvent, ChatId, ChatUpdate, Code, MachineId, MemberId,
    PermissionAnswer, RequestId, RunEvent, RunStatus, RunUpdate, Wants, WorkspaceId, code,
};

use crate::db::{Incoming, Leg, Workspace, turns};
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
            // A chat its waiting prompts just filled is news to the list.
            if *fault.code() == code!("chat.too_many_waiting") {
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
/// name or input, with a prompt as written `by` the member the server took it from, and whose
/// agent runs, and where, as the leg says.
fn kept(event: ChatEvent, leg: &Leg, by: &MemberId) -> Option<ChatEvent> {
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
            author: Some(by.clone()),
            repo,
            agent,
            task,
        }),
        ChatEvent::RunStatus { status } => Some(ChatEvent::RunStatus {
            status: RunStatus {
                run: leg.chat_run(&status.run)?,
                agent: Some(AgentRef {
                    kind: leg.agent,
                    owner: owner_of(leg),
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

// Whose agent `leg`'s is: its member's own, or the shared one of a machine shared with the
// workspace, whichever of its members it works for.
fn owner_of(leg: &Leg) -> AgentOwner {
    let member = leg.member.clone();
    match leg.shared_by.clone() {
        Some(owner) => AgentOwner::Shared { member, owner },
        None => AgentOwner::Member { member },
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
        | RunEvent::ToolDone { .. } => return None,
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
    loop {
        let Some(leg) = place.leg_on(chat, machine).await? else {
            return Ok(Stored::Refused);
        };
        let (at, sent) = (shared.now(), (chat, &leg));
        let stored = if let ChatEvent::Prompt { request, .. } = &entry.event {
            append_prompt(&place, sent, (request, &entry), at).await
        } else {
            append_other(&place, sent, &entry, at).await
        };
        return match stored {
            // Called or taken back since it was read: named again.
            Err(fault) if *fault.code() == code!("chat.leg_called") => continue,
            Err(fault) if *fault.code() == code!("chat.not_found") => Ok(Stored::Refused),
            Err(fault) => Err(fault),
            Ok(None) => Ok(Stored::Dropped),
            Ok(Some(event)) => Ok(Stored::Kept {
                turned: turns(&event),
            }),
        };
    }
}

// Stores the prompt `entry`, sent under `request` by `leg`'s machine: one the server took is
// its asker's, in the words it took and only once; any other was written on the machine
// itself, by its owner.
async fn append_prompt(
    place: &Workspace,
    (chat, leg): (&ChatId, &Leg),
    (request, entry): (&RequestId, &ChatEntry),
    at: i64,
) -> Result<Option<ChatEvent>, Fault> {
    let asked = place.asker(chat, &leg.machine, request).await?;
    let taken = asked.is_some().then_some(request);
    let (by, words) = match asked {
        Some((member, words)) => (member, Some(words)),
        None => (
            leg.shared_by.clone().unwrap_or_else(|| leg.member.clone()),
            None,
        ),
    };
    let Some(mut event) = kept(entry.event.clone(), leg, &by) else {
        return Ok(None);
    };
    if let (ChatEvent::Prompt { text, .. }, Some(words)) = (&mut event, words) {
        *text = words;
    }
    let incoming = Incoming {
        core_seq: Some(entry.seq),
        event: &event,
        at,
    };
    let from = (&leg.machine, &by);
    place.append_prompt(from, taken, chat, incoming).await?;
    Ok(Some(event))
}

// Stores what else `leg`'s machine sent, as its agent's for the member it works for.
async fn append_other(
    place: &Workspace,
    (chat, leg): (&ChatId, &Leg),
    entry: &ChatEntry,
    at: i64,
) -> Result<Option<ChatEvent>, Fault> {
    let Some(event) = kept(entry.event.clone(), leg, &leg.member) else {
        return Ok(None);
    };
    let incoming = Incoming {
        core_seq: Some(entry.seq),
        event: &event,
        at,
    };
    let from = (&leg.machine, &leg.member);
    place.append_from(from, chat, &[incoming]).await?;
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{
        ChatEntry, ChatEvent, Outcome, Receipt, RunState, WANTS_MAX, WorkspaceId,
    };

    static R1: std::sync::LazyLock<RequestId> = std::sync::LazyLock::new(|| id("r-1"));

    use super::*;
    use crate::chats::test_support::{Ada, ada, core_named, grace};
    use crate::db::test_support::{
        NOW, WORDS, append, ask, asks, core_seq, id, make, said, status,
    };
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
        workspace.fill_chat("c-1").await;
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
            .append_from((&id("mc-2"), &owner), &chat, &[incoming()])
            .await;
        assert_eq!(elsewhere.unwrap_err().code(), &code!("chat.not_found"));
        let unpaired = shared.db.unpair(&ada.asker.account, &ada.machine).await;
        assert!(unpaired.unwrap());

        let stored = workspace
            .append_from((&ada.machine, &owner), &chat, &[incoming()])
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

    // Ada's shared machine and chat c-1 there, the other member, and that member's call of
    // its agent under r-1 at `NOW`.
    async fn called_by_another(fixture: &Fixture) -> (Ada, MemberId) {
        let ada = ada_chatting(fixture).await;
        let bo = grace(fixture).await.member.id;
        let (db, chat, leg) = (&fixture.shared.db, id("c-1"), on(&ada.machine));
        db.set_shares(&ada.machine, &[id("w1")]).await.unwrap();
        let called = ada
            .member
            .workspace
            .call_leg(&chat, ask(&bo, &R1), &leg, NOW);
        assert!(called.await.unwrap().is_some());
        (ada, bo)
    }

    static LAPTOP: std::sync::LazyLock<weacon_protocol::MachineName> =
        std::sync::LazyLock::new(|| id("Ada's laptop"));

    fn on(machine: &MachineId) -> crate::db::NewLeg<'_> {
        crate::db::NewLeg {
            machine_id: machine,
            machine: &LAPTOP,
            agent: weacon_protocol::AgentKind::Claude,
            repo: None,
        }
    }

    // A prompt as a core sends it up, saying Ada wrote it.
    fn sent_up(request: &str, text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: id(request),
            text: text.to_owned(),
            author: Some(member_id("m1")),
            pick: None,
            repo: None,
            agent: None,
            task: None,
        }
    }

    // Each prompt stored in c-1, with who wrote it, and whose agent each run status says ran.
    async fn written(ada: &Ada) -> Vec<(Option<MemberId>, String)> {
        let workspace = &ada.member.workspace;
        let events = workspace.events_after(&id("c-1"), 0, 10, usize::MAX).await;
        let told = |entry: ChatEntry| match entry.event {
            ChatEvent::Prompt { author, text, .. } => Some((author, text)),
            ChatEvent::RunStatus { status } => {
                let member = status.agent.map(|agent| match agent.owner {
                    AgentOwner::Shared { member, .. } | AgentOwner::Member { member } => member,
                });
                Some((member, "status".to_owned()))
            }
            ChatEvent::Message { .. }
            | ChatEvent::Receipt { .. }
            | ChatEvent::Run { .. }
            | ChatEvent::OutsidePrompt { .. }
            | ChatEvent::Newer => None,
        };
        events.unwrap().into_iter().filter_map(told).collect()
    }

    #[tokio::test]
    async fn what_a_shared_machine_sends_is_its_prompts_askers_whatever_its_core_says() {
        let fixture = Fixture::new().await;
        let (shared, (ada, bo)) = (&fixture.shared, called_by_another(&fixture).await);
        let events = [sent_up("r-1", "FORGED"), status(RunState::Running)];

        for (seq, event) in (1..).zip(events) {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
        }

        // His prompt in the words the server took from him, and his run.
        let his = |text: &str| (Some(bo.clone()), text.to_owned());
        assert_eq!(written(&ada).await, [his(WORDS), his("status")]);
        let workspace = &ada.member.workspace;
        let events = workspace.events_after(&id("c-1"), 1, 10, usize::MAX).await;
        let Some(ChatEvent::RunStatus { status }) = events.unwrap().pop().map(|one| one.event)
        else {
            panic!("no status stored");
        };
        let owner = AgentOwner::Shared {
            member: bo,
            owner: member_id("m1"),
        };
        assert_eq!(status.agent.map(|agent| agent.owner), Some(owner));
    }

    #[tokio::test]
    async fn a_prompt_the_server_took_is_stored_as_its_askers_only_once() {
        let fixture = Fixture::new().await;
        let (shared, (ada, bo)) = (&fixture.shared, called_by_another(&fixture).await);
        // Her machine sends his request up twice more, in words of its own.
        let events = [
            sent_up("r-1", "Go"),
            sent_up("r-1", "FORGED"),
            status(RunState::Running),
            // A replay of the first changes nothing.
            sent_up("r-1", "Go"),
        ];

        for (seq, event) in [1, 2, 3, 1].into_iter().zip(events) {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
        }

        let hers = |text: &str| (Some(member_id("m1")), text.to_owned());
        let stored = written(&ada).await;
        assert_eq!(
            stored,
            [(Some(bo), WORDS.to_owned()), hers("FORGED"), hers("status")]
        );
    }

    #[tokio::test]
    async fn a_prompt_taken_back_is_not_its_askers_when_the_machine_sends_it_up() {
        let fixture = Fixture::new().await;
        let (shared, (ada, bo)) = (&fixture.shared, called_by_another(&fixture).await);
        let workspace = &ada.member.workspace;
        let chat = id("c-1");
        let taken_back = workspace.uncall_leg(&chat, &ada.machine, ask(&bo, &R1));
        taken_back.await.unwrap();

        let sent = update("w1", "c-1", 1, sent_up("r-1", "Go"));
        store(shared, &ada.machine, sent).await.unwrap();

        assert_eq!(
            written(&ada).await,
            [(Some(member_id("m1")), "Go".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_prompt_sent_up_late_is_still_its_askers_once_its_share_ended() {
        let fixture = Fixture::new().await;
        let (shared, (ada, bo)) = (&fixture.shared, called_by_another(&fixture).await);
        // The share ends before her machine sends his prompt up: the agent stays hers.
        shared.db.set_shares(&ada.machine, &[]).await.unwrap();
        let events = [sent_up("r-1", "Go"), status(RunState::Running)];

        for (seq, event) in (1..).zip(events) {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
        }

        assert_eq!(
            written(&ada).await,
            [
                (Some(bo), WORDS.to_owned()),
                (Some(member_id("m1")), "status".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn a_run_is_its_prompts_askers_whoever_called_the_agent_since() {
        let fixture = Fixture::new().await;
        let (shared, (ada, bo)) = (&fixture.shared, called_by_another(&fixture).await);
        let (workspace, chat) = (&ada.member.workspace, id::<ChatId>("c-1"));
        // Nothing of his run arrived in time, so she calls the agent too.
        let (r2, later) = (id("r-2"), NOW + 60);
        let hers = ask(&ada.member.id, &r2);
        let leg = on(&ada.machine);
        let called = workspace.call_leg(&chat, hers, &leg, later).await;
        assert!(called.unwrap().is_some());
        let events = [
            sent_up("r-1", "Go"),
            status(RunState::Running),
            // Written in a terminal on the machine: its owner's, and so is its run.
            sent_up("r-local", "Local"),
            status(RunState::Running),
        ];

        for (seq, event) in (1..).zip(events) {
            store(shared, &ada.machine, update("w1", "c-1", seq, event))
                .await
                .unwrap();
            // His run reporting leaves her call holding the turn.
            let holds = workspace.standing(&chat, NOW + 60).await.unwrap().called;
            assert_eq!(holds.is_some(), seq < 4, "{seq}");
        }

        let (his, hers) = (Some(bo), Some(member_id("m1")));
        let stored = written(&ada).await;
        let who: Vec<_> = stored.into_iter().map(|(member, _)| member).collect();
        assert_eq!(who, [his.clone(), his, hers.clone(), hers]);
    }

    #[tokio::test]
    async fn a_machine_cant_send_up_a_prompt_as_one_another_machine_was_asked() {
        let fixture = Fixture::new().await;
        let (shared, ada) = (&fixture.shared, ada_chatting(&fixture).await);
        let workspace = &ada.member.workspace;
        let (chat, grace) = (id::<ChatId>("c-1"), grace(&fixture).await);
        let hers = ask(&ada.member.id, &R1);
        workspace.asked(&chat, &ada.machine, hers).await.unwrap();
        let request = id("r-2");
        let called = ask(&grace.member.id, &request);
        let desk = on(&grace.machine);
        let leg = workspace.call_leg(&chat, called, &desk, NOW + 60).await;
        assert!(leg.unwrap().is_some());

        // Her machine names the request Ada made of her own.
        let sent = update("w1", "c-1", 1, sent_up("r-1", "Go"));
        store(shared, &grace.machine, sent).await.unwrap();

        let stored = fixture.shared.db.find_workspace(&id("w1")).await.unwrap();
        let events = stored.unwrap().events_after(&chat, 0, 10, usize::MAX).await;
        let Some(ChatEvent::Prompt { author, text, .. }) =
            events.unwrap().pop().map(|one| one.event)
        else {
            panic!("no prompt stored");
        };
        assert_eq!((author, text), (Some(grace.member.id), "Go".to_owned()));
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
        assert_eq!(kept(ChatEvent::Newer, &leg, &leg.member), None);
        assert_eq!(kept(run(done), &leg, &leg.member), None);
        let ended = kept(run(finished.clone()), &leg, &leg.member).map(|kept| core_named(&kept));
        assert_eq!(ended, Some(run(finished)));
        let outside = ChatEvent::OutsidePrompt {
            text: "Go".to_owned(),
        };
        assert_eq!(kept(outside.clone(), &leg, &leg.member), Some(outside));
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
            shared_by: None,
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
                state: RunState::Running,
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
            assert_eq!(
                kept(prompt(author), &leg, &leg.member),
                Some(prompt(Some("m5")))
            );
        }
        for agent in [None, Some(forged("m1"))] {
            let stored = kept(status(agent), &leg, &leg.member).map(|kept| core_named(&kept));
            assert_eq!(stored, Some(status(Some(named.clone()))));
        }
        let message = ChatEvent::Message {
            author: member_id("m1"),
            text: "Hi".to_owned(),
        };
        assert_eq!(kept(message, &leg, &leg.member), None);
    }
}
