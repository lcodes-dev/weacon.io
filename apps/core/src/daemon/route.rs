//! Which part of the daemon answers each message a client sends, or the server relays.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};
use weacon_observability::{Fault, Field};
use weacon_protocol::{Envelope, FrameError, Message, Outcome, Pick, Receipt, SessionsQuery, code};

use super::worktrees::{self, Late};
use super::{Shared, command_chat, query_chat};
use crate::chats::Chats;
use crate::chats::feed::Client;
use crate::git::service::Worktrees;
use crate::machine_settings::{self, Host, Identity};
use crate::picks::KeepError;
use crate::session::{Reply, Session};
use crate::uplink::{self, failure};
use crate::{app, machine};

/// How many of the server's queries are answered at once; each may run vendors' status checks.
const QUERIES_AT_ONCE: usize = 4;

// The reply to one message from a client; only a greeted one is served.
pub(super) async fn route(
    shared: &Shared,
    session: &mut Session,
    received: Result<Envelope, FrameError>,
    client: &Client,
    late: &Late,
) -> Reply {
    let Envelope { id, message } = match received {
        Ok(envelope) if session.greeted() => envelope,
        received => return session.answer(received, &shared.app),
    };
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other message is the session's to answer"
    )]
    match message {
        Message::Pick(pick) => keep(shared, id, pick).await,
        Message::ChatCommand(command) => command_chat(shared, id, command).await,
        Message::ChatQuery(query) => query_chat(shared, id, query, client).await,
        Message::DiffQuery(query) => worktrees::diff(shared, id, query, late),
        Message::WorktreeQuery(query) => worktrees::query(shared, id, query, late),
        Message::WorktreeCommand(command) => worktrees::command(shared, id, command, late),
        // The socket is its owner's alone, so only a query the server relays names who asked.
        Message::SessionsQuery(query) if query.by.is_none() => {
            match shared.outside.answer(query, &shared.log).await {
                Ok(answer) => Reply {
                    envelope: Some(Envelope {
                        id,
                        message: Message::SessionsAnswer(answer),
                    }),
                    close: false,
                },
                Err(fault) => Reply::error(id, fault.code().clone(), false),
            }
        }
        message => session.answer(Ok(Envelope { id, message }), &shared.app),
    }
}

// The receipt for a pick the extension sent, once it is kept.
async fn keep(shared: &Shared, id: u32, pick: Pick) -> Reply {
    let (picks, request) = (shared.picks.clone(), pick.request.clone());
    let kept = tokio::task::spawn_blocking(move || picks.keep(&pick))
        .await
        .unwrap_or_else(|error| Err(KeepError::Io(error.into())));
    let outcome = match kept {
        Ok(()) => {
            let request_id = request.as_str().into();
            shared
                .log
                .info("element picked", &[("request_id", request_id)]);
            Outcome::Completed
        }
        Err(KeepError::Conflict) => return Reply::error(id, code!("request.conflict"), false),
        Err(KeepError::Io(error)) => {
            shared
                .log
                .warn("pick not kept", &[("err", Field::error(&error))]);
            Outcome::Failed {
                code: code!("pick.not_kept"),
            }
        }
    };
    Reply {
        envelope: Some(Envelope {
            id,
            message: Message::Receipt(Receipt { request, outcome }),
        }),
        close: false,
    }
}

/// Keeps the machine connected to its server, and answers what the server asks, for as long
/// as the task runs; chat messages go to `chats`.
pub(super) fn server(
    app: &app::App,
    state_dir: &Path,
    chats: Chats,
    worktrees: Worktrees,
) -> impl Future<Output = ()> + Send + 'static {
    let (to_daemon, asks) = mpsc::channel(uplink::QUEUE);
    let (replies, from_daemon) = mpsc::channel(uplink::QUEUE);
    let host = Host {
        state_dir: state_dir.to_owned(),
        path: app.path.clone(),
        reporter: Arc::clone(&app.reporter),
        outside: app.outside.clone(),
        log: app.log.child(&[("component", "asks".into())]),
    };
    let link = uplink::new(app, state_dir, to_daemon, chats);
    async move {
        tokio::select! {
            never = Box::pin(link.run(from_daemon)) => match never {},
            never = uplink(Asked { host, worktrees }, asks, replies) => match never {},
        }
    }
}

