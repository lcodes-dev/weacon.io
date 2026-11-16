//! A member's command to a chat: a message is stored here, and a command to one of its agents
//! is checked, then relayed to that agent's machine, whose answer is its receipt.

use weacon_model::{ChatRuns, chat_title};
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentKind, Asker, ChatAction, ChatCommand, ChatEvent, ChatId, ChatTask, MachineId, MemberId,
    Message, Outcome, Receipt, RequestId, RunState, code,
};

use super::admit::{Admitted, admit, not_owner};
use super::{link, quotes};
use crate::auth::Member;
use crate::db::{Asked, Chat, Incoming, Leg, NewChat, NewLeg, Workspace};
use crate::relay::{self, Back};
use crate::shared::Shared;

/// Who sends a command: a member of the chat's workspace, and their account.
#[derive(Clone, Copy)]
struct Sender<'a> {
    shared: &'a Shared,
    member: &'a Member,
    asker: &'a Asker,
}

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
    let from = Sender {
        shared,
        member,
        asker,
    };
    let chat = member.workspace.chat(&command.chat).await?;
    match admit(chat, asker.clone(), command)? {
        Admitted::Said {
            chat,
            text,
            mut task,
        } => {
            say(from, chat.as_ref(), &id, text, &mut task).await?;
            let outcome = Outcome::Accepted;
            return Ok(Some(Message::Receipt(Receipt { request, outcome })));
        }
        Admitted::Prompt {
            chat: None,
            machine,
            command,
        } => start(from, machine, command, back).await?,
        Admitted::Prompt {
            chat: Some(chat),
            machine,
            command,
        } => prompt(from, &chat, machine, command, back).await?,
        Admitted::Act { chat, mut command } => {
            let leg = acted_on(from, &chat, &mut command).await?;
            serve(from, &leg, command, back).await?;
        }
    }
    Ok(None)
}

// The leg an answer or a stop is for. An answer is for the run it names, under its machine's
// name for it, and only from the owner of the machine that runs it. A stop is for the leg at
// work, from the member it works for or its machine's owner, and else for a leg the asker
// called last.
async fn acted_on(
    Sender {
        shared,
        member,
        asker,
    }: Sender<'_>,
    chat: &Chat,
    command: &mut ChatCommand,
) -> Result<Leg, Fault> {
    let workspace = &member.workspace;
    // The run is read before the legs, so whom a leg works for is never older than its run.
    let runs = workspace.chat_runs(&command.chat).await?;
    let all = workspace.legs(&command.chat).await?;
    let mut legs = all.iter().filter(|leg| chat.made && leg.core_seq > 0);
    if let ChatAction::Permission { answer } = &mut command.action {
        let asked = legs.find_map(|leg| Some((leg.own_run(&answer.run)?, leg)));
        let (run, leg) = asked.ok_or_else(not_owner)?;
        answer.run = run;
        let owns = shared.db.has_machine(&asker.account, &leg.machine).await?;
        return owns.then(|| leg.clone()).ok_or_else(not_owner);
    }
    let legs: Vec<Leg> = legs.cloned().collect();
    let theirs = |leg: &&Leg| leg.member == member.id;
    // A run at work is stopped by whom it works for or its machine's owner, and no one else,
    // whatever call of theirs still holds the turn.
    if let Some(working) = at_work(&runs, &legs) {
        let owns = shared.db.has_machine(&asker.account, &working.machine);
        let may = theirs(&working) || owns.await?;
        return may.then(|| working.clone()).ok_or_else(not_owner);
    }
    // Before its run reports, a call in flight says which leg is meant, even one whose machine
    // has sent nothing up yet: the asker's own call, and never a leg someone else just called.
    let called = workspace.calling(&command.chat, shared.now()).await?;
    let (on, by) = called.unzip();
    let held = |leg: &&Leg| !leg.detached && Some(&leg.machine) == on.as_ref();
    let leg = if by.as_ref() == Some(&member.id) {
        all.iter().find(held)
    } else {
        legs.iter().find(|leg| theirs(leg) && !held(leg))
    };
    leg.cloned().ok_or_else(not_owner)
}

// Stores a person's message, in a chat it makes when there is none.
async fn say(
    Sender { shared, member, .. }: Sender<'_>,
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
                title: &chat_title(&text),
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
    from: Sender<'_>,
    machine: Option<MachineId>,
    mut command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let Sender { shared, member, .. } = from;
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
    let (title, repo, task) = (chat_title(text), repo.clone(), task.clone());
    let (id, words) = (command.chat.clone(), text.clone());
    let (machine_id, machine) = callable(from, &id, machine).await?;
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
    let asked = (&command.request, words.as_str());
    first_call(workspace, &id, (&machine_id, &member.id), asked, chat.at).await?;
    let follow = link::follow(workspace.id(), &id, 0);
    if let Err(fault) = relay::command(shared, &machine_id, command, follow, back) {
        workspace.forget_chat(&id).await?;
        return Err(fault);
    }
    // Sent, so its machine's answer is its receipt, whatever the list shows.
    if let Err(fault) = workspace.chat_made(chat.id, &member.id).await {
        let fields = [
            ("workspace_id", workspace.id().as_str().into()),
            ("chat_id", chat.id.as_str().into()),
            ("err", Field::error(&fault)),
        ];
        shared.log.warn("chat not made", &fields);
    }
    shared.wake_feeds(workspace.id());
    Ok(())
}

