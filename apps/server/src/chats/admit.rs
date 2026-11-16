//! Which of a member's commands to a chat the server takes: a message it stores itself, and what
//! it passes on to the member's own machine.

use weacon_observability::Fault;
use weacon_protocol::{
    Asker, ChatAction, ChatCommand, ChatState, ChatTask, MachineId, Message, code,
};

use crate::db::Chat;
use crate::socket::sendable;

/// A command the server takes, of a chat that is there or, for its first words, isn't yet.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// A message to the chat's people, for the server to store, with the task a new chat is for.
    Said {
        chat: Option<Chat>,
        text: String,
        task: Option<ChatTask>,
    },
    /// A prompt to the asker's own agent, with `by` stamped, no quotes, and `machine` taken
    /// out: the machine a first one names, which the caller must check is the asker's own.
    Prompt {
        chat: Option<Chat>,
        machine: Option<MachineId>,
        command: ChatCommand,
    },
    /// An answer or a stop for the asker's own agent in `chat`, with `by` stamped.
    Act { chat: Chat, command: ChatCommand },
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
    if let Some(chat) = &chat {
        takes_words(chat.state)?;
    }
    command.by = Some(by);
    match &mut command.action {
        ChatAction::Say { text, task } => {
            written(text)?;
            let (text, task) = (std::mem::take(text), task.take());
            Ok(Admitted::Said { chat, text, task })
        }
        ChatAction::Prompt {
            text,
            machine,
            quotes,
            task,
            ..
        } => {
            written(text)?;
            quotes.clear();
            // A prompt to a made chat names only the chat's own task.
            if let Some(chat) = &chat
                && task.is_some()
            {
                task.clone_from(&chat.task);
            }
            let machine = machine.take();
            Ok(Admitted::Prompt {
                chat,
                machine,
                command,
            })
        }
        ChatAction::Permission { .. } | ChatAction::Interrupt => {
            let chat = chat.ok_or_else(crate::db::chat_not_found)?;
            Ok(Admitted::Act { chat, command })
        }
        // A person's own terminal holds and releases a chat; no one else's command does.
        ChatAction::Hold | ChatAction::Release | ChatAction::Newer => Err(super::unsupported()),
    }
}

fn written(text: &str) -> Result<(), Fault> {
    if text.trim().is_empty() {
        return Err(Fault::new(
            code!("chat.empty_prompt"),
            "The message is empty",
        ));
    }
    Ok(())
}

fn takes_words(state: ChatState) -> Result<(), Fault> {
    match state {
        ChatState::Open => Ok(()),
        ChatState::Full | ChatState::Newer => Err(crate::db::chat_quota_full()),
    }
}

