//! A member's command to a chat: checked here, then relayed to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{
    Asker, ChatAction, ChatCommand, ChatTask, MachineId, Message, Outcome, Receipt, code,
};

use super::admit::{Admitted, admit, detached};
use super::link;
use crate::auth::Member;
use crate::db::{Chat, NewChat, NewRequest, Recorded, Workspace};
use crate::shared::Shared;

/// Takes `command` from `asker`, one of the chat's workspace's members, refusing what the
/// chat's owner or machine doesn't allow, and relays it; its receipt says the machine has it.
pub(crate) async fn take(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    command: ChatCommand,
) -> Result<Message, Fault> {
    let request = command.request.clone();
    let chat = member.workspace.chat(&command.chat).await?;
    match admit(chat, asker.clone(), command)? {
        Admitted::New {
            title,
            machine,
            repo,
            mut command,
        } => {
            let task = stamp(&member.workspace, &mut command).await?;
            let owned = shared.db.machines_of(&asker.account, &[]).await?;
            let Some(summary) = owned.iter().find(|summary| summary.id == machine) else {
                return Err(crate::db::machine_not_found());
            };
            let chat = NewChat {
                id: &command.chat,
                owner: &member.id,
                title: &title,
                repo: repo.as_ref(),
                machine_id: &summary.id,
                machine: &summary.name,
                at: shared.now(),
                task: task.as_ref(),
            };
            match start(shared, member, &chat, &command).await {
                // Made meanwhile, by this command sent again or by another.
                Err(fault) if *fault.code() == code!("db.exists") => {
                    again(member, asker, shared, command, machine).await?;
                }
                started => started?,
            }
        }
        Admitted::Existing { chat, command } => existing(shared, member, &chat, &command).await?,
    }
    Ok(Message::Receipt(Receipt {
        request,
        outcome: Outcome::Accepted,
    }))
}

// A first prompt to `machine` whose chat was made while it was on its way, taken as one to
// that chat, which must be on `machine` too.
async fn again(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    mut command: ChatCommand,
    machine: MachineId,
) -> Result<(), Fault> {
    if let ChatAction::Prompt { machine: named, .. } = &mut command.action {
        *named = Some(machine);
    }
    // Not made, so its request id is another chat's.
    let Some(chat) = member.workspace.chat(&command.chat).await? else {
        return Err(conflict());
    };
    match admit(Some(chat), asker.clone(), command)? {
        Admitted::Existing { chat, command } => existing(shared, member, &chat, &command).await,
        Admitted::New { .. } => Err(conflict()),
    }
}

// A chat's first prompt: the chat is made before it is sent, so two at once can't both reach
// the machine, and taken back when it can't be sent.
async fn start(
    shared: &Shared,
    member: &Member,
    chat: &NewChat<'_>,
    command: &ChatCommand,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    let asked = NewRequest {
        command,
        member: &member.id,
        at: chat.at,
    };
    workspace.add_chat(chat, &asked).await?;
    let sent = [
        Message::ChatCommand(command.clone()),
        link::follow(workspace.id(), &command.chat, 0),
    ];
    if let Err(fault) = link::send_all(shared, chat.machine_id, sent.into()) {
        workspace.forget_chat(&command.chat).await?;
        return Err(fault);
    }
    workspace.chat_made(&command.chat, &member.id).await?;
    shared.wake_feeds(workspace.id());
    Ok(())
}

// A command to a chat that exists, on a machine its owner still owns.
async fn existing(
    shared: &Shared,
    member: &Member,
    chat: &Chat,
    command: &ChatCommand,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    if !workspace.runs_on(&command.chat, &chat.machine).await? {
        // A chat whose machine was unpaired can still be read, never run.
        workspace.detach_chat(&command.chat, &member.id).await?;
        shared.wake_feeds(workspace.id());
        return Err(detached());
    }
    if !chat.made {
        // Its first prompt is still on its way, and may yet be taken back.
        return Err(link::offline());
    }
    go_on(shared, member, chat, command).await
}

