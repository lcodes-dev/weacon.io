//! A member's command to a chat: checked here, then relayed to the chat's machine, whose
//! answer is the command's receipt.

use weacon_observability::Fault;
use weacon_protocol::{Asker, ChatAction, ChatCommand, ChatTask};

use super::admit::{Admitted, admit, detached};
use super::link;
use crate::auth::Member;
use crate::db::{Chat, NewChat, Workspace};
use crate::relay::{self, Back};
use crate::shared::Shared;

/// Takes `command` from `asker`, one of the chat's workspace's members, refusing what the
/// chat's owner or machine doesn't allow, and relays it once; the machine's answer goes `back`.
pub(crate) async fn take(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
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
            let id = command.chat.clone();
            let chat = NewChat {
                id: &id,
                owner: &member.id,
                title: &title,
                repo: repo.as_ref(),
                machine_id: &summary.id,
                machine: &summary.name,
                at: shared.now(),
                task: task.as_ref(),
            };
            start(shared, member, &chat, command, back).await
        }
        Admitted::Existing { chat, command } => {
            existing(shared, member, &chat, command, back).await
        }
    }
}

// A chat's first prompt: the chat is made before it is sent, so two at once can't both reach
// the machine, and taken back when it can't be sent.
async fn start(
    shared: &Shared,
    member: &Member,
    chat: &NewChat<'_>,
    command: ChatCommand,
    back: Back<'_>,
) -> Result<(), Fault> {
    let workspace = &member.workspace;
    workspace.add_chat(chat).await?;
    let follow = link::follow(workspace.id(), chat.id, 0);
    if let Err(fault) = relay::command(shared, chat.machine_id, command, follow, back) {
        workspace.forget_chat(chat.id).await?;
        return Err(fault);
    }
    workspace.chat_made(chat.id, &member.id).await?;
    shared.wake_feeds(workspace.id());
    Ok(())
}

// A command to a chat that exists, on a machine its owner still owns.
async fn existing(
    shared: &Shared,
    member: &Member,
    chat: &Chat,
    command: ChatCommand,
    back: Back<'_>,
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
    let follow = link::follow(workspace.id(), &command.chat, chat.core_seq);
    relay::command(shared, &chat.machine, command, follow, back)
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
    workspace
        .task(&named.id)
        .await?
        .title
        .as_str()
        .clone_into(&mut named.title);
    Ok(Some(named.clone()))
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{
        AgentKind, ChatAction, ChatQuery, ChatQueryKind, ErrorBody, Message, Outcome, Receipt, code,
    };

    use tokio::sync::mpsc;
    use weacon_protocol::Envelope;

    use super::*;
    use crate::chats::test_support::{Ada, ada, attach, sent};
    use crate::db::test_support::{NOW, id, make, prompt};
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
    ) -> (Result<(), Fault>, mpsc::Receiver<Envelope>) {
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
        let made: Vec<_> = chats
            .iter()
            .map(|chat| (chat.title.as_str(), chat.machine.as_str()))
            .collect();
        assert_eq!(made, [("Fix it", "Studio")]);
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
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

        let foreign = for_task(prompt("r-1", "c-1", "Fix it", Some(&ada.machine)), "t-1");
        let (refused, _answers) = asked(&ada, shared, foreign).await;

        assert_eq!(refused.unwrap_err().code().as_str(), "task.not_found");
        assert!(sent(&mut attached).is_empty());
        assert_eq!(ada.member.workspace.chat(&id("c-1")).await.unwrap(), None);
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
        let (refused, _answers) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;
        assert_eq!(refused.unwrap_err().code().as_str(), "machine.offline");
        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn a_command_sent_again_goes_again_and_each_answer_returns_to_its_own_ask() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);

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
                .any(|envelope| envelope.message == follow(0))
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
    async fn a_chat_whose_machine_is_no_longer_its_owners_is_detached_and_the_feed_hears() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        let before = workspace.changes_since(0, 50).await.unwrap().len();

        let (refused, _answers) = asked(&ada, shared, prompt("r-2", "c-1", "Go", None)).await;

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
        for action in [ChatAction::Hold, ChatAction::Release] {
            let mut command = prompt("r-2", "c-1", "Go", None);
            command.action = action;

            let (refused, _answers) = asked(&ada, shared, command).await;

            assert_eq!(refused.unwrap_err().code().as_str(), "message.unsupported");
            assert!(sent(&mut serving).is_empty());
        }
    }
}
