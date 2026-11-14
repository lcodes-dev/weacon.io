//! Which of a member's commands to a chat the server passes on to the chat's machine.

use weacon_observability::Fault;
use weacon_protocol::{Asker, ChatAction, ChatCommand, ChatState, MachineId, code};

use crate::db::Chat;

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
        command: ChatCommand,
    },
    /// A command to a chat the asker owns.
    Existing(ChatCommand),
}

/// Checks `command` from `by` against `chat`, the chat it names if there is one.
pub(crate) fn admit(
    chat: Option<&Chat>,
    by: Asker,
    mut command: ChatCommand,
) -> Result<Admitted, Fault> {
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
        ChatAction::Newer => {
            return Err(Fault::new(
                code!("message.unsupported"),
                "This build doesn't know that message",
            ));
        }
    };
    let Some(chat) = chat else {
        let ChatAction::Prompt { text, .. } = &command.action else {
            return Err(crate::db::chat_not_found());
        };
        let machine = machine.ok_or_else(|| {
            Fault::new(code!("chat.machine_required"), "A new chat needs a machine")
        })?;
        let title = title(text);
        command.by = Some(by);
        return Ok(Admitted::New {
            title,
            machine,
            command,
        });
    };
    if chat.owner != by.member {
        return Err(not_owner());
    }
    if machine.is_some() {
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
    command.by = Some(by);
    Ok(Admitted::Existing(command))
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

    fn id<T: TryFrom<String>>(text: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(text.to_owned()).unwrap()
    }

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
        }
    }

    fn chat(state: ChatState) -> Chat {
        Chat {
            owner: id("m1"),
            machine: id("mc-1"),
            state,
            seq: 3,
            core_seq: 3,
        }
    }

    fn refused(chat: Option<&Chat>, member: &str, action: ChatAction) -> String {
        let fault = admit(chat, asker(member), command(action)).unwrap_err();
        fault.code().as_str().to_owned()
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

        let admitted = admit(Some(&open), asker("m1"), command(ChatAction::Interrupt));
        assert_eq!(admitted.unwrap(), Admitted::Existing(stamped));
        let answer = ChatAction::Permission {
            answer: PermissionAnswer {
                run: id("run-1"),
                prompt: 1,
                allow: true,
            },
        };
        assert_eq!(refused(Some(&open), "m3", answer), "chat.not_owner");
        assert_eq!(
            refused(Some(&open), "m3", prompt("Go", None)),
            "chat.not_owner"
        );
        assert_eq!(
            refused(Some(&open), "m1", prompt("Go", Some("mc-2"))),
            "chat.machine_fixed"
        );
        let later = admit(Some(&open), asker("m1"), command(prompt("Go", None)));
        assert!(matches!(later.unwrap(), Admitted::Existing(_)));
    }

    #[test]
    fn a_full_or_detached_chat_takes_no_commands_and_nothing_empty_or_unknown_passes() {
        let full = chat(ChatState::Full);
        let detached = chat(ChatState::Detached);

        assert_eq!(
            refused(Some(&full), "m1", prompt("Go", None)),
            "chat.quota_full"
        );
        assert_eq!(
            refused(Some(&detached), "m1", ChatAction::Interrupt),
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
    }
}