/// What answers the server's asks: the machine's settings and its chats' worktrees.
#[derive(Clone)]
pub(super) struct Asked {
    pub(super) host: Host,
    pub(super) worktrees: Worktrees,
}

/// Answers each ask the uplink hands over, under its envelope id: queries on tasks of their
/// own, a few at once, and commands one after another in the order they came.
pub(super) async fn uplink(
    asked: Asked,
    mut asks: mpsc::Receiver<Envelope>,
    replies: mpsc::Sender<Envelope>,
) -> ! {
    let (commands, in_order) = mpsc::channel(uplink::QUEUE);
    tokio::spawn(one_by_one(asked.clone(), in_order, replies.clone()));
    let room = Arc::new(Semaphore::new(QUERIES_AT_ONCE));
    while let Some(ask) = asks.recv().await {
        if matches!(
            ask.message,
            Message::MachineSettingsCommand(_) | Message::WorktreeCommand(_)
        ) {
            drop(commands.send(ask).await);
            continue;
        }
        let (asked, replies, room) = (asked.clone(), replies.clone(), Arc::clone(&room));
        tokio::spawn(async move {
            let Ok(_turn) = room.acquire().await else {
                return;
            };
            send_all(&replies, relayed(&asked, ask).await).await;
        });
    }
    // The uplink holds the other end for as long as the daemon runs.
    std::future::pending().await
}

async fn one_by_one(
    asked: Asked,
    mut commands: mpsc::Receiver<Envelope>,
    replies: mpsc::Sender<Envelope>,
) {
    while let Some(command) = commands.recv().await {
        send_all(&replies, relayed(&asked, command).await).await;
    }
}

async fn send_all(replies: &mpsc::Sender<Envelope>, answers: Vec<Envelope>) {
    for answer in answers {
        if replies.send(answer).await.is_err() {
            return;
        }
    }
}

/// The replies to an ask the server relays, in order: a chat's changes are one a file. None to
/// an error it sends of its own, which a reply would only echo. The uplink lets through only
/// the owner's asks.
async fn relayed(asked: &Asked, ask: Envelope) -> Vec<Envelope> {
    let Envelope { id, message } = ask;
    let Asked { host, worktrees } = asked;
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "the server relays no other ask a core answers"
    )]
    let answered = match message {
        Message::Error(_) => return Vec::new(),
        Message::MachineSettingsQuery(query) => {
            async { machine_settings::query(host, &owner(host).await?, query).await }
                .await
                .map(Message::MachineSettingsAnswer)
        }
        Message::MachineSettingsCommand(command) => {
            async { machine_settings::command(host, &owner(host).await?, command).await }
                .await
                .map(Message::MachineSettingsAnswer)
        }
        Message::SessionsQuery(query) => sessions(host, query).await,
        Message::DiffQuery(query) => {
            let files = worktrees.diff(query).await;
            let files = files.map(|files| files.into_iter().map(Message::DiffAnswer).collect());
            return replies(host, id, files);
        }
        Message::WorktreeQuery(query) => worktrees.query(query).await.map(Message::WorktreeAnswer),
        Message::WorktreeCommand(command) => worktrees
            .command(command)
            .await
            .map(Message::WorktreeAnswer),
        _ => Err(Fault::new(
            code!("request.unknown"),
            "This core doesn't answer that request",
        )),
    };
    replies(host, id, answered.map(|message| vec![message]))
}

// The answers under the ask's envelope id, or the failure the ask came to.
fn replies(host: &Host, id: u32, answered: Result<Vec<Message>, Fault>) -> Vec<Envelope> {
    answered.map_or_else(
        |fault| {
            host.log
                .warn("ask failed", &[("err", Field::error(&fault))]);
            vec![failure(id, fault.code().clone())]
        },
        |messages| {
            messages
                .into_iter()
                .map(|message| Envelope { id, message })
                .collect()
        },
    )
}

