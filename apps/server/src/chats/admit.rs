//! Which of a member's commands to a chat the server passes on to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{
    Asker, ChatAction, ChatCommand, ChatState, MachineId, Message, RepoId, code,
};

use crate::db::Chat;
use crate::socket::sendable;

/// The most characters of a chat's title, which is the first line of its first prompt.
const TITLE_MAX: usize = 80;

/// A command the server may relay, with `by` stamped and `machine` taken out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// The first prompt, which makes the chat on `machine`, one the caller must check is the
    /// asker's own.
    New {
        title: String,
        machine: MachineId,
        repo: Option<RepoId>,
        command: ChatCommand,
    },
    /// A command to `chat`, which the asker owns.
    Existing { chat: Chat, command: ChatCommand },
}

/// Checks `command` from `by` against `chat`, the chat it names if there is one.
pub(crate) fn admit(
    chat: Option<Chat>,
    by: Asker,
    command: ChatCommand,
) -> Result<Admitted, Fault> {
    let Some(Message::ChatCommand(mut command)) = sendable(Message::ChatCommand(command)) else {
        return Err(super::unsupported());
    };
    let machine = match &mut command.action {
        ChatAction::Prompt { text, machine, .. } => {
            if text.trim().is_empty() {
                return Err(Fault::new(
                    code!("chat.empty_prompt"),
                    "The prompt is empty",
                ));
            }
            machine.take()
        }
        ChatAction::Permission { .. } | ChatAction::Interrupt => None,
        // A person's own terminal holds and releases a chat; no one else's command does.
        ChatAction::Hold | ChatAction::Release | ChatAction::Newer => {
            return Err(super::unsupported());
        }
    };
    let Some(chat) = chat else {
        let ChatAction::Prompt { text, repo, .. } = &command.action else {
            return Err(crate::db::chat_not_found());
        };
        let machine = machine.ok_or_else(|| {
            Fault::new(code!("chat.machine_required"), "A new chat needs a machine")
        })?;
        let (title, repo) = (title(text), repo.clone());
        command.by = Some(by);
        return Ok(Admitted::New {
            title,
            machine,
            repo,
            command,
        });
    };
    if chat.owner != by.member {
        return Err(not_owner());
    }
    // A first prompt sent again names the machine its chat is on.
    if machine.is_some_and(|machine| machine != chat.machine) {
        return Err(Fault::new(
            code!("chat.machine_fixed"),
            "A chat stays on the machine it started on",
        ));
    }
    match chat.state {
        ChatState::Open => {}
        ChatState::Full => return Err(crate::db::chat_quota_full()),
        ChatState::Detached | ChatState::Newer => return Err(detached()),
    }
    // A prompt to a made chat names only the chat's own task, as a first prompt sent again does.
    if let ChatAction::Prompt { task, .. } = &mut command.action
        && task.is_some()
    {
        task.clone_from(&chat.task);
    }
    command.by = Some(by);
    Ok(Admitted::Existing { chat, command })
}

pub(super) fn not_owner() -> Fault {
    Fault::new(
        code!("chat.not_owner"),
        "Only the chat's owner can ask its agent",
    )
}

pub(super) fn detached() -> Fault {
    Fault::new(
        code!("chat.detached"),
        "The chat can be read but no longer run",
    )
}