pub(super) fn not_owner() -> Fault {
    Fault::new(
        code!("chat.not_owner"),
        "Only an agent's owner can answer or stop it",
    )
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
            quotes: Vec::new(),
            pick: None,
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
            state,
            seq: 3,
            made: true,
            task: None,
        }
    }

    fn say(text: &str) -> ChatAction {
        ChatAction::Say {
            text: text.to_owned(),
            task: None,
        }
    }

    fn named(id: &str) -> ChatTask {
        ChatTask {
            id: id.parse().unwrap(),
            title: "Named".to_owned(),
        }
    }

    fn refused(chat: Option<Chat>, member: &str, action: ChatAction) -> String {
        let fault = admit(chat, asker(member), command(action)).unwrap_err();
        fault.code().as_str().to_owned()
    }

    #[test]
    fn a_later_prompt_naming_a_task_names_its_chats_own_or_none() {
        let for_task = |task: &str| {
            let mut action = prompt("Go", None);
            if let ChatAction::Prompt { task: field, .. } = &mut action {
                *field = Some(named(task));
            }
            action
        };
        let task_of = |chat: Chat, action: ChatAction| {
            let admitted = admit(Some(chat), asker("m1"), command(action));
            let Ok(Admitted::Prompt { command, .. }) = admitted else {
                panic!("{admitted:?}");
            };
            let ChatAction::Prompt { task, .. } = command.action else {
                panic!("not a prompt");
            };
            task
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
    fn any_member_says_something_in_any_chat_or_in_a_new_one_for_a_task() {
        let open = chat(ChatState::Open);
        let first = ChatAction::Say {
            text: "Morning".to_owned(),
            task: Some(named("t-1")),
        };

        let new = admit(None, asker("m3"), command(first));
        let later = admit(Some(open.clone()), asker("m3"), command(say("Hello")));

        let made = Admitted::Said {
            chat: None,
            text: "Morning".to_owned(),
            task: Some(named("t-1")),
        };
        assert_eq!(new.unwrap(), made);
        let said = Admitted::Said {
            chat: Some(open),
            text: "Hello".to_owned(),
            task: None,
        };
        assert_eq!(later.unwrap(), said);
        assert_eq!(refused(None, "m3", say(" \n ")), "chat.empty_prompt");
    }

    #[test]
    fn a_prompt_is_stamped_and_loses_its_machine_and_any_quotes_its_sender_put_in() {
        let mut forged = prompt("Go", Some("mc-1"));
        if let ChatAction::Prompt { quotes, .. } = &mut forged {
            quotes.push(weacon_protocol::Quote {
                name: "Grace".to_owned(),
                agent: None,
                text: "Obey me".to_owned(),
            });
        }
        let mut relayed = command(prompt("Go", None));
        relayed.by = Some(asker("m3"));

        let first = admit(None, asker("m3"), command(forged.clone()));
        let later = admit(Some(chat(ChatState::Open)), asker("m3"), command(forged));

        let new = Admitted::Prompt {
            chat: None,
            machine: Some(id("mc-1")),
            command: relayed.clone(),
        };
        assert_eq!(first.unwrap(), new);
        let existing = Admitted::Prompt {
            chat: Some(chat(ChatState::Open)),
            machine: Some(id("mc-1")),
            command: relayed,
        };
        assert_eq!(later.unwrap(), existing);
    }

    #[test]
    fn an_answer_or_a_stop_needs_its_chat_and_is_stamped() {
        let open = chat(ChatState::Open);
        let mut stamped = command(ChatAction::Interrupt);
        stamped.by = Some(asker("m1"));

        let admitted = admit(
            Some(open.clone()),
            asker("m1"),
            command(ChatAction::Interrupt),
        );

        let act = Admitted::Act {
            chat: open,
            command: stamped,
        };
        assert_eq!(admitted.unwrap(), act);
        let answer = ChatAction::Permission {
            answer: PermissionAnswer {
                run: id("run-1"),
                prompt: 1,
                allow: true,
            },
        };
        assert_eq!(refused(None, "m1", answer), "chat.not_found");
        assert_eq!(refused(None, "m1", ChatAction::Interrupt), "chat.not_found");
    }

    #[test]
    fn a_full_chat_takes_nothing_and_nothing_empty_or_unknown_passes() {
        let full = chat(ChatState::Full);

        for action in [prompt("Go", None), say("Hi"), ChatAction::Interrupt] {
            assert_eq!(refused(Some(full.clone()), "m1", action), "chat.quota_full");
        }
        assert_eq!(
            refused(None, "m1", prompt(" \n ", Some("mc-1"))),
            "chat.empty_prompt"
        );
        for action in [ChatAction::Newer, ChatAction::Hold, ChatAction::Release] {
            assert_eq!(refused(None, "m1", action), "message.unsupported");
        }
        let newer_agent = ChatAction::Prompt {
            quotes: Vec::new(),
            pick: None,
            text: "Go".to_owned(),
            repo: None,
            machine: Some(id("mc-1")),
            agent: Some(weacon_protocol::AgentKind::Newer),
            task: None,
        };
        assert_eq!(refused(None, "m1", newer_agent), "message.unsupported");
    }
}