/// The machine owner's own sessions outside Weacon, for them alone.
async fn sessions(host: &Host, query: SessionsQuery) -> Result<Message, Fault> {
    let owner = owner(host).await?;
    if query.by.as_ref() != Some(&owner.account) {
        return Err(Fault::new(
            code!("sessions.not_owner"),
            "Only this machine's owner can read its sessions",
        ));
    }
    let answer = host.outside.answer(query, &host.log).await?;
    Ok(Message::SessionsAnswer(answer))
}

/// The machine and its owner, as the pairing kept now says.
async fn owner(host: &Host) -> Result<Identity, Fault> {
    let dir = host.state_dir.clone();
    let paired = app::blocking(move || Ok(machine::read(&dir)))
        .await
        .map_err(|error| machine::unreadable().because(error))??
        .ok_or_else(|| Fault::new(code!("machine.unpaired"), "This machine isn't paired"))?;
    Ok(Identity {
        machine: paired.machine,
        account: paired.owner,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use weacon_protocol::{
        AccountId, AgentKind, Asker, ErrorBody, MachineSettingsAction, MachineSettingsAnswerKind,
        MachineSettingsCommand, MachineSettingsQuery, MachineSettingsQueryKind, SessionsAnswer,
        SessionsAnswerKind, SessionsQuery, SessionsQueryKind,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::machine::{Key, Paired};
    use crate::outside::Outside;
    use crate::reports;
    use crate::test_support::{self, SESSION, claude_home};

    fn id<T: TryFrom<String>>(id: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(id.to_owned()).unwrap()
    }

    fn host(dir: &TempDir) -> (Asked, weacon_testing::Records) {
        let app = test_support::app(Some(dir), None);
        let (log, records) = test_support::recording_logger();
        let host = Host {
            state_dir: dir.to_path_buf(),
            path: Some(dir.join("bin").into_os_string()),
            reporter: Arc::clone(&app.reporter),
            outside: Outside::new(
                vec![(
                    AgentKind::Claude,
                    claude_home(dir, "/MARKER-FOLDER", "MARKER-PROMPT"),
                )],
                Some(&dir.join("state")),
            ),
            log,
        };
        let no_trash = |_: &Path, _| Err(std::io::ErrorKind::Unsupported.into());
        let worktrees = Worktrees::new(dir, None, None, no_trash);
        (Asked { host, worktrees }, records)
    }

    // The one reply to `ask`.
    async fn one(asked: &Asked, ask: Envelope) -> Envelope {
        let mut replies = relayed(asked, ask).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        replies.remove(0)
    }

    fn pair(dir: &TempDir) {
        let paired = Paired {
            server: "https://weacon.example.com".to_owned(),
            pinned: None,
            machine: id("mc-1"),
            owner: id("a-1"),
            token: "t0ken".to_owned(),
            key: Key::from_random([7; 32]),
        };
        machine::write(dir, &paired).unwrap();
    }

    fn by(account: &str) -> Asker {
        Asker {
            account: id::<AccountId>(account),
            member: id("m-1"),
        }
    }

    fn settings(account: &str) -> Envelope {
        Envelope {
            id: 7,
            message: Message::MachineSettingsQuery(MachineSettingsQuery {
                request: id("r-1"),
                machine: id("mc-1"),
                workspace: id("w1"),
                query: MachineSettingsQueryKind::Current,
                by: Some(by(account)),
            }),
        }
    }

    #[tokio::test]
    async fn the_owners_settings_query_is_answered_under_its_envelope_id() {
        let dir = TempDir::new().unwrap();
        pair(&dir);
        let (host, records) = host(&dir);

        let reply = one(&host, settings("a-1")).await;

        assert_eq!(reply.id, 7);
        let Message::MachineSettingsAnswer(answer) = reply.message else {
            panic!("expected an answer, got {:?}", reply.message);
        };
        assert_eq!(answer.request.as_str(), "r-1");
        assert!(matches!(
            answer.answer,
            MachineSettingsAnswerKind::Current(_)
        ));
        assert!(test_support::messages(&records).is_empty());
    }

    #[tokio::test]
    async fn an_ask_that_fails_is_answered_with_its_code_and_logged_once() {
        let dir = TempDir::new().unwrap();
        let (host, records) = host(&dir);

        let unpaired = one(&host, settings("a-1")).await;
        pair(&dir);
        let stranger = one(&host, settings("a-2")).await;

        assert_eq!(unpaired.id, 7);
        assert_eq!(
            test_support::error_code(&unpaired.message),
            Some("machine.unpaired")
        );
        assert_eq!(
            test_support::error_code(&stranger.message),
            Some("settings.not_owner")
        );
        assert_eq!(
            test_support::messages(&records),
            ["ask failed", "ask failed"]
        );
    }

    #[tokio::test]
    async fn an_ask_it_doesnt_answer_is_unknown_and_an_error_goes_unanswered() {
        let dir = TempDir::new().unwrap();
        let (host, _) = host(&dir);
        let other = Envelope {
            id: 4,
            message: Message::Pick(test_support::pick("r-3", "<p>Hi</p>")),
        };
        let error = Envelope {
            id: 5,
            message: Message::Error(ErrorBody {
                code: code!("relay.busy"),
                retryable: true,
            }),
        };

        let unknown = one(&host, other).await;
        let echoed = relayed(&host, error).await;

        assert_eq!(unknown.id, 4);
        assert_eq!(
            test_support::error_code(&unknown.message),
            Some("request.unknown")
        );
        assert_eq!(echoed, []);
    }

    #[tokio::test]
    async fn a_relayed_diff_comes_back_a_file_to_a_reply_under_its_envelope_id() {
        use weacon_protocol::{DiffQuery, DiffQueryKind};

        let dir = TempDir::new().unwrap();
        let (mut asked, _) = host(&dir);
        let (worktrees, workspace, chat, worktree) = crate::git::service::with_chat(&dir);
        asked.worktrees = worktrees;
        for name in ["a.md", "b.md"] {
            std::fs::write(worktree.join(name), "mine\n").unwrap();
        }
        let diff = Message::DiffQuery(DiffQuery {
            request: id("r-1"),
            workspace,
            chat,
            query: DiffQueryKind::Changes,
            by: Some(by("a-1")),
        });

        let files = relayed(
            &asked,
            Envelope {
                id: 8,
                message: diff,
            },
        )
        .await;

        let paths: Vec<(u32, u32, String)> = files.into_iter().map(file_of).collect();
        assert_eq!(
            paths,
            [(8, 2, "a.md".to_owned()), (8, 2, "b.md".to_owned())]
        );
    }

    fn file_of(reply: Envelope) -> (u32, u32, String) {
        let Message::DiffAnswer(answer) = reply.message else {
            panic!("not a diff answer");
        };
        let weacon_protocol::DiffAnswerKind::File { files, file } = answer.answer else {
            panic!("not a file");
        };
        (reply.id, files, file.path)
    }

    #[tokio::test]
    async fn a_relayed_worktree_ask_for_a_chat_without_one_is_refused_under_its_id() {
        let dir = TempDir::new().unwrap();
        let (mut asked, _) = host(&dir);
        let (worktrees, workspace, _, _) = crate::git::service::with_chat(&dir);
        asked.worktrees = worktrees;
        let none = Message::WorktreeQuery(weacon_protocol::WorktreeQuery {
            request: id("r-2"),
            workspace,
            chat: id("c-2"),
            query: weacon_protocol::WorktreeQueryKind::State,
            by: Some(by("a-1")),
        });

        let refused = one(
            &asked,
            Envelope {
                id: 9,
                message: none,
            },
        )
        .await;

        assert_eq!(refused.id, 9);
        assert_eq!(
            test_support::error_code(&refused.message),
            Some("worktree.none")
        );
    }

    fn sessions(by: Option<&str>, query: SessionsQueryKind) -> Envelope {
        Envelope {
            id: 9,
            message: Message::SessionsQuery(SessionsQuery {
                request: id("r-4"),
                machine: id("mc-1"),
                query,
                by: by.map(id),
            }),
        }
    }

    #[tokio::test]
    async fn only_the_machines_owner_reads_its_sessions_and_none_of_them_is_logged() {
        let dir = TempDir::new().unwrap();
        pair(&dir);
        let (host, records) = host(&dir);
        let open = SessionsQueryKind::Open {
            agent: AgentKind::Claude,
            session: id(SESSION),
            after: 0,
        };

        let listed = one(&host, sessions(Some("a-1"), SessionsQueryKind::List)).await;
        let opened = one(&host, sessions(Some("a-1"), open)).await;
        let stranger = one(&host, sessions(Some("a-2"), SessionsQueryKind::List)).await;
        let nobody = one(&host, sessions(None, SessionsQueryKind::List)).await;
        let newer = one(&host, sessions(Some("a-1"), SessionsQueryKind::Newer)).await;

        assert_eq!(listed.id, 9);
        let Message::SessionsAnswer(answer) = listed.message else {
            panic!("expected an answer, got {:?}", listed.message);
        };
        assert_eq!(answer.request.as_str(), "r-4");
        let SessionsAnswerKind::List { sessions } = answer.answer else {
            panic!("expected a list, got {:?}", answer.answer);
        };
        assert_eq!(
            sessions.first().and_then(|found| found.folder.as_deref()),
            Some("/MARKER-FOLDER")
        );
        assert!(matches!(
            opened.message,
            Message::SessionsAnswer(SessionsAnswer {
                answer: SessionsAnswerKind::Opened { ref events, more: false },
                ..
            }) if events.len() == 1
        ));
        for refused in [stranger, nobody] {
            assert_eq!(
                test_support::error_code(&refused.message),
                Some("sessions.not_owner")
            );
        }
        assert_eq!(
            test_support::error_code(&newer.message),
            Some("message.unsupported")
        );
        let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    fn reports(envelope: u32, send: bool) -> Envelope {
        Envelope {
            id: envelope,
            message: Message::MachineSettingsCommand(MachineSettingsCommand {
                request: id("r-2"),
                machine: id("mc-1"),
                workspace: id("w1"),
                action: MachineSettingsAction::SetReports { send },
                by: Some(by("a-1")),
            }),
        }
    }

    #[tokio::test]
    async fn commands_are_applied_in_the_order_they_came_and_queries_answered_beside_them() {
        let dir = TempDir::new().unwrap();
        pair(&dir);
        let (host, _) = host(&dir);
        let (to_daemon, asks) = mpsc::channel(8);
        let (replies, mut from_daemon) = mpsc::channel(8);
        tokio::spawn(uplink(host, asks, replies));
        for ask in [
            reports(1, true),
            settings("a-2"),
            reports(2, false),
            reports(3, true),
            reports(4, false),
        ] {
            to_daemon.send(ask).await.unwrap();
        }

        let mut answered = Vec::new();
        for _ in 0..5 {
            // Bounded, so a reply that never comes fails the test rather than hanging it.
            let reply =
                tokio::time::timeout(std::time::Duration::from_secs(10), from_daemon.recv())
                    .await
                    .unwrap()
                    .unwrap();
            answered.push((
                reply.id,
                matches!(reply.message, Message::MachineSettingsAnswer(_)),
            ));
        }
        let commands: Vec<u32> = answered
            .iter()
            .filter(|(_, ok)| *ok)
            .map(|(id, _)| *id)
            .collect();
        let refused: Vec<u32> = answered
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(id, _)| *id)
            .collect();

        assert_eq!(commands, [1, 2, 3, 4]);
        assert_eq!(refused, [7]);
        assert_eq!(reports::current(&dir), Some(false));
    }
}