fn title(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim_end();
    line.chars().take(TITLE_MAX).collect()
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{AccountId, ChatId, MemberId, PermissionAnswer, RequestId, WorkspaceId};

    use super::*;
    use crate::db::test_support::id;

    fn asker(member: &str) -> Asker {
        Asker {
            account: id::<AccountId>("a-1"),
            member: id::<MemberId>(member),
        }
    }

    fn command(action: ChatAction) -> ChatCommand {
        ChatCommand {
            request: id::<RequestId>("r-1"),
            workspace: id::<WorkspaceId>("w1"),
            chat: id::<ChatId>("c-1"),
            action,
            by: Some(asker("forged")),
        }
    }

    fn prompt(text: &str, machine: Option<&str>) -> ChatAction {
        ChatAction::Prompt {
            text: text.to_owned(),
            repo: None,
            machine: machine.map(id),
            agent: None,
            task: None,
        }
    }

    fn chat(state: ChatState) -> Chat {
        Chat {
            owner: id("m1"),
            machine: id("mc-1"),
            state,
            seq: 3,
            core_seq: 3,
            made: true,
            task: None,
        }
    }

    fn refused(chat: Option<Chat>, member: &str, action: ChatAction) -> String {
        let fault = admit(chat, asker(member), command(action)).unwrap_err();
        fault.code().as_str().to_owned()
    }

    #[test]
    fn a_later_prompt_naming_a_task_names_its_chats_own_or_none() {
        let named = |id: &str| weacon_protocol::ChatTask {
            id: id.parse().unwrap(),
            title: "Named".to_owned(),
        };
        let for_task = |task: &str| {
            let mut action = prompt("Go", None);
            if let ChatAction::Prompt { task: field, .. } = &mut action {
                *field = Some(named(task));
            }
            action
        };
        let task_of = |chat: Chat, action: ChatAction| match admit(
            Some(chat),
            asker("m1"),
            command(action),
        ) {
            Ok(Admitted::Existing { command, .. }) => match command.action {
                ChatAction::Prompt { task, .. } => task,
                _ => panic!("not a prompt"),
            },
            other => panic!("{other:?}"),
        };
        let with_task = Chat {
            task: Some(named("t-1")),
            ..chat(ChatState::Open)
        };

        assert_eq!(
            task_of(with_task.clone(), for_task("t-2")),
            Some(named("t-1"))
        );
        assert_eq!(task_of(with_task, prompt("Go", None)), None);
        assert_eq!(task_of(chat(ChatState::Open), for_task("t-2")), None);
    }

    #[test]
    fn a_first_prompt_makes_the_chat_on_its_machine_titled_by_its_first_line() {
        let text = format!("  {}\nand more", "x".repeat(90));
        let admitted = admit(None, asker("m1"), command(prompt(&text, Some("mc-1"))));

        let mut relayed = command(prompt(&text, None));
        relayed.by = Some(asker("m1"));
        let made = Admitted::New {
            title: "x".repeat(80),
            machine: id("mc-1"),
            repo: None,
            command: relayed,
        };
        assert_eq!(admitted.unwrap(), made);
        let short = admit(
            None,
            asker("m1"),
            command(prompt("Fix it \n", Some("mc-1"))),
        );
        assert!(matches!(short.unwrap(), Admitted::New { title, .. } if title == "Fix it"));
        assert_eq!(
            refused(None, "m1", prompt("Go", None)),
            "chat.machine_required"
        );
        assert_eq!(refused(None, "m1", ChatAction::Interrupt), "chat.not_found");
    }

    #[test]
    fn only_the_owner_asks_an_open_chat_and_its_machine_stays() {
        let open = chat(ChatState::Open);
        let mut stamped = command(ChatAction::Interrupt);
        stamped.by = Some(asker("m1"));

        let admitted = admit(
            Some(open.clone()),
            asker("m1"),
            command(ChatAction::Interrupt),
        );
        let existing = Admitted::Existing {
            chat: open.clone(),
            command: stamped,
        };
        assert_eq!(admitted.unwrap(), existing);
        let answer = ChatAction::Permission {
            answer: PermissionAnswer {
                run: id("run-1"),
                prompt: 1,
                allow: true,
            },
        };
        assert_eq!(refused(Some(open.clone()), "m3", answer), "chat.not_owner");
        assert_eq!(
            refused(Some(open.clone()), "m3", prompt("Go", None)),
            "chat.not_owner"
        );
        assert_eq!(
            refused(Some(open.clone()), "m1", prompt("Go", Some("mc-2"))),
            "chat.machine_fixed"
        );
        let again = admit(
            Some(open.clone()),
            asker("m1"),
            command(prompt("Go", Some("mc-1"))),
        );
        assert!(matches!(again.unwrap(), Admitted::Existing { .. }));
        let later = admit(Some(open), asker("m1"), command(prompt("Go", None)));
        assert!(matches!(later.unwrap(), Admitted::Existing { .. }));
    }

    #[test]
    fn a_full_or_detached_chat_takes_no_commands_and_nothing_empty_or_unknown_passes() {
        let full = chat(ChatState::Full);
        let detached = chat(ChatState::Detached);

        assert_eq!(
            refused(Some(full), "m1", prompt("Go", None)),
            "chat.quota_full"
        );
        assert_eq!(
            refused(Some(detached), "m1", ChatAction::Interrupt),
            "chat.detached"
        );
        assert_eq!(
            refused(None, "m1", prompt(" \n ", Some("mc-1"))),
            "chat.empty_prompt"
        );
        assert_eq!(
            refused(None, "m1", ChatAction::Newer),
            "message.unsupported"
        );
        let newer_agent = ChatAction::Prompt {
            text: "Go".to_owned(),
            repo: None,
            machine: Some(id("mc-1")),
            agent: Some(weacon_protocol::AgentKind::Newer),
            task: None,
        };
        assert_eq!(refused(None, "m1", newer_agent), "message.unsupported");
    }
}
