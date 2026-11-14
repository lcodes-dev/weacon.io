//! The made-up worktree of the fake chat `c-1`: it has uncommitted work and unpushed commits,
//! goes to the Trash and comes back, and pushes its branch to a made-up compare page. Any other
//! chat has no worktree, and this core opens no terminals.

use weacon_protocol::{
    ChatId, Code, DownloadUrl, Envelope, ErrorBody, Message, WorktreeAction, WorktreeAnswer,
    WorktreeAnswerKind, WorktreeQueryKind, code,
};

const CHAT: &str = "c-1";
const COMPARE: &str = "https://github.com/example/shopping-list/compare/weacon-c-1?expand=1";

/// Whether the worktree is in the Trash.
#[derive(Debug, Default)]
pub(crate) struct FakeWorktree {
    removed: bool,
}

impl FakeWorktree {
    /// The answer to a worktree query or command in `envelope`, under its id; none for any other
    /// message.
    pub(crate) fn answer(&mut self, envelope: &Envelope) -> Option<Envelope> {
        let (asked, answer) = if let Message::WorktreeQuery(query) = &envelope.message {
            let asked = (&query.request, &query.workspace, &query.chat);
            (asked, self.query(&query.chat, &query.query)?)
        } else if let Message::WorktreeCommand(command) = &envelope.message {
            let asked = (&command.request, &command.workspace, &command.chat);
            (asked, self.act(&command.chat, &command.action)?)
        } else {
            return None;
        };
        let (request, workspace, chat) = asked;
        let message = match answer {
            Ok(answer) => Message::WorktreeAnswer(WorktreeAnswer {
                request: request.clone(),
                workspace: workspace.clone(),
                chat: chat.clone(),
                answer,
            }),
            Err(code) => Message::Error(ErrorBody {
                code,
                retryable: false,
            }),
        };
        Some(Envelope {
            id: envelope.id,
            message,
        })
    }

    fn query(&self, chat: &ChatId, query: &WorktreeQueryKind) -> Option<Answer> {
        if !matches!(query, WorktreeQueryKind::State) {
            return None;
        }
        Some(self.state(chat))
    }

    fn act(&mut self, chat: &ChatId, action: &WorktreeAction) -> Option<Answer> {
        if chat.as_str() != CHAT {
            return Some(Err(code!("worktree.none")));
        }
        match action {
            WorktreeAction::Remove => self.removed = true,
            WorktreeAction::Restore => self.removed = false,
            WorktreeAction::OpenPullRequest { .. } => {
                let url = DownloadUrl::try_from(COMPARE.to_owned()).ok()?;
                return Some(Ok(WorktreeAnswerKind::PullRequestPage { url }));
            }
            WorktreeAction::OpenTerminal => return Some(Err(code!("request.unknown"))),
            WorktreeAction::Newer => return None,
        }
        Some(self.state(chat))
    }

    fn state(&self, chat: &ChatId) -> Answer {
        if chat.as_str() != CHAT {
            return Err(code!("worktree.none"));
        }
        Ok(WorktreeAnswerKind::State {
            removed: self.removed,
            uncommitted: true,
            unpushed: true,
        })
    }
}

type Answer = Result<WorktreeAnswerKind, Code>;

#[cfg(test)]
mod tests {
    use weacon_protocol::{RequestId, WorkspaceId, WorktreeCommand, WorktreeQuery};

    use super::*;

    fn asked(
        chat: &str,
        message: impl FnOnce(RequestId, WorkspaceId, ChatId) -> Message,
    ) -> Envelope {
        Envelope {
            id: 9,
            message: message(
                RequestId::try_from("q-1".to_owned()).unwrap(),
                WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                ChatId::try_from(chat.to_owned()).unwrap(),
            ),
        }
    }

    fn state(chat: &str) -> Envelope {
        asked(chat, |request, workspace, chat| {
            Message::WorktreeQuery(WorktreeQuery {
                request,
                workspace,
                chat,
                query: WorktreeQueryKind::State,
                by: None,
            })
        })
    }