// Takes a new chat's turn for its first prompt, `asked`, and notes it; a chat it can't note
// that for is taken back.
async fn first_call(
    workspace: &Workspace,
    id: &ChatId,
    (machine, member): (&MachineId, &MemberId),
    (request, text): (&RequestId, &str),
    at: i64,
) -> Result<(), Fault> {
    let asked = Asked {
        member,
        request,
        text,
        away: None,
    };
    let called = workspace.call(id, (machine, member), at).await;
    let noted = match called {
        Ok(_) => workspace.asked(id, machine, asked).await,
        Err(fault) => Err(fault),
    };
    if noted.is_err() {
        workspace.forget_chat(id).await?;
    }
    noted
}

// A prompt to a chat that is there: to its agent on the machine the prompt names, which the
// first one to it there makes, with what the chat's others said since that agent's last.
async fn prompt(
    from: Sender<'_>,
    chat: &Chat,
    machine: Option<MachineId>,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let member = from.member;
    let workspace = &member.workspace;
    if !chat.made {
        // Its first prompt is still on its way, and may yet be taken back.
        return Err(link::offline());
    }
    let (id, now) = (command.chat.clone(), from.shared.now());
    let (machine_id, machine) = callable(from, &id, machine).await?;
    let ChatAction::Prompt {
        text, repo, agent, ..
    } = &command.action
    else {
        return Err(super::unsupported());
    };
    let (request, words) = (command.request.clone(), text.clone());
    let new = NewLeg {
        machine_id: &machine_id,
        machine: &machine,
        agent: agent.unwrap_or(AgentKind::Claude),
        repo: repo.as_ref(),
    };
    let away = away(from, &id).await?;
    let asked = Asked {
        member: &member.id,
        request: &request,
        text: &words,
        away: away.as_ref(),
    };
    let called = workspace.call_leg(&id, asked, &new, now).await?;
    let leg = called.ok_or_else(agent_working)?;
    // A machine that was away and came back as the turn was taken is at work after all.
    let links = &from.shared.links;
    let back_since = away.as_ref().is_some_and(|machine| links.is_open(machine));
    let sent = if back_since {
        Err(agent_working())
    } else {
        relay_to(from, (chat, &leg), command, back).await
    };
    if let Err(fault) = sent {
        workspace.uncall_leg(&id, &leg.machine, asked).await?;
        return Err(fault);
    }
    workspace.quoted(&id, &leg.machine, chat.seq).await
}

// Relays the prompt `command` to `leg`, with what `chat`'s others said since its last.
async fn relay_to(
    Sender { shared, member, .. }: Sender<'_>,
    (chat, leg): (&Chat, &Leg),
    mut command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    if let ChatAction::Prompt { quotes, .. } = &mut command.action {
        let stretch = (leg.quoted_seq, chat.seq);
        *quotes = quotes::between(workspace, &command.chat, &leg.machine, stretch).await?;
    }
    let follow = link::follow(workspace.id(), &command.chat, leg.core_seq);
    relay::command(shared, &leg.machine, command, follow, back)
}

// The machine a prompt to chat `id` names, which every one must, and what it is called: the
// asker's own, or one shared with the workspace. An agent the chat has on a machine that no
// longer serves it can never run again, so it is detached; the chat stays.
async fn callable(
    Sender {
        shared,
        member,
        asker,
    }: Sender<'_>,
    id: &ChatId,
    named: Option<MachineId>,
) -> Result<(MachineId, weacon_protocol::MachineName), Fault> {
    let workspace = &member.workspace;
    let named = named.ok_or_else(|| {
        Fault::new(
            code!("chat.machine_required"),
            "Pick the agent this prompt is for",
        )
    })?;
    if let Some(name) = workspace.callable(&asker.account, &named).await? {
        return Ok((named, name));
    }
    let Some(leg) = workspace.leg(id, &named).await? else {
        return Err(crate::db::machine_not_found());
    };
    if workspace.leg_on(id, &named).await?.is_some() {
        return Err(crate::db::machine_not_found());
    }
    if !leg.detached {
        workspace.detach_leg(id, &leg).await?;
        shared.wake_feeds(workspace.id());
    }
    Err(crate::db::detached())
}

// The machine whose agent is at work in the chat `id` while it is away, as a closed laptop
// is: it holds no one back. A chat otherwise runs one agent at a time, which the call itself
// checks as it takes the turn.
async fn away(
    Sender { shared, member, .. }: Sender<'_>,
    id: &ChatId,
) -> Result<Option<MachineId>, Fault> {
    let workspace = &member.workspace;
    let runs = workspace.chat_runs(id).await?;
    let legs = workspace.legs(id).await?;
    let working = at_work(&runs, &legs);
    let away = working.filter(|leg| !shared.links.is_open(&leg.machine));
    Ok(away.map(|leg| leg.machine.clone()))
}