// A command to a made chat: in the ledger before it is sent, and out of it again when it
// can't be, so nothing waits to run later.
async fn go_on(
    shared: &Shared,
    member: &Member,
    chat: &Chat,
    command: &ChatCommand,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    let asked = NewRequest {
        command,
        member: &member.id,
        at: shared.now(),
    };
    let recorded = workspace.record_request(&asked).await?;
    match recorded {
        Recorded::Conflict => return Err(conflict()),
        // Its receipt has ended it: nothing more to send.
        Recorded::Same { open: false } => return Ok(()),
        Recorded::New | Recorded::Same { open: true } => {}
    }
    let sent = [
        Message::ChatCommand(command.clone()),
        link::follow(workspace.id(), &command.chat, chat.core_seq),
    ];
    if let Err(fault) = link::send_all(shared, &chat.machine, sent.into()) {
        if recorded == Recorded::New {
            workspace.forget_request(&command.request).await?;
        }
        return Err(fault);
    }
    Ok(())
}

/// Gives the task a chat's first prompt names the title it has now in `workspace`.
async fn stamp(
    workspace: &Workspace,
    command: &mut ChatCommand,
) -> Result<Option<ChatTask>, Fault> {
    let ChatAction::Prompt {
        task: Some(named), ..
    } = &mut command.action
    else {
        return Ok(None);
    };
    named.title = workspace.task(&named.id).await?.title.as_str().to_owned();
    Ok(Some(named.clone()))
}

