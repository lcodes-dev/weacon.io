//! A member's command to a chat: checked here, then relayed to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{
    Asker, ChatCommand, ChatState, MachineSummary, Message, Outcome, Receipt, RepoId, RequestId,
    code,
};

use super::admit::{Admitted, admit, detached};
use super::link;
use crate::auth::Member;
use crate::db::{Chat, NewChat, NewRequest, Recorded};
use crate::http::Shared;

/// Takes `command` from `asker`, one of the chat's workspace's members, refusing what the
/// chat's owner or machine doesn't allow, and relays it; its receipt says the machine has it.
pub(crate) async fn take(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    command: ChatCommand,
) -> Result<Message, Fault> {
    let chat = member.workspace.chat(&command.chat).await?;
    let owned = shared.db.machines_of(&asker.account).await?;
    let request = match admit(chat, asker.clone(), command)? {
        Admitted::New {
            title,
            machine,
            repo,
            command,
        } => {
            let Some(summary) = owned.iter().find(|summary| summary.id == machine) else {
                return Err(Fault::new(
                    code!("machine.not_found"),
                    "There is no such machine",
                ));
            };
            let made = Made {
                machine: summary,
                title: &title,
                repo: repo.as_ref(),
            };
            start(shared, member, &made, command).await?
        }
        Admitted::Existing { chat, command } => {
            if !owned.iter().any(|summary| summary.id == chat.machine) {
                // A chat whose machine was unpaired can still be read, never run.
                let workspace = &member.workspace;
                workspace
                    .set_chat_state(&command.chat, ChatState::Detached)
                    .await?;
                return Err(detached());
            }
            go_on(shared, member, &chat, command).await?
        }
    };
    Ok(Message::Receipt(Receipt {
        request,
        outcome: Outcome::Accepted,
    }))
}

/// What a first prompt makes its chat with.
struct Made<'a> {
    machine: &'a MachineSummary,
    title: &'a str,
    repo: Option<&'a RepoId>,
}

// A chat's first prompt: the chat is made before it is sent, so two at once can't both reach
// the machine, and taken back when it can't be sent.
async fn start(
    shared: &Shared,
    member: &Member,
    made: &Made<'_>,
    command: ChatCommand,
) -> Result<RequestId, Fault> {
    let (workspace, at) = (&member.workspace, shared.now());
    let chat = NewChat {
        id: &command.chat,
        owner: &member.id,
        title: made.title,
        repo: made.repo,
        machine_id: &made.machine.id,
        machine: &made.machine.name,
        at,
    };
    let asked = NewRequest {
        command: &command,
        member: &member.id,
        at,
    };
    workspace.add_chat(&chat, &asked).await.map_err(|fault| {
        if fault.code().as_str() == "db.exists" {
            conflict()
        } else {
            fault
        }
    })?;
    let machine = &made.machine.id;
    if let Err(fault) = link::send(shared, machine, Message::ChatCommand(command.clone())) {
        workspace.forget_chat(&command.chat).await?;
        return Err(fault);
    }
    workspace.chat_made(&command.chat, &member.id).await?;
    shared.changed(workspace.id());
    // A machine that left meanwhile follows the chat when it connects again.
    drop(link::follow(
        shared,
        machine,
        workspace.id(),
        &command.chat,
        0,
    ));
    Ok(command.request)
}

// A command to a chat that exists: in the ledger before it is sent, and out of it again when
// it can't be, so nothing waits to run later.
async fn go_on(
    shared: &Shared,
    member: &Member,
    chat: &Chat,
    command: ChatCommand,
) -> Result<RequestId, Fault> {
    let workspace = &member.workspace;
    let asked = NewRequest {
        command: &command,
        member: &member.id,
        at: shared.now(),
    };
    let recorded = workspace.record_request(&asked).await?;
    match recorded {
        Recorded::Conflict => return Err(conflict()),
        // Its receipt has ended it: nothing more to send.
        Recorded::Same { open: false } => return Ok(command.request),
        Recorded::New | Recorded::Same { open: true } => {}
    }
    if let Err(fault) = link::send(shared, &chat.machine, Message::ChatCommand(command.clone())) {
        if recorded == Recorded::New {
            workspace.forget_request(&command.request).await?;
        }
        return Err(fault);
    }
    drop(link::follow(
        shared,
        &chat.machine,
        workspace.id(),
        &command.chat,
        chat.core_seq,
    ));
    Ok(command.request)
}

fn conflict() -> Fault {
    Fault::new(
        code!("request.conflict"),
        "That request id was used for another command",
    )
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AgentKind, ChatAction, ChatQuery, ChatQueryKind};

    use super::*;
    use crate::chats::test_support::{ada, attach, sent};
    use crate::db::test_support::{NOW, id, make, prompt};
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
}