// Which of `legs` works or waits for an answer, as the chat's `runs` say, if one does.
fn at_work<'a>(runs: &ChatRuns, legs: &'a [Leg]) -> Option<&'a Leg> {
    let working = |state| {
        matches!(
            state,
            RunState::Starting | RunState::Running | RunState::Waiting
        )
    };
    let run = runs.run().filter(|run| working(run.state))?;
    let named = legs.iter().find(|leg| leg.own_run(&run.run).is_some());
    // A run from before legs named theirs is the chat's first leg's.
    named.or_else(|| legs.first())
}

fn agent_working() -> Fault {
    Fault::new(
        code!("chat.agent_working"),
        "Another agent is working in this chat",
    )
}

// Relays an answer or a stop to the machine of `leg`, while the asker may still call it.
async fn serve(
    from: Sender<'_>,
    leg: &Leg,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &from.member.workspace;
    if leg.detached {
        return Err(crate::db::detached());
    }
    callable(from, &command.chat, Some(leg.machine.clone())).await?;
    let follow = link::follow(workspace.id(), &command.chat, leg.core_seq);
    relay::command(from.shared, &leg.machine, command, follow, back)
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

    // The prompt "Go" to the agent that chat c-1 has on `person`'s machine.
    fn go(request: &str, person: &Ada) -> ChatCommand {
        prompt(request, "c-1", "Go", Some(&person.machine))
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
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

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
        let leg = ada.member.workspace.leg(&id("c-1"), &ada.machine).await;
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
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);
        let first = |request| for_task(prompt(request, "c-1", "Fix it", Some(&ada.machine)), "t-1");

        asked(&ada, shared, first("r-1")).await.0.unwrap();
        let stamped = ChatTask {
            id: id("t-1"),
            title: "Speed up the build".to_owned(),
        };
        assert_eq!(task_of(&sent(&mut attached)[0]), Some(&stamped));
        let listed = workspace.chats().await.unwrap();
        assert_eq!(listed[0].task, Some(id("t-1")));
        // Sent again, it goes again to its chat, for the chat's own task.
        asked(&ada, shared, first("r-9")).await.0.unwrap();
        workspace.legs_took_prompts().await;
        let later = for_task(prompt("r-2", "c-1", "And this", Some(&ada.machine)), "t-9");
        asked(&ada, shared, later).await.0.unwrap();
        let plain = prompt("r-3", "c-1", "Then that", Some(&ada.machine));
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
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

        let foreign = for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");
        let (refused, _answers) = asked(&ada, shared, foreign).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "task.not_found");
        assert!(sent(&mut attached).is_empty());
        assert_eq!(ada.member.workspace.chat(&id("c-1")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_first_prompt_sent_whose_chat_cant_be_listed_still_waits_for_its_machine() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        ada.member.workspace.break_changes().await;
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

        let first = prompt("r-1", "c-1", "Fix it", Some(&ada.machine));
        let (taken, mut answers) = asked(&ada, shared, first).await;

        taken.unwrap();
        assert_eq!(sent(&mut attached).len(), 2);
        answers.try_recv().unwrap_err();
        let logged = crate::test_support::messages(&fixture.records);
        assert!(logged.contains(&"chat not made"), "{logged:?}");
    }

    #[tokio::test]
    async fn a_first_prompt_whose_call_cant_be_stored_is_refused_and_its_chat_taken_back() {
        for turn in [true, false] {
            let fixture = Fixture::new().await;
            let shared = &fixture.shared;
            let ada = ada(&fixture).await;
            let workspace = &ada.member.workspace;
            workspace.break_calls(turn).await;
            let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

            let first = prompt("r-1", "c-1", "Fix it", Some(&ada.machine));
            let (refused, _answers) = asked(&ada, shared, first).await;

            refused.unwrap_err();
            assert!(sent(&mut attached).is_empty(), "{turn}");
            assert_eq!(workspace.chat(&id("c-1")).await.unwrap(), None, "{turn}");
        }
    }

    #[tokio::test]
    async fn a_command_to_a_machine_that_cant_take_it_is_refused_and_a_first_prompts_chat_taken_back()
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
        let (refused, _answers) = asked(&ada, shared, go("r-2", &ada)).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn a_second_command_goes_too_and_each_answer_returns_to_its_own_ask() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.legs_took_prompts().await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        let (one, mut first) = asked(&ada, shared, go("r-2", &ada)).await;
        let (two, mut second) = asked(&ada, shared, go("r-3", &ada)).await;

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
    async fn a_leg_whose_machine_is_gone_is_detached_and_the_feed_hears() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        let idle = [(Some(1), status(RunState::Idle))];
        append(workspace, "c-1", &idle).await.unwrap();
        let before = workspace.changes_since(0, 50).await.unwrap().len();

        let gone = id::<MachineId>("mc-1");
        let go = |request| prompt(request, "c-1", "Go", Some(&gone));
        let (refused, _answers) = asked(&ada, shared, go("r-2")).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.detached");
        let leg = workspace.leg(&id("c-1"), &gone).await.unwrap();
        assert!(leg.unwrap().detached);
        let changes = workspace.changes_since(0, 50).await.unwrap();
        assert_eq!(changes.len(), before + 1);
        // The chat itself still takes what its people say.
        let hello = say("r-3", "c-1", "MARKER-HELLO");
        asked(&ada, shared, hello).await.0.unwrap();
        let (again, _answers) = asked(&ada, shared, go("r-4")).await;
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
        let mut attached = attach(shared, &ada, &["chat.serve"]);
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
            pick: None,
            request: id(request),
            text: text.to_owned(),
            author: None,
            repo: None,
            agent: None,
            task: None,
        }
    }

    // Ada and Grace in chat c-1, each with her machine connected, after a message from each.
    struct Two {
        fixture: Fixture,
        ada: Ada,
        grace: Ada,
        adas: crate::links::Attached,
        graces: crate::links::Attached,
    }

    impl Two {
        async fn new() -> Self {
            let fixture = Fixture::new().await;
            let (ada, grace) = (ada(&fixture).await, grace(&fixture).await);
            let offers = ["chat.serve"];
            let adas = attach(&fixture.shared, &ada, &offers);
            let graces = attach(&fixture.shared, &grace, &offers);
            let two = Self {
                fixture,
                ada,
                grace,
                adas,
                graces,
            };
            two.ask(&two.ada, say("r-1", "c-1", "Morning"))
                .await
                .unwrap();
            let reply = say("r-2", "c-1", "Try the cache");
            two.ask(&two.grace, reply).await.unwrap();
            two
        }

        async fn ask(&self, person: &Ada, command: ChatCommand) -> Result<Option<Message>, Fault> {
            asked(person, &self.fixture.shared, command).await.0
        }

        // What `person`'s machine sends up as its log's entry `seq`.
        async fn up(&self, person: &Ada, seq: u64, event: ChatEvent) {
            up(&self.fixture.shared, person, seq, event).await;
        }

        // Ada's first prompt, taken by her machine, which works on it.
        async fn ada_calls(&mut self) -> Vec<Message> {
            let first = prompt("r-4", "c-1", "Fix it", Some(&self.ada.machine));
            self.ask(&self.ada, first).await.unwrap();
            self.up(&self.ada, 1, prompted("r-4", "Fix it")).await;
            self.up(&self.ada, 2, status(RunState::Running)).await;
            sent(&mut self.adas)
        }
    }

    fn person(name: &str, text: &str) -> (String, Option<AgentKind>, String) {
        (name.to_owned(), None, text.to_owned())
    }

    #[tokio::test]
    async fn a_prompt_names_the_machine_of_the_agent_it_is_for_which_its_asker_may_call() {
        let mut two = Two::new().await;
        let (ada, grace) = (&two.ada, &two.grace);

        let unnamed = two.ask(grace, prompt("r-3", "c-1", "Go", None)).await;
        let theirs = prompt("r-3", "c-1", "Go", Some(&ada.machine));
        let stolen = two.ask(grace, theirs).await;

        let code = |refused: Result<Option<Message>, Fault>| refused.unwrap_err().code().clone();
        assert_eq!(code(unnamed), code!("chat.machine_required"));
        assert_eq!(code(stolen), code!("machine.not_found"));
        assert!(sent(&mut two.graces).is_empty());
        assert!(sent(&mut two.adas).is_empty());
        let mine = prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        two.ask(grace, mine).await.unwrap();
        assert_eq!(sent(&mut two.graces).len(), 2);
        // Her next one goes to the same agent, before her machine's first word too.
        let more = prompt("r-6", "c-1", "And more", Some(&grace.machine));
        two.ask(grace, more).await.unwrap();
        assert_eq!(sent(&mut two.graces).len(), 2);
    }

    #[tokio::test]
    async fn while_one_members_agent_works_no_one_elses_is_called_and_only_its_owner_stops_it() {
        let mut two = Two::new().await;
        two.ada_calls().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let mine = || prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        let mut stop = prompt("r-6", "c-1", "x", None);
        stop.action = ChatAction::Interrupt;

        let busy = two.ask(grace, mine()).await;
        let stopped = two.ask(grace, stop.clone()).await;

        assert_eq!(busy.unwrap_err().code().as_str(), "chat.agent_working");
        assert_eq!(stopped.unwrap_err().code().as_str(), "chat.not_owner");
        // What people say always goes, and the agent's owner still asks it.
        let hello = say("r-7", "c-1", "Still here");
        two.ask(grace, hello).await.unwrap();
        two.ask(ada, stop).await.unwrap();
        let this = prompt("r-8", "c-1", "And this", Some(&ada.machine));
        two.ask(ada, this).await.unwrap();
        assert!(sent(&mut two.graces).is_empty());
        assert_eq!(sent(&mut two.adas).len(), 4);
        // Her queued turn starts, then ends.
        two.up(ada, 3, status(RunState::Running)).await;
        two.up(ada, 4, status(RunState::Idle)).await;
        two.ask(grace, mine()).await.unwrap();
        assert_eq!(sent(&mut two.graces).len(), 2);
    }

    #[tokio::test]
    async fn two_calls_at_once_dont_both_run_and_a_call_that_never_left_holds_nothing() {
        let mut two = Two::new().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let hers = |request| prompt(request, "c-1", "Fix it", Some(&ada.machine));
        let mine = || prompt("r-5", "c-1", "And mine", Some(&grace.machine));

        // Ada's reached her machine, whose run hasn't reported yet.
        two.ask(ada, hers("r-4")).await.unwrap();
        let second = two.ask(grace, mine()).await;

        assert_eq!(second.unwrap_err().code().as_str(), "chat.agent_working");
        assert!(sent(&mut two.graces).is_empty());
        // Her own next one still goes, and her run's first word ends the wait for it.
        two.ask(ada, hers("r-7")).await.unwrap();
        two.up(ada, 1, status(RunState::Running)).await;
        two.up(ada, 2, status(RunState::Idle)).await;
        drop(std::mem::replace(
            &mut two.graces,
            attach(&two.fixture.shared, grace, &["machine.connect"]),
        ));
        let unsent = two.ask(grace, mine()).await;
        assert_eq!(unsent.unwrap_err().code().as_str(), "machine.offline");
        two.ask(ada, go("r-6", ada)).await.unwrap();
    }

    #[tokio::test]
    async fn what_is_written_while_a_prompt_is_on_its_way_is_quoted_with_the_next() {
        let mut two = Two::new().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let first = prompt("r-4", "c-1", "Fix it", Some(&ada.machine));
        two.ask(ada, first).await.unwrap();
        two.ask(grace, say("r-5", "c-1", "One more thing"))
            .await
            .unwrap();
        two.up(ada, 1, prompted("r-4", "Fix it")).await;
        two.up(ada, 2, status(RunState::Running)).await;
        two.up(ada, 3, status(RunState::Idle)).await;
        sent(&mut two.adas);

        let now = prompt("r-6", "c-1", "And now?", Some(&ada.machine));
        two.ask(ada, now).await.unwrap();

        let relayed = sent(&mut two.adas);
        assert_eq!(quotes_of(&relayed[0]), [person("grace", "One more thing")]);
    }

    #[tokio::test]
    async fn a_working_agent_whose_owner_left_holds_no_one_back_and_an_unnamed_one_is_the_chat_owners()
     {
        let mut two = Two::new().await;
        two.ada_calls().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let workspace = &ada.member.workspace;
        let mine = || prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        let busy = two.ask(grace, mine()).await;
        assert_eq!(busy.unwrap_err().code().as_str(), "chat.agent_working");
        // As a chat from before agents were named keeps it: Ada started this one.
        let unnamed = r#"{"run":"run-1","state":"running"}"#;
        workspace.set_run_status(&id("c-1"), unnamed).await;
        two.ask(ada, go("r-6", ada)).await.unwrap();
        two.up(ada, 3, status(RunState::Running)).await;
        let still = two.ask(grace, mine()).await;
        assert_eq!(still.unwrap_err().code().as_str(), "chat.agent_working");

        let unpaired = two
            .fixture
            .shared
            .db
            .unpair(&ada.asker.account, &ada.machine);
        assert!(unpaired.await.unwrap());

        two.ask(grace, mine()).await.unwrap();
        assert_eq!(sent(&mut two.graces).len(), 2);
    }

    #[tokio::test]
    async fn a_working_agent_whose_machine_went_away_holds_no_one_back() {
        let mut two = Two::new().await;
        two.ada_calls().await;
        let grace = &two.grace;
        let mine = || prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        let busy = two.ask(grace, mine()).await;
        assert_eq!(busy.unwrap_err().code().as_str(), "chat.agent_working");

        // As a closed laptop: still paired, its run still working as far as the chat knows.
        two.fixture.shared.links.close(&two.ada.machine);

        two.ask(grace, mine()).await.unwrap();
        assert_eq!(sent(&mut two.graces).len(), 2);
    }

    #[tokio::test]
    async fn an_agent_is_given_what_the_others_said_since_its_owners_last_prompt_as_quotes() {
        let mut two = Two::new().await;
        let first = two.ada_calls().await;
        // Her own message is quoted too: her agent hasn't seen it.
        let both = [person("ada", "Morning"), person("grace", "Try the cache")];
        assert_eq!(quotes_of(&first[0]), both);
        let (ada, grace) = (&two.ada, &two.grace);
        two.ask(grace, say("r-7", "c-1", "Still here"))
            .await
            .unwrap();
        two.up(ada, 3, said("Fixed.")).await;
        two.up(ada, 4, status(RunState::Idle)).await;

        let mine = prompt("r-5", "c-1", "And mine", Some(&grace.machine));
        two.ask(grace, mine).await.unwrap();
        two.up(grace, 1, prompted("r-5", "And mine")).await;
        two.up(grace, 2, status(RunState::Running)).await;
        two.up(grace, 3, status(RunState::Idle)).await;
        let thanks = prompt("r-8", "c-1", "Thanks", Some(&ada.machine));
        two.ask(ada, thanks).await.unwrap();

        let (hers, again) = (sent(&mut two.graces), sent(&mut two.adas));
        let by_agent = (
            "ada".to_owned(),
            Some(AgentKind::Claude),
            "Fixed.".to_owned(),
        );
        let mut quoted = both.to_vec();
        quoted.extend([
            person("ada", "Fix it"),
            person("grace", "Still here"),
            by_agent,
        ]);
        assert_eq!(quotes_of(&hers[0]), quoted);
        assert_eq!(hers[1], follow(0));
        // Since her last prompt, Ada's agent has only Grace's own words to read.
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
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        for action in [ChatAction::Hold, ChatAction::Release] {
            let mut command = prompt("r-2", "c-1", "Go", None);
            command.action = action;

            let (refused, _answers) = asked(&ada, shared, command).await;

            assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
            assert!(sent(&mut serving).is_empty());
        }
    }

    // Has Ada's machine say it is shared with `workspaces`, as its core does.
    async fn share(two: &Two, workspaces: &[&str]) {
        let shared: Vec<_> = workspaces.iter().copied().map(id).collect();
        let db = &two.fixture.shared.db;
        db.set_shares(&two.ada.machine, &shared).await.unwrap();
    }

    fn act(request: &str, action: ChatAction) -> ChatCommand {
        ChatCommand {
            action,
            ..prompt(request, "c-1", "x", None)
        }
    }

    fn allow(request: &str, run: &str) -> ChatCommand {
        let answer = weacon_protocol::PermissionAnswer {
            run: id(run),
            prompt: 1,
            allow: true,
        };
        act(request, ChatAction::Permission { answer })
    }

    fn code(refused: Result<Option<Message>, Fault>) -> String {
        refused.unwrap_err().code().as_str().to_owned()
    }

    // The commands among what a machine was sent, each with the account it names as its asker.
    fn commands(sent: Vec<Message>) -> Vec<(ChatAction, weacon_protocol::AccountId)> {
        let command = |message| {
            let Message::ChatCommand(command) = message else {
                return None;
            };
            Some((command.action, command.by?.account))
        };
        sent.into_iter().filter_map(command).collect()
    }

    #[tokio::test]
    async fn a_member_calls_a_machine_shared_with_their_workspace_until_its_owner_stops_sharing_it()
    {
        let mut two = Two::new().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let workspace = &ada.member.workspace;
        let call = |request| prompt(request, "c-1", "Go", Some(&ada.machine));

        let unshared = two.ask(grace, call("r-3")).await;
        share(&two, &["w1"]).await;
        two.ask(grace, call("r-4")).await.unwrap();

        assert_eq!(code(unshared), "machine.not_found");
        let called = commands(sent(&mut two.adas));
        assert_eq!(called.len(), 1);
        assert_eq!(called[0].1, grace.asker.account);
        two.up(ada, 1, prompted("r-4", "Go")).await;
        two.up(ada, 2, status(RunState::Running)).await;
        // Its events are the caller's, and its agent the workspace's shared one.
        let stored = events(workspace, "c-1").await;
        let Some(ChatEvent::Prompt { author, .. }) = stored.get(2) else {
            panic!("no prompt: {stored:?}");
        };
        assert_eq!(*author, Some(grace.member.id.clone()));
        let Some(ChatEvent::RunStatus { status }) = stored.last() else {
            panic!("no status: {stored:?}");
        };
        let agent = status.agent.clone().unwrap();
        let shared_agent = weacon_protocol::AgentOwner::Shared {
            member: grace.member.id.clone(),
            owner: ada.member.id.clone(),
        };
        assert_eq!(agent.owner, shared_agent);
        assert_eq!(agent.machine.unwrap().as_str(), "Studio");
        // What it says is quoted to the others' agents as the shared agent's.
        two.up(ada, 3, said("Done.")).await;
        let chat = id("c-1");
        let quoted = quotes::between(workspace, &chat, &grace.machine, (0, 99));
        let quoted = quoted.await.unwrap();
        let by_agent = quoted.iter().find_map(|quote| quote.agent.as_ref());
        assert!(by_agent.is_some_and(|agent| agent.shared));
        let idle = crate::db::test_support::status(RunState::Idle);
        two.up(ada, 4, idle).await;
        share(&two, &[]).await;
        let after = two.ask(grace, go("r-5", ada)).await;
        assert_eq!(code(after), "machine.not_found");
        assert!(sent(&mut two.adas).is_empty());
        // The agent stays its owner's to call, and works for her from then on.
        two.ask(ada, go("r-6", ada)).await.unwrap();
        let owners = commands(sent(&mut two.adas));
        assert_eq!(owners[0].1, ada.asker.account);
        let leg = workspace.leg(&chat, &ada.machine).await.unwrap().unwrap();
        assert_eq!((leg.member, leg.detached), (ada.member.id.clone(), false));
    }

    #[tokio::test]
    async fn only_a_shared_machines_owner_answers_its_prompts_and_its_caller_or_its_owner_stops_it()
    {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        let call = prompt("r-4", "c-1", "Go", Some(&ada.machine));
        two.ask(grace, call).await.unwrap();
        two.up(ada, 1, prompted("r-4", "Go")).await;
        two.up(ada, 2, status(RunState::Running)).await;
        two.up(ada, 3, crate::db::test_support::asks(1)).await;
        two.up(ada, 4, status(RunState::Waiting)).await;
        sent(&mut two.adas);

        let by_caller = two.ask(grace, allow("r-5", "run-1-l1")).await;
        let another_run = two.ask(ada, allow("r-6", "run-1-l2")).await;
        two.ask(ada, allow("r-7", "run-1-l1")).await.unwrap();
        two.ask(grace, act("r-8", ChatAction::Interrupt))
            .await
            .unwrap();
        two.ask(ada, act("r-9", ChatAction::Interrupt))
            .await
            .unwrap();

        assert_eq!(code(by_caller), "chat.not_owner");
        assert_eq!(code(another_run), "chat.not_owner");
        // The answer names the run as its own machine does.
        let (owner, caller) = (ada.asker.account.clone(), grace.asker.account.clone());
        let answer = allow("r-7", "run-1").action;
        let relayed = [
            (answer, owner.clone()),
            (ChatAction::Interrupt, caller),
            (ChatAction::Interrupt, owner),
        ];
        assert_eq!(commands(sent(&mut two.adas)), relayed);
        assert!(sent(&mut two.graces).is_empty());
    }

    #[tokio::test]
    async fn what_still_runs_on_a_machine_no_longer_shared_is_its_owners_to_stop_and_no_one_elses()
    {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        let call = prompt("r-4", "c-1", "Go", Some(&ada.machine));
        two.ask(grace, call).await.unwrap();
        two.up(ada, 1, prompted("r-4", "Go")).await;
        two.up(ada, 2, status(RunState::Running)).await;
        two.up(ada, 3, crate::db::test_support::asks(1)).await;
        sent(&mut two.adas);
        share(&two, &[]).await;

        let by_caller = two.ask(grace, act("r-5", ChatAction::Interrupt)).await;
        two.ask(ada, act("r-6", ChatAction::Interrupt))
            .await
            .unwrap();
        two.ask(ada, allow("r-7", "run-1-l1")).await.unwrap();
        let more = two.ask(grace, go("r-8", ada)).await;
        // What it sends from then on is its owner's own agent's.
        two.up(ada, 4, status(RunState::Waiting)).await;
        let stored = events(&ada.member.workspace, "c-1").await;
        let Some(ChatEvent::RunStatus { status }) = stored.last() else {
            panic!("no status: {stored:?}");
        };
        let owner = status.agent.clone().map(|agent| agent.owner);
        let hers = weacon_protocol::AgentOwner::Member {
            member: ada.member.id.clone(),
        };
        assert_eq!(owner, Some(hers));

        assert_eq!(code(by_caller), "chat.not_owner");
        assert_eq!(code(more), "machine.not_found");
        let owner = ada.asker.account.clone();
        let ended = [
            (ChatAction::Interrupt, owner.clone()),
            (allow("r-7", "run-1").action, owner),
        ];
        assert_eq!(commands(sent(&mut two.adas)), ended);
    }

    #[tokio::test]
    async fn a_machine_shared_with_one_workspace_is_neither_listed_nor_called_in_another() {
        let two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        let db = &two.fixture.shared.db;
        let other = db.create_workspace(&id("w9"), "Other").await.unwrap();
        let address = |name: &str| crate::test_support::email(&format!("{name}@example.com"));
        for (member, person, name) in [("m1", ada, "ada"), ("m5", grace, "grace")] {
            let account = &person.asker.account;
            other
                .add_member(&id(member), account, &address(name))
                .await
                .unwrap();
        }
        let there = Member {
            workspace: other,
            id: id("m5"),
            token_hash: [0; 32],
        };
        let mut call = prompt("r-3", "c-9", "Go", Some(&ada.machine));
        call.workspace = id("w9");
        let (back, _answers) = mpsc::channel(4);
        let back = Back { id: 4, to: &back };

        let elsewhere = take(&there, &grace.asker, &two.fixture.shared, call, back).await;

        assert_eq!(code(elsewhere), "machine.not_found");
        assert!(there.workspace.shared_machines().await.unwrap().is_empty());
        let listed = ada.member.workspace.shared_machines().await.unwrap();
        let shown: Vec<_> = listed
            .iter()
            .map(|machine| (machine.name.as_str(), machine.owner.as_str()))
            .collect();
        assert_eq!(shown, [("Studio", "m1")]);
        // Its core naming a workspace its owner isn't in shares nothing there.
        let stranger = db.create_workspace(&id("w8"), "Theirs").await.unwrap();
        share(&two, &["w1", "w8"]).await;
        assert!(stranger.shared_machines().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_second_member_calls_the_same_shared_agent_with_only_what_it_hasnt_got_as_quotes() {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        two.ada_calls().await;
        let (ada, grace) = (&two.ada, &two.grace);
        let workspace = &ada.member.workspace;
        two.up(ada, 3, said("Fixed.")).await;
        two.up(ada, 4, status(RunState::Idle)).await;
        two.ask(grace, say("r-5", "c-1", "Still here"))
            .await
            .unwrap();

        let second = prompt("r-6", "c-1", "And mine", Some(&ada.machine));
        two.ask(grace, second).await.unwrap();

        // What the agent was told or said itself isn't quoted back to it.
        let hers = sent(&mut two.adas);
        assert_eq!(quotes_of(&hers[0]), [person("grace", "Still here")]);
        assert_eq!(hers[1], follow(4));
        assert_eq!(commands(hers)[0].1, grace.asker.account);
        two.up(ada, 5, prompted("r-6", "And mine")).await;
        two.up(ada, 6, status(RunState::Running)).await;
        two.up(ada, 7, status(RunState::Idle)).await;
        let stored = events(workspace, "c-1").await;
        let authors: Vec<_> = stored.iter().filter_map(author_of).collect();
        assert_eq!(authors, [&ada.member.id, &grace.member.id]);
        // One agent on the machine, whoever called it, and no words of its own callers to quote.
        let legs = workspace.chats().await.unwrap().remove(0).legs;
        let on: Vec<_> = legs.iter().map(|leg| leg.machine_id.as_str()).collect();
        assert_eq!(on, [ada.machine.as_str()]);
        let thanks = prompt("r-7", "c-1", "Thanks", Some(&ada.machine));
        two.ask(ada, thanks).await.unwrap();
        assert!(quotes_of(&sent(&mut two.adas)[0]).is_empty());
    }

    #[tokio::test]
    async fn a_stop_before_a_run_reports_is_for_the_leg_the_asker_just_called() {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        // Grace has called her own agent, and Ada the shared one, which works for Ada; both
        // rest. Then Grace calls the shared one.
        two.ask(grace, go("r-3", grace)).await.unwrap();
        two.up(grace, 1, status(RunState::Running)).await;
        two.up(grace, 2, status(RunState::Idle)).await;
        two.ask(ada, go("r-4", ada)).await.unwrap();
        two.up(ada, 1, status(RunState::Running)).await;
        two.up(ada, 2, status(RunState::Idle)).await;
        two.ask(grace, go("r-5", ada)).await.unwrap();
        sent(&mut two.adas);
        sent(&mut two.graces);

        two.ask(grace, act("r-6", ChatAction::Interrupt))
            .await
            .unwrap();

        let stopped = commands(sent(&mut two.adas));
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].0, ChatAction::Interrupt);
        assert!(sent(&mut two.graces).is_empty());
    }

    #[tokio::test]
    async fn a_stop_reaches_an_agent_called_for_the_first_time_before_it_sends_anything_up() {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        two.ask(grace, go("r-3", ada)).await.unwrap();
        sent(&mut two.adas);

        two.ask(grace, act("r-4", ChatAction::Interrupt))
            .await
            .unwrap();

        let stopped = commands(sent(&mut two.adas));
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].0, ChatAction::Interrupt);
    }

    #[tokio::test]
    async fn a_stop_never_reaches_an_agent_someone_else_just_called() {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        // The shared agent last worked for Grace, and rests.
        two.ask(grace, go("r-3", ada)).await.unwrap();
        two.up(ada, 1, status(RunState::Running)).await;
        two.up(ada, 2, status(RunState::Idle)).await;
        // Ada calls it; her prompt hasn't come back up yet.
        two.ask(ada, go("r-4", ada)).await.unwrap();
        sent(&mut two.adas);

        let stop = two.ask(grace, act("r-5", ChatAction::Interrupt)).await;

        assert_eq!(stop.unwrap_err().code().as_str(), "chat.not_owner");
        assert!(sent(&mut two.adas).is_empty());
    }

    #[tokio::test]
    async fn a_call_that_holds_the_turn_stops_no_run_that_works_for_someone_else() {
        let mut two = Two::new().await;
        share(&two, &["w1"]).await;
        let (ada, grace) = (&two.ada, &two.grace);
        two.ask(ada, go("r-3", ada)).await.unwrap();
        two.up(ada, 1, status(RunState::Running)).await;
        two.up(ada, 2, status(RunState::Idle)).await;
        // Grace's call holds the turn, but the machine runs a prompt its owner wrote there.
        two.ask(grace, go("r-4", ada)).await.unwrap();
        let local = ChatEvent::Prompt {
            request: id("r-local"),
            text: "Local".to_owned(),
            author: None,
            pick: None,
            repo: None,
            agent: None,
            task: None,
        };
        two.up(ada, 3, local).await;
        two.up(ada, 4, status(RunState::Running)).await;
        sent(&mut two.adas);

        let stop = two.ask(grace, act("r-5", ChatAction::Interrupt)).await;

        assert_eq!(stop.unwrap_err().code().as_str(), "chat.not_owner");
        assert!(sent(&mut two.adas).is_empty());
    }

    fn author_of(event: &ChatEvent) -> Option<&MemberId> {
        let ChatEvent::Prompt { author, .. } = event else {
            return None;
        };
        author.as_ref()
    }
}
