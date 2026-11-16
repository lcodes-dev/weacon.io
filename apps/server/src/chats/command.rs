//! A member's command to a chat: a message is stored here, and a command to their own agent is
//! checked, then relayed to their machine, whose answer is its receipt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, AgentOwner, Asker, ChatAction, ChatCommand, ChatEvent, ChatId, ChatTask, MachineId,
    Message, Outcome, Receipt, RunState, code,
};

use super::admit::{Admitted, admit, detached, not_owner, title};
use super::{link, quotes};
use crate::auth::Member;
use crate::db::{Chat, Incoming, Leg, NewChat, NewLeg, Workspace};
use crate::relay::{self, Back};
use crate::shared::Shared;

/// Takes `command` from `asker`, one of the chat's workspace's members. A message is stored,
/// and its receipt is the answer. A command to the asker's own agent is refused when their
/// machine or another member's working agent doesn't allow it, and else relayed once; the
/// machine's answer goes `back`.
pub(crate) async fn take(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<Option<Message>, Fault> {
    let (id, request) = (command.chat.clone(), command.request.clone());
    let chat = member.workspace.chat(&command.chat).await?;
    match admit(chat, asker.clone(), command)? {
        Admitted::Said {
            chat,
            text,
            mut task,
        } => {
            say(shared, member, chat.as_ref(), &id, text, &mut task).await?;
            let outcome = Outcome::Accepted;
            return Ok(Some(Message::Receipt(Receipt { request, outcome })));
        }
        Admitted::Prompt {
            chat: None,
            machine,
            command,
        } => start(shared, member, asker, machine, command, back).await?,
        Admitted::Prompt {
            chat: Some(chat),
            machine,
            command,
        } => prompt(shared, (member, asker), &chat, machine, command, back).await?,
        Admitted::Act { chat, command } => {
            let made = Some(chat.made).filter(|made| *made);
            let leg = member.workspace.leg(&id, &member.id).await?;
            let leg = made.and(leg).filter(|leg| leg.core_seq > 0);
            let leg = leg.ok_or_else(not_owner)?;
            serve(shared, member, &leg, command, back).await?;
        }
    }
    Ok(None)
}

// Stores a person's message, in a chat it makes when there is none.
async fn say(
    shared: &Shared,
    member: &Member,
    chat: Option<&Chat>,
    id: &ChatId,
    text: String,
    task: &mut Option<ChatTask>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    let at = shared.now();
    match chat {
        None => {
            stamp(workspace, task).await?;
            let new = NewChat {
                id,
                owner: &member.id,
                title: &title(&text),
                leg: None,
                task: task.as_ref(),
                at,
            };
            workspace.add_chat(&new).await?;
        }
        // Its first prompt is still on its way, and may yet be taken back.
        Some(chat) if !chat.made => return Err(link::offline()),
        Some(_) => {}
    }
    let event = ChatEvent::Message {
        author: member.id.clone(),
        text,
    };
    let incoming = Incoming {
        core_seq: None,
        event: &event,
        at,
    };
    let stored = workspace.append_events(id, &[incoming]).await;
    if chat.is_none() {
        if let Err(fault) = stored {
            workspace.forget_chat(id).await?;
            return Err(fault);
        }
        workspace.chat_made(id, &member.id).await?;
    } else {
        stored?;
    }
    shared.wake_feeds(workspace.id());
    shared.chats.stored(workspace.id().clone(), id.clone());
    Ok(())
}

// A chat's first prompt: the chat is made before it is sent, so two at once can't both reach
// the machine, and taken back when it can't be sent.
async fn start(
    shared: &Shared,
    member: &Member,
    asker: &Asker,
    machine: Option<MachineId>,
    mut command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    let ChatAction::Prompt {
        text,
        repo,
        agent,
        task,
        ..
    } = &mut command.action
    else {
        return Err(super::unsupported());
    };
    stamp(workspace, task).await?;
    let (title, repo, task) = (title(text), repo.clone(), task.clone());
    let (machine_id, machine) = own_machine(shared, asker, machine).await?;
    let id = command.chat.clone();
    let chat = NewChat {
        id: &id,
        owner: &member.id,
        title: &title,
        leg: Some(NewLeg {
            machine_id: &machine_id,
            machine: &machine,
            agent: agent.unwrap_or(AgentKind::Claude),
            repo: repo.as_ref(),
        }),
        task: task.as_ref(),
        at: shared.now(),
    };
    workspace.add_chat(&chat).await?;
    let follow = link::follow(workspace.id(), &id, 0);
    if let Err(fault) = relay::command(shared, &machine_id, command, follow, back) {
        workspace.forget_chat(&id).await?;
        return Err(fault);
    }
    workspace.chat_made(&id, &member.id).await?;
    shared.wake_feeds(workspace.id());
    Ok(())
}

// A prompt to a chat that is there: to the asker's own agent in it, which their first one
// there fixes, with what the others said since their last.
async fn prompt(
    shared: &Shared,
    (member, asker): (&Member, &Asker),
    chat: &Chat,
    machine: Option<MachineId>,
    mut command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    if !chat.made {
        // Its first prompt is still on its way, and may yet be taken back.
        return Err(link::offline());
    }
    let id = command.chat.clone();
    let fixed = workspace.leg(&id, &member.id).await?;
    let leg = match fixed.filter(|leg| leg.core_seq > 0) {
        Some(leg) if machine.as_ref().is_some_and(|named| *named != leg.machine) => {
            return Err(crate::db::machine_fixed());
        }
        Some(leg) => leg,
        None => {
            let ChatAction::Prompt { repo, agent, .. } = &command.action else {
                return Err(super::unsupported());
            };
            let (machine_id, machine) = own_machine(shared, asker, machine).await?;
            let leg = NewLeg {
                machine_id: &machine_id,
                machine: &machine,
                agent: agent.unwrap_or(AgentKind::Claude),
                repo: repo.as_ref(),
            };
            workspace.set_leg(&id, &member.id, &leg).await?;
            let made = workspace.leg(&id, &member.id).await?;
            made.ok_or_else(crate::db::machine_not_found)?
        }
    };
    one_at_a_time(workspace, &id, member).await?;
    if let ChatAction::Prompt { quotes, .. } = &mut command.action {
        *quotes = quotes::since(workspace, &id, &member.id).await?;
    }
    serve(shared, member, &leg, command, back).await
}

// The asker's machine `named`, which a first prompt must name, and what it is called.
async fn own_machine(
    shared: &Shared,
    asker: &Asker,
    named: Option<MachineId>,
) -> Result<(MachineId, weacon_protocol::MachineName), Fault> {
    let named = named.ok_or_else(|| {
        Fault::new(
            code!("chat.machine_required"),
            "Pick a machine for your agent in this chat",
        )
    })?;
    let owned = shared.db.machines_of(&asker.account, &[]).await?;
    let found = owned.into_iter().find(|summary| summary.id == named);
    let summary = found.ok_or_else(crate::db::machine_not_found)?;
    Ok((summary.id, summary.name))
}

// A chat runs one agent at a time: while another member's works or waits for an answer, no
// one else's is called.
async fn one_at_a_time(workspace: &Workspace, chat: &ChatId, member: &Member) -> Result<(), Fault> {
    let runs = workspace.chat_runs(chat).await?;
    let Some(run) = runs.run() else {
        return Ok(());
    };
    let working = matches!(
        run.state,
        RunState::Starting | RunState::Running | RunState::Waiting
    );
    let theirs = run.agent.as_ref().is_some_and(
        |agent| matches!(&agent.owner, AgentOwner::Member { member: owner } if *owner == member.id),
    );
    if working && !theirs {
        return Err(Fault::new(
            code!("chat.agent_working"),
            "Another person's agent is working in this chat",
        ));
    }
    Ok(())
}

// Relays `command` to the machine of `leg`, the asker's own, while they still own it.
async fn serve(
    shared: &Shared,
    member: &Member,
    leg: &Leg,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    if leg.detached {
        return Err(detached());
    }
    let held = workspace.leg_on(&command.chat, &leg.machine).await?;
    if held.is_none() {
        // A leg whose machine was unpaired can never run again; the chat stays.
        workspace.detach_leg(&command.chat, &member.id).await?;
        shared.wake_feeds(workspace.id());
        return Err(detached());
    }
    let follow = link::follow(workspace.id(), &command.chat, leg.core_seq);
    relay::command(shared, &leg.machine, command, follow, back)
}

/// Gives the task a chat's first words name the title it has now in `workspace`.
async fn stamp(workspace: &Workspace, task: &mut Option<ChatTask>) -> Result<(), Fault> {
    let Some(named) = task else {
        return Ok(());
    };
    let now = workspace.task(&named.id).await?;
    now.title.as_str().clone_into(&mut named.title);
    Ok(())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{
        AgentKind, ChatAction, ChatQuery, ChatQueryKind, ErrorBody, Message, Outcome, Receipt, code,
    };

    use tokio::sync::mpsc;
    use weacon_protocol::Envelope;

    use super::*;
    use crate::chats::test_support::{Ada, ada, attach, grace, sent};
    use crate::db::test_support::{NOW, append, id, make, prompt, said, status};
    use crate::test_support::Fixture;

    // `command`, a prompt, naming the agent to run; the server passes it on as it is.
    fn with_codex(mut command: ChatCommand) -> ChatCommand {
        if let ChatAction::Prompt { agent, .. } = &mut command.action {
            *agent = Some(AgentKind::Codex);
        }
        command
    }

    fn follow(after: u64) -> Message {
        Message::ChatQuery(ChatQuery {
            workspace: id("w1"),
            chat: id("c-1"),
            query: ChatQueryKind::Subscribe { after },
        })
    }

    // Takes `command` from Ada as her browser's envelope 4, with room for its answer.
    async fn asked(
        ada: &Ada,
        shared: &Shared,
        command: ChatCommand,
    ) -> (Result<Option<Message>, Fault>, mpsc::Receiver<Envelope>) {
        let (back, answers) = mpsc::channel(4);
        let taken = take(
            &ada.member,
            &ada.asker,
            shared,
            command,
            Back { id: 4, to: &back },
        )
        .await;
        (taken, answers)
    }

    #[tokio::test]
    async fn a_first_prompt_reaches_the_machine_stamped_then_its_chat_is_made_and_its_answer_returns()
     {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let mut attached = attach(
            shared,
            &ada,
            &["machine.connect", "chat.serve", "chat.answers"],
        );

        let first = with_codex(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)));
        let (taken, mut answers) = asked(&ada, shared, first).await;

        taken.unwrap();
        let envelope = attached.incoming.try_recv().unwrap();
        let mut relayed = with_codex(prompt("r-1", "c-1", "Fix it", None));
        relayed.by = Some(ada.asker.clone());
        assert_eq!(envelope.message, Message::ChatCommand(relayed));
        assert_eq!(sent(&mut attached), [follow(0)]);
        let chats = ada.member.workspace.chats().await.unwrap();
        let titles: Vec<_> = chats.iter().map(|chat| chat.title.as_str()).collect();
        assert_eq!(titles, ["Fix it"]);
        let leg = ada.member.workspace.leg(&id("c-1"), &ada.member.id).await;
        let leg = leg.unwrap().unwrap();
        assert_eq!(
            (leg.machine_name.as_str(), leg.agent, leg.core_seq),
            ("Studio", AgentKind::Codex, 0)
        );
        answers.try_recv().unwrap_err();
        let busy = Message::Receipt(Receipt {
            request: id("r-1"),
            outcome: Outcome::Failed {
                code: code!("chat.busy"),
            },
        });
        let answer = Envelope {
            id: envelope.id,
            message: busy.clone(),
        };
        assert!(shared.relay.answered(&ada.machine, answer));
        assert_eq!(
            answers.try_recv().unwrap(),
            Envelope {
                id: 4,
                message: busy
            }
        );
    }

    // `command`, a prompt, naming the task `task` as the client last read its title.
    fn for_task(mut command: ChatCommand, task: &str) -> ChatCommand {
        if let ChatAction::Prompt { task: named, .. } = &mut command.action {
            *named = Some(ChatTask {
                id: id(task),
                title: "As the client read it".to_owned(),
            });
        }
        command
    }

    fn task_of(message: &Message) -> Option<&ChatTask> {
        let Message::ChatCommand(ChatCommand {
            action: ChatAction::Prompt { task, .. },
            ..
        }) = message
        else {
            return None;
        };
        task.as_ref()
    }

    #[tokio::test]
    async fn a_first_prompt_for_a_task_relays_its_title_now_and_later_prompts_keep_it() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        let by = weacon_model::Attribution::by(ada.member.id.clone(), NOW);
        let task = crate::test_support::new_task("t-1", "Speed up the build", "a");
        workspace.add_task(&task, &by).await.unwrap();
        let mut attached = attach(
            shared,
            &ada,
            &["machine.connect", "chat.serve", "chat.answers"],
        );
        let first = || for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");

        asked(&ada, shared, first()).await.0.unwrap();
        let stamped = ChatTask {
            id: id("t-1"),
            title: "Speed up the build".to_owned(),
        };
        assert_eq!(task_of(&sent(&mut attached)[0]), Some(&stamped));
        let listed = workspace.chats().await.unwrap();
        assert_eq!(listed[0].task, Some(id("t-1")));
        // Sent again, it goes again to its chat, for the chat's own task.
        asked(&ada, shared, first()).await.0.unwrap();
        workspace.legs_took_prompts().await;
        let later = for_task(prompt("r-2", "c-1", "And this", None), "t-9");
        asked(&ada, shared, later).await.0.unwrap();
        let plain = prompt("r-3", "c-1", "Then that", None);
        asked(&ada, shared, plain).await.0.unwrap();
        let relayed = sent(&mut attached);
        let tasks: Vec<_> = relayed.iter().map(task_of).collect();
        assert_eq!(
            tasks,
            [Some(&stamped), None, Some(&stamped), None, None, None]
        );
    }

    #[tokio::test]
    async fn a_first_prompt_for_a_task_of_another_workspace_makes_no_chat() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let other = shared
            .db
            .create_workspace(&id("w9"), "Other")
            .await
            .unwrap();
        let bo = crate::test_support::email("bo@example.com");
        let account = crate::test_support::fresh_account();
        other.add_member(&id("m9"), &account, &bo).await.unwrap();
        let elsewhere = crate::test_support::new_task("t-1", "Plan the trip", "a");
        let by = weacon_model::Attribution::by(id("m9"), NOW);
        other.add_task(&elsewhere, &by).await.unwrap();
        let mut attached = attach(
            shared,
            &ada,
            &["machine.connect", "chat.serve", "chat.answers"],
        );

        let foreign = for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");
        let (refused, _answers) = asked(&ada, shared, foreign).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "task.not_found");
        assert!(sent(&mut attached).is_empty());
        assert_eq!(ada.member.workspace.chat(&id("c-1")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_command_to_a_machine_that_cant_answer_it_is_refused_and_a_first_prompts_chat_taken_back()
     {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let mut attached = attach(shared, &ada, &["machine.connect"]);

        let first = prompt("r-1", "c-1", "Fix it", Some(&ada.machine));
        let (refused, _answers) = asked(&ada, shared, first).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(sent(&mut attached).is_empty());
        let workspace = &ada.member.workspace;
        assert_eq!(workspace.chat(&id("c-1")).await.unwrap(), None);
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.legs_took_prompts().await;
        let (refused, _answers) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(sent(&mut attached).is_empty());
        // A core that serves chats but answers no command, as an older one, is too old for it.
        let mut older = attach(shared, &ada, &["chat.serve"]);
        let (refused, _answers) = asked(&ada, shared, prompt("r-3", "c-1", "Go", None)).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
        assert!(sent(&mut older).is_empty());
    }

    #[tokio::test]
    async fn a_command_sent_again_goes_again_and_each_answer_returns_to_its_own_ask() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.legs_took_prompts().await;
        let mut attached = attach(shared, &ada, &["chat.serve", "chat.answers"]);

        let (one, mut first) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;
        let (two, mut second) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;

        one.unwrap();
        two.unwrap();
        let envelopes: Vec<_> = std::iter::from_fn(|| attached.incoming.try_recv().ok()).collect();
        let commands: Vec<_> = envelopes
            .iter()
            .filter(|envelope| matches!(envelope.message, Message::ChatCommand(_)))
            .collect();
        assert_eq!(commands.len(), 2);
        assert!(
            envelopes
                .iter()
                .any(|envelope| envelope.message == follow(1))
        );
        let refused = Message::Error(ErrorBody {
            code: code!("chat.unavailable"),
            retryable: true,
        });
        let ids: Vec<_> = commands.iter().map(|command| command.id).collect();
        for id in ids.iter().rev() {
            let answer = Envelope {
                id: *id,
                message: refused.clone(),
            };
            assert!(shared.relay.answered(&ada.machine, answer));
        }
        assert_eq!(first.try_recv().unwrap().message, refused);
        assert_eq!(second.try_recv().unwrap().message, refused);
    }

    #[tokio::test]
    async fn a_leg_whose_machine_is_no_longer_its_members_is_detached_and_the_feed_hears() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        let idle = [(Some(1), status(RunState::Idle))];
        append(workspace, "c-1", &idle).await.unwrap();
        let before = workspace.changes_since(0, 50).await.unwrap().len();

        let (refused, _answers) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.detached");
        let leg = workspace.leg(&id("c-1"), &ada.member.id).await.unwrap();
        assert!(leg.unwrap().detached);
        let changes = workspace.changes_since(0, 50).await.unwrap();
        assert_eq!(changes.len(), before + 1);
        // The chat itself still takes what its people say.
        let hello = say("r-3", "c-1", "MARKER-HELLO");
        asked(&ada, shared, hello).await.0.unwrap();
        let (again, _answers) = asked(&ada, shared, prompt("r-4", "c-1", "Go", None)).await;
        assert_eq!(again.unwrap_err().code().as_str(), "chat.detached");
    }

    fn say(request: &str, chat: &str, text: &str) -> ChatCommand {
        let mut command = prompt(request, chat, text, None);
        command.action = ChatAction::Say {
            text: text.to_owned(),
            task: None,
        };
        command
    }

    fn message(author: &str, text: &str) -> ChatEvent {
        ChatEvent::Message {
            author: id(author),
            text: text.to_owned(),
        }
    }

    async fn events(workspace: &Workspace, chat: &str) -> Vec<ChatEvent> {
        let chat = id(chat);
        let entries = workspace.events_after(&chat, 0, 50, usize::MAX);
        let entries = entries.await.unwrap();
        entries.into_iter().map(|entry| entry.event).collect()
    }

    #[tokio::test]
    async fn a_message_makes_a_chat_or_joins_one_and_is_stored_under_its_author_for_its_workspace_only()
     {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let (ada, grace) = (ada(&fixture).await, grace(&fixture).await);
        let workspace = &ada.member.workspace;
        let other = id("w9");
        let other = shared.db.create_workspace(&other, "Other").await.unwrap();
        let mut attached = attach(shared, &ada, &["chat.serve", "chat.answers"]);
        let mut notices = shared.chats.notices();

        let first = say("r-1", "c-1", "MARKER-MORNING\nand more");
        let (taken, _answers) = asked(&ada, shared, first).await;
        let reply = say("r-2", "c-1", "MARKER-REPLY");
        let (answered, _answers) = asked(&grace, shared, reply).await;

        let accepted = |request: &str| {
            Some(Message::Receipt(Receipt {
                request: id(request),
                outcome: Outcome::Accepted,
            }))
        };
        assert_eq!(taken.unwrap(), accepted("r-1"));
        assert_eq!(answered.unwrap(), accepted("r-2"));
        // No machine is told, and no agent starts.
        assert!(sent(&mut attached).is_empty());
        let said = [
            message("m1", "MARKER-MORNING\nand more"),
            message("m5", "MARKER-REPLY"),
        ];
        assert_eq!(events(workspace, "c-1").await, said);
        let listed = workspace.chats().await.unwrap();
        let chat = listed.first().unwrap();
        assert_eq!(
            (chat.title.as_str(), chat.owner.as_str(), chat.legs.len()),
            ("MARKER-MORNING", "m1", 0)
        );
        assert_eq!(notices.try_recv().unwrap(), (id("w1"), id("c-1")));
        assert!(other.chats().await.unwrap().is_empty());
        assert_eq!(other.chat(&id("c-1")).await.unwrap(), None);
        let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    fn quotes_of(message: &Message) -> Vec<(String, Option<AgentKind>, String)> {
        let Message::ChatCommand(ChatCommand {
            action: ChatAction::Prompt { quotes, .. },
            ..
        }) = message
        else {
            panic!("not a prompt: {message:?}");
        };
        let agent = |quote: &weacon_protocol::Quote| quote.agent.as_ref().map(|agent| agent.kind);
        quotes
            .iter()
            .map(|quote| (quote.name.clone(), agent(quote), quote.text.clone()))
            .collect()
    }

    // What `person`'s machine sends up as its log's entry `seq`, stored as the server keeps it.
    async fn up(shared: &Shared, person: &Ada, seq: u64, event: ChatEvent) {
        let update = weacon_protocol::ChatUpdate {
            workspace: id("w1"),
            chat: id("c-1"),
            entry: weacon_protocol::ChatEntry { seq, event },
        };
        let stored = crate::chats::upload::store(shared, &person.machine, update);
        stored.await.unwrap();
    }

    fn prompted(request: &str, text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: id(request),
            text: text.to_owned(),
            author: None,
            repo: None,
            agent: None,
            task: None,
        }
    }

    #[tokio::test]
    async fn each_member_calls_their_own_agent_on_their_own_machine_with_what_the_others_said() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let (ada, grace) = (ada(&fixture).await, grace(&fixture).await);
        let offers = ["chat.serve", "chat.answers"];
        let mut adas = attach(shared, &ada, &offers);
        let mut graces = attach(shared, &grace, &offers);
        let ask = |person, command| async move { asked(person, shared, command).await.0 };
        ask(&ada, say("r-1", "c-1", "Morning")).await.unwrap();
        ask(&grace, say("r-2", "c-1", "Try the cache"))
            .await
            .unwrap();

        // Grace has no agent in the chat until a prompt of hers names her machine.
        let unnamed = ask(&grace, prompt("r-3", "c-1", "Go", None)).await;
        let stolen = ask(&grace, prompt("r-3", "c-1", "Go", Some(&ada.machine))).await;
        ask(&ada, prompt("r-4", "c-1", "Fix it", Some(&ada.machine)))
            .await
            .unwrap();

        assert_eq!(
            unnamed.unwrap_err().code().as_str(),
            "chat.machine_required"
        );
        assert_eq!(stolen.unwrap_err().code().as_str(), "machine.not_found");
        assert!(sent(&mut graces).is_empty());
        let relayed = sent(&mut adas);
        let person = |name: &str, text: &str| (name.to_owned(), None, text.to_owned());
        // Her own message is quoted too: her agent hasn't seen it.
        let both = [person("ada", "Morning"), person("grace", "Try the cache")];
        assert_eq!(quotes_of(&relayed[0]), both);
        up(shared, &ada, 1, prompted("r-4", "Fix it")).await;
        up(shared, &ada, 2, status(RunState::Running)).await;
        // While Ada's agent works, Grace's isn't called, and Grace can't stop or answer Ada's.
        let grace_first = || prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        let busy = ask(&grace, grace_first()).await;
        assert_eq!(busy.unwrap_err().code().as_str(), "chat.agent_working");
        let mut stop = prompt("r-6", "c-1", "x", None);
        stop.action = ChatAction::Interrupt;
        let stopped = ask(&grace, stop.clone()).await;
        assert_eq!(stopped.unwrap_err().code().as_str(), "chat.not_owner");
        ask(&grace, say("r-7", "c-1", "Still here")).await.unwrap();
        ask(&ada, stop).await.unwrap();
        assert_eq!(sent(&mut adas).len(), 2);
        up(shared, &ada, 3, said("Fixed.")).await;
        up(shared, &ada, 4, status(RunState::Idle)).await;

        ask(&grace, grace_first()).await.unwrap();

        let relayed = sent(&mut graces);
        let quoted = [
            person("ada", "Morning"),
            person("grace", "Try the cache"),
            person("ada", "Fix it"),
            person("grace", "Still here"),
            (
                "ada".to_owned(),
                Some(AgentKind::Claude),
                "Fixed.".to_owned(),
            ),
        ];
        assert_eq!(quotes_of(&relayed[0]), quoted);
        assert_eq!(relayed[1], follow(0));
        up(shared, &grace, 1, prompted("r-5", "And mine")).await;
        // Since her last prompt, Ada's agent has only Grace's own words to read.
        ask(&ada, prompt("r-8", "c-1", "Thanks", None))
            .await
            .unwrap();
        let again = sent(&mut adas);
        let since = [person("grace", "Still here"), person("grace", "And mine")];
        assert_eq!(quotes_of(&again[0]), since);
        assert_eq!(again[1], follow(4));
        let legs = ada.member.workspace.chats().await.unwrap().remove(0).legs;
        let on: Vec<_> = legs
            .iter()
            .map(|leg| (leg.member.as_str(), leg.machine_id.as_str()))
            .collect();
        assert_eq!(on, [("m1", ada.machine.as_str()), ("m5", "mc-grace")]);
    }

    #[tokio::test]
    async fn an_action_the_server_doesnt_list_never_reaches_the_machine() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.legs_took_prompts().await;
        let mut serving = attach(shared, &ada, &["chat.serve", "chat.answers"]);
        for action in [ChatAction::Hold, ChatAction::Release] {
            let mut command = prompt("r-2", "c-1", "Go", None);
            command.action = action;

            let (refused, _answers) = asked(&ada, shared, command).await;

            assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
            assert!(sent(&mut serving).is_empty());
        }
    }
}
