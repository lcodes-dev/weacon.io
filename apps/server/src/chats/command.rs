//! A member's command to a chat: checked here, then relayed to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{
    Asker, ChatAction, ChatCommand, ChatState, MachineSummary, Message, Outcome, Receipt,
    RequestId, code,
};

use super::admit::{Admitted, admit, detached};
use super::link;
use crate::auth::Member;
use crate::db::{Chat, NewChat, NewRequest, Recorded, chat_not_found};
use crate::http::Shared;

/// Takes `command` from `member`, refusing what the chat's owner or machine doesn't allow,
/// and relays it; its receipt says the machine has it.
pub(crate) async fn take(
    member: &Member,
    asker: &Asker,
    shared: &Shared,
    command: ChatCommand,
) -> Result<Option<Message>, Fault> {
    let workspace = &member.workspace;
    // Another workspace's chat answers as one there isn't.
    if command.workspace != *workspace.id() {
        return Err(chat_not_found());
    }
    let chat = workspace.chat(&command.chat).await?;
    let admitted = admit(chat.as_ref(), asker.clone(), command)?;
    let owned = shared.db.machines_of(&asker.account).await?;
    let request = match (admitted, chat) {
        (
            Admitted::New {
                title,
                machine,
                command,
            },
            _,
        ) => {
            let Some(summary) = owned.iter().find(|summary| summary.id == machine) else {
                return Err(Fault::new(
                    code!("machine.not_found"),
                    "There is no such machine",
                ));
            };
            start(shared, member, summary, &title, command).await?
        }
        (Admitted::Existing(command), Some(chat)) => {
            if !owned.iter().any(|summary| summary.id == chat.machine) {
                // A chat whose machine was unpaired can still be read, never run.
                workspace
                    .set_chat_state(&command.chat, ChatState::Detached)
                    .await?;
                return Err(detached());
            }
            go_on(shared, member, &chat, command).await?
        }
        (Admitted::Existing(_), None) => return Err(chat_not_found()),
    };
    Ok(Some(Message::Receipt(Receipt {
        request,
        outcome: Outcome::Accepted,
    })))
}

// The chat's first prompt: sent, then the chat is made and followed, so none of its events
// arrive before it exists.
async fn start(
    shared: &Shared,
    member: &Member,
    machine: &MachineSummary,
    title: &str,
    command: ChatCommand,
) -> Result<RequestId, Fault> {
    let workspace = &member.workspace;
    if workspace.has_request(&command.request).await? {
        return Err(conflict());
    }
    link::send(shared, &machine.id, Message::ChatCommand(command.clone()))?;
    let repo = if let ChatAction::Prompt { repo, .. } = &command.action {
        repo.as_ref()
    } else {
        None
    };
    let at = shared.now();
    let chat = NewChat {
        id: &command.chat,
        owner: &member.id,
        title,
        repo,
        machine_id: &machine.id,
        machine: &machine.name,
        at,
    };
    let asked = NewRequest {
        command: &command,
        member: &member.id,
        at,
    };
    workspace.add_chat(&chat, &asked).await?;
    // Followed from its start; a machine that left meanwhile is followed when it connects.
    drop(link::follow(
        shared,
        &machine.id,
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
    use weacon_protocol::{ChatQuery, ChatQueryKind, MachineId};

    use super::*;
    use crate::chats::test_support::{ada, attach, sent};
    use crate::db::test_support::{NOW, id, make};
    use crate::test_support::Fixture;

    fn prompt(request: &str, chat: &str, text: &str, machine: Option<&MachineId>) -> ChatCommand {
        ChatCommand {
            request: id(request),
            workspace: id("w1"),
            chat: id(chat),
            action: ChatAction::Prompt {
                text: text.to_owned(),
                repo: None,
                machine: machine.cloned(),
            },
            by: None,
        }
    }

    fn accepted(request: &str) -> Option<Message> {
        Some(Message::Receipt(Receipt {
            request: id(request),
            outcome: Outcome::Accepted,
        }))
    }

    #[tokio::test]
    async fn a_first_prompt_reaches_the_machine_stamped_then_the_chat_is_made_and_followed() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let mut attached = attach(shared, &ada, &["machine.connect", "chat.serve"]);

        let asked = prompt("r-1", "c-1", "Fix it", Some(&ada.machine));
        let answer = take(&ada.member, &ada.asker, shared, asked).await.unwrap();

        assert_eq!(answer, accepted("r-1"));
        let mut relayed = prompt("r-1", "c-1", "Fix it", None);
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