fn conflict() -> Fault {
    Fault::new(
        code!("request.conflict"),
        "That request id was used for another command",
    )
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AgentKind, ChatQuery, ChatQueryKind};

    use super::*;
    use crate::chats::test_support::{ada, attach, sent};
    use crate::db::test_support::{NOW, begin, id, make, prompt};
    use crate::test_support::Fixture;

    // `command`, a prompt, naming the agent to run; the server passes it on as it is.
    fn with_codex(mut command: ChatCommand) -> ChatCommand {
        if let ChatAction::Prompt { agent, .. } = &mut command.action {
            *agent = Some(AgentKind::Codex);
        }
        command
    }

    fn accepted(request: &str) -> Message {
        Message::Receipt(Receipt {
            request: id(request),
            outcome: Outcome::Accepted,
        })
    }

    #[tokio::test]
    async fn a_first_prompt_reaches_the_machine_stamped_then_the_chat_is_made_and_followed() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

        let asked = with_codex(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)));
        let answer = take(&ada.member, &ada.asker, shared, asked).await.unwrap();

        assert_eq!(answer, accepted("r-1"));
        let mut relayed = with_codex(prompt("r-1", "c-1", "Fix it", None));
        relayed.by = Some(ada.asker.clone());
        let follow = ChatQuery {
            workspace: id("w1"),
            chat: id("c-1"),
            query: ChatQueryKind::Subscribe { after: 0 },
        };
        assert_eq!(
            sent(&mut attached),
            [Message::ChatCommand(relayed), Message::ChatQuery(follow)]
        );
        let chats = ada.member.workspace.chats().await.unwrap();
        let made: Vec<_> = chats
            .iter()
            .map(|chat| (chat.title.as_str(), chat.machine.as_str()))
            .collect();
        assert_eq!(made, [("Fix it", "Studio")]);
        let again = prompt("r-1", "c-2", "Fix it", Some(&ada.machine));
        let taken = take(&ada.member, &ada.asker, shared, again).await;
        assert_eq!(taken.unwrap_err().code().as_str(), "request.conflict");
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
        match message {
            Message::ChatCommand(ChatCommand {
                action: ChatAction::Prompt { task, .. },
                ..
            }) => task.as_ref(),
            _ => None,
        }
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
        let first = || for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");

        take(&ada.member, &ada.asker, shared, first())
            .await
            .unwrap();
        let stamped = ChatTask {
            id: id("t-1"),
            title: "Speed up the build".to_owned(),
        };
        assert_eq!(task_of(&sent(&mut attached)[0]), Some(&stamped));
        let listed = workspace.chats().await.unwrap();
        assert_eq!(listed[0].task, Some(id("t-1")));
        // Sent again, it is the same command still, not one that conflicts with it.
        let again = take(&ada.member, &ada.asker, shared, first()).await;
        assert_eq!(again.unwrap(), accepted("r-1"));
        let later = for_task(prompt("r-2", "c-1", "And this", None), "t-9");
        take(&ada.member, &ada.asker, shared, later).await.unwrap();
        let plain = prompt("r-3", "c-1", "Then that", None);
        take(&ada.member, &ada.asker, shared, plain).await.unwrap();
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

        let asked = for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");
        let refused = take(&ada.member, &ada.asker, shared, asked).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "task.not_found");
        assert!(sent(&mut attached).is_empty());
        assert_eq!(ada.member.workspace.chat(&id("c-1")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_command_to_a_machine_that_cant_take_it_is_refused_and_not_kept() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["machine.connect"]);

        let refused = take(
            &ada.member,
            &ada.asker,
            shared,
            prompt("r-2", "c-1", "Go", None),
        )
        .await;
        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(!workspace.has_request(&id("r-2")).await.unwrap());
        assert!(sent(&mut attached).is_empty());

        let mut serving = attach(shared, &ada, &["chat.serve"]);
        let taken = take(
            &ada.member,
            &ada.asker,
            shared,
            prompt("r-2", "c-1", "Go", None),
        )
        .await;
        assert_eq!(taken.unwrap(), accepted("r-2"));
        assert!(workspace.has_request(&id("r-2")).await.unwrap());
        assert_eq!(sent(&mut serving).len(), 2);
        let retried = take(
            &ada.member,
            &ada.asker,
            shared,
            prompt("r-2", "c-1", "Go", None),
        )
        .await;
        assert_eq!(retried.unwrap(), accepted("r-2"));
        assert_eq!(sent(&mut serving).len(), 2);
    }

    #[tokio::test]
    async fn a_first_prompt_sent_again_is_taken_as_itself_and_one_racing_its_chat_retries() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        let first = || prompt("r-1", "c-1", "Fix it", Some(&ada.machine));

        let taken = take(&ada.member, &ada.asker, shared, first()).await;
        assert_eq!(taken.unwrap(), accepted("r-1"));
        assert_eq!(sent(&mut serving).len(), 2);
        // Its receipt was lost: the same command again, which the core does once.
        let again = take(&ada.member, &ada.asker, shared, first()).await;
        assert_eq!(again.unwrap(), accepted("r-1"));
        assert_eq!(sent(&mut serving).len(), 2);

        begin(workspace, "c-2", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let racing = prompt("r-c-2", "c-2", "Fix the build", Some(&ada.machine));
        let refused = take(&ada.member, &ada.asker, shared, racing).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(sent(&mut serving).is_empty());
    }

    #[tokio::test]
    async fn a_first_prompt_losing_its_chat_to_one_on_another_machine_is_refused() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        // As admitted for a new chat, its machine taken out.
        let lost = || prompt("r-c-1", "c-1", "Fix the build", None);

        let elsewhere = again(&ada.member, &ada.asker, shared, lost(), id("mc-2")).await;

        assert_eq!(elsewhere.unwrap_err().code().as_str(), "chat.machine_fixed");
        assert!(sent(&mut serving).is_empty());
        let same = again(&ada.member, &ada.asker, shared, lost(), ada.machine.clone()).await;
        same.unwrap();
        assert_eq!(sent(&mut serving).len(), 2);
    }

    #[tokio::test]
    async fn a_chat_whose_machine_is_no_longer_its_owners_is_detached_and_the_feed_hears() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        let before = workspace.changes_since(0, 50).await.unwrap().len();

        let asked = prompt("r-2", "c-1", "Go", None);
        let refused = take(&ada.member, &ada.asker, shared, asked).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "chat.detached");
        let chat = workspace.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(chat.state, weacon_protocol::ChatState::Detached);
        let changes = workspace.changes_since(0, 50).await.unwrap();
        assert_eq!(changes.len(), before + 1);
    }

    #[tokio::test]
    async fn an_action_the_server_doesnt_list_never_reaches_the_machine() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        let mut asked = prompt("r-2", "c-1", "Go", None);
        asked.action = ChatAction::Hold;

        let refused = take(&ada.member, &ada.asker, shared, asked).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
        assert!(sent(&mut serving).is_empty());
        assert!(!workspace.has_request(&id("r-2")).await.unwrap());
        let mut released = prompt("r-3", "c-1", "Go", None);
        released.action = ChatAction::Release;
        let refused = take(&ada.member, &ada.asker, shared, released).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
        assert!(sent(&mut serving).is_empty());
    }
}