    fn command(chat: &str, action: WorktreeAction) -> Envelope {
        asked(chat, |request, workspace, chat| {
            Message::WorktreeCommand(WorktreeCommand {
                request,
                workspace,
                chat,
                action,
                by: None,
            })
        })
    }

    // What the answer says, as the id it came under and a short line.
    fn told(answer: Option<Envelope>) -> (u32, String) {
        let answer = answer.unwrap();
        if let Message::Error(ErrorBody { code, retryable }) = &answer.message {
            assert!(!retryable);
            return (answer.id, format!("error {}", code.as_str()));
        }
        let Message::WorktreeAnswer(told) = answer.message else {
            panic!("not a worktree answer");
        };
        assert_eq!(
            (
                told.request.as_str(),
                told.workspace.as_str(),
                told.chat.as_str()
            ),
            ("q-1", "w-1", "c-1")
        );
        (answer.id, format!("{:?}", told.answer))
    }

    const IN_PLACE: &str = "State { removed: false, uncommitted: true, unpushed: true }";
    const IN_TRASH: &str = "State { removed: true, uncommitted: true, unpushed: true }";

    #[test]
    fn the_worktree_goes_to_the_trash_and_back_and_a_repeat_answers_the_same() {
        let mut fake = FakeWorktree::default();

        assert_eq!(told(fake.answer(&state("c-1"))), (9, IN_PLACE.to_owned()));
        assert_eq!(
            told(fake.answer(&command("c-1", WorktreeAction::Remove))),
            (9, IN_TRASH.to_owned())
        );
        assert_eq!(
            told(fake.answer(&command("c-1", WorktreeAction::Remove))),
            (9, IN_TRASH.to_owned())
        );
        assert_eq!(told(fake.answer(&state("c-1"))), (9, IN_TRASH.to_owned()));
        assert_eq!(
            told(fake.answer(&command("c-1", WorktreeAction::Restore))),
            (9, IN_PLACE.to_owned())
        );
        assert_eq!(
            told(fake.answer(&command("c-1", WorktreeAction::Restore))),
            (9, IN_PLACE.to_owned())
        );
    }

    #[test]
    fn a_pull_request_gets_the_compare_page_and_a_terminal_is_refused() {
        let mut fake = FakeWorktree::default();
        let pull = WorktreeAction::OpenPullRequest {
            title: "Merge the lists".to_owned(),
            body: String::new(),
        };

        assert_eq!(
            told(fake.answer(&command("c-1", pull))).1,
            format!("PullRequestPage {{ url: DownloadUrl({COMPARE:?}) }}")
        );
        assert_eq!(
            told(fake.answer(&command("c-1", WorktreeAction::OpenTerminal))),
            (9, "error request.unknown".to_owned())
        );
        assert_eq!(fake.answer(&command("c-1", WorktreeAction::Newer)), None);
        assert_eq!(told(fake.answer(&state("c-1"))).1, IN_PLACE);
    }

    #[test]
    fn another_chat_has_no_worktree_and_other_messages_get_nothing() {
        let mut fake = FakeWorktree::default();
        let newer = asked("c-1", |request, workspace, chat| {
            Message::WorktreeQuery(WorktreeQuery {
                request,
                workspace,
                chat,
                query: WorktreeQueryKind::Newer,
                by: None,
            })
        });

        assert_eq!(
            told(fake.answer(&state("c-2"))),
            (9, "error worktree.none".to_owned())
        );
        assert_eq!(
            told(fake.answer(&command("c-2", WorktreeAction::Remove))),
            (9, "error worktree.none".to_owned())
        );
        assert_eq!(told(fake.answer(&state("c-1"))).1, IN_PLACE);
        assert_eq!(fake.answer(&newer), None);
        assert_eq!(
            fake.answer(&Envelope {
                id: 1,
                message: Message::Unknown
            }),
            None
        );
    }
}
