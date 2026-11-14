//! The made-up worktree of the fake chat `c-1`: it has uncommitted work and unpushed commits,
//! goes to the Trash and comes back, and pushes its branch to a made-up compare page. Any other
//! chat has no worktree, and this core opens no terminals.

use weacon_protocol::{
    ChatId, Code, DownloadUrl, Envelope, ErrorBody, Message, RequestId, WorkspaceId,
    WorktreeAction, WorktreeAnswer, WorktreeAnswerKind, WorktreeQueryKind, code,
};

const CHAT: &str = "c-1";
const COMPARE: &str = "https://github.com/example/shopping-list/compare/weacon-c-1?expand=1";

type Answer = Result<WorktreeAnswerKind, Code>;

/// Whether the worktree is in the Trash.
#[derive(Debug, Default)]
pub(crate) struct FakeWorktree {
    removed: bool,
}

impl FakeWorktree {
    /// The answer to a worktree query or command in `envelope`, under its id; none for any other
    /// message, or a query or action this build doesn't know.
    pub(crate) fn answer(&mut self, envelope: &Envelope) -> Option<Envelope> {
        let (asked, answer) = if let Message::WorktreeQuery(query) = &envelope.message {
            let asked = (&query.request, &query.workspace, &query.chat);
            (
                asked,
                matches!(query.query, WorktreeQueryKind::State).then(|| self.state(&query.chat))?,
            )
        } else if let Message::WorktreeCommand(command) = &envelope.message {
            let asked = (&command.request, &command.workspace, &command.chat);
            (asked, self.act(&command.chat, &command.action)?)
        } else {
            return None;
        };
        Some(Envelope {
            id: envelope.id,
            message: answered(asked, answer),
        })
    }

    fn act(&mut self, chat: &ChatId, action: &WorktreeAction) -> Option<Answer> {
        match action {
            WorktreeAction::OpenTerminal => return Some(Err(code!("request.unknown"))),
            WorktreeAction::Newer => return None,
            WorktreeAction::Remove
            | WorktreeAction::Restore
            | WorktreeAction::OpenPullRequest { .. } => {}
        }
        if chat.as_str() != CHAT {
            return Some(Err(code!("worktree.none")));
        }
        if let WorktreeAction::OpenPullRequest { .. } = action {
            let url = DownloadUrl::try_from(COMPARE.to_owned()).ok()?;
            return Some(Ok(WorktreeAnswerKind::PullRequestPage { url }));
        }
        self.removed = matches!(action, WorktreeAction::Remove);
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

fn answered(
    (request, workspace, chat): (&RequestId, &WorkspaceId, &ChatId),
    answer: Answer,
) -> Message {
    match answer {
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
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{WorktreeCommand, WorktreeQuery};

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

    fn query(chat: &str, query: WorktreeQueryKind) -> Envelope {
        asked(chat, |request, workspace, chat| {
            Message::WorktreeQuery(WorktreeQuery {
                request,
                workspace,
                chat,
                query,
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

    fn refused(code: Code) -> Message {
        Message::Error(ErrorBody {
            code,
            retryable: false,
        })
    }

    // What answers `asked` under its id: the state or page, or the code it was refused with.
    fn told(fake: &mut FakeWorktree, asked: &Envelope) -> Answer {
        let answer = fake.answer(asked).unwrap();
        assert_eq!(answer.id, 9);
        if let Message::Error(ErrorBody { code, .. }) = &answer.message {
            assert_eq!(answer.message, refused(code.clone()));
            return Err(code.clone());
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
        Ok(told.answer)
    }

    fn state(removed: bool) -> WorktreeAnswerKind {
        WorktreeAnswerKind::State {
            removed,
            uncommitted: true,
            unpushed: true,
        }
    }

    #[test]
    fn the_worktree_goes_to_the_trash_and_back_and_a_repeat_answers_the_same() {
        let mut fake = FakeWorktree::default();
        let ask = query("c-1", WorktreeQueryKind::State);
        let remove = command("c-1", WorktreeAction::Remove);
        let restore = command("c-1", WorktreeAction::Restore);

        assert_eq!(told(&mut fake, &ask), Ok(state(false)));
        assert_eq!(told(&mut fake, &remove), Ok(state(true)));
        assert_eq!(told(&mut fake, &remove), Ok(state(true)));
        assert_eq!(told(&mut fake, &ask), Ok(state(true)));
        assert_eq!(told(&mut fake, &restore), Ok(state(false)));
        assert_eq!(told(&mut fake, &restore), Ok(state(false)));
    }

    #[test]
    fn a_pull_request_gets_the_compare_page_and_a_terminal_is_refused() {
        let mut fake = FakeWorktree::default();
        told(&mut fake, &command("c-1", WorktreeAction::Remove)).unwrap();
        let pull = WorktreeAction::OpenPullRequest {
            title: "Merge the lists".to_owned(),
            body: String::new(),
        };
        let url = DownloadUrl::try_from(COMPARE.to_owned()).unwrap();

        assert_eq!(
            told(&mut fake, &command("c-1", pull)),
            Ok(WorktreeAnswerKind::PullRequestPage { url })
        );
        for chat in ["c-1", "c-2"] {
            let terminal = fake.answer(&command(chat, WorktreeAction::OpenTerminal));
            assert_eq!(terminal.unwrap().message, refused(code!("request.unknown")));
        }
        assert_eq!(fake.answer(&command("c-1", WorktreeAction::Newer)), None);
        // Neither moved the worktree.
        assert_eq!(
            told(&mut fake, &query("c-1", WorktreeQueryKind::State)),
            Ok(state(true))
        );
    }

    #[test]
    fn another_chat_has_no_worktree_and_other_messages_get_nothing() {
        let mut fake = FakeWorktree::default();
        let pull = WorktreeAction::OpenPullRequest {
            title: String::new(),
            body: String::new(),
        };

        for asked in [
            query("c-2", WorktreeQueryKind::State),
            command("c-2", WorktreeAction::Remove),
            command("c-2", WorktreeAction::Restore),
            command("c-2", pull),
        ] {
            assert_eq!(
                fake.answer(&asked).unwrap().message,
                refused(code!("worktree.none"))
            );
        }
        assert_eq!(fake.answer(&query("c-1", WorktreeQueryKind::Newer)), None);
        let unknown = Envelope {
            id: 1,
            message: Message::Unknown,
        };
        assert_eq!(fake.answer(&unknown), None);
        // The other chat's remove moved nothing.
        assert_eq!(
            told(&mut fake, &query("c-1", WorktreeQueryKind::State)),
            Ok(state(false))
        );
    }
}
