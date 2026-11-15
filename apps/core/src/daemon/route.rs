//! Which part of the daemon answers each message a client sends, or the server relays.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};
use weacon_observability::{Fault, Field};
use weacon_protocol::{Envelope, FrameError, Message, code};

use super::worktrees::{self, Late};
use super::{Shared, command_chat, keep, query_chat};
use crate::chats::Chats;
use crate::chats::feed::Client;
use crate::machine_settings::{self, Host, Identity};
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
        Message::ChatCommand(command) => command_chat(shared, id, command, client).await,
        Message::ChatQuery(query) => query_chat(shared, id, query, client).await,
        Message::DiffQuery(query) => worktrees::diff(shared, id, query, late),
        Message::WorktreeQuery(query) => worktrees::query(shared, id, query, late),
        Message::WorktreeCommand(command) => worktrees::command(shared, id, command, late),
        message => session.answer(Ok(Envelope { id, message }), &shared.app),
    }
}

/// Keeps the machine connected to its server, and answers what the server asks, for as long
/// as the task runs; chat messages go to `chats`.
pub(super) fn server(
    app: &app::App,
    state_dir: &Path,
    chats: Chats,
) -> impl Future<Output = ()> + Send + 'static {
    let (to_daemon, asks) = mpsc::channel(uplink::QUEUE);
    let (replies, from_daemon) = mpsc::channel(uplink::QUEUE);
    let host = Host {
        state_dir: state_dir.to_owned(),
        path: app.path.clone(),
        reporter: Arc::clone(&app.reporter),
        log: app.log.child(&[("component", "asks".into())]),
    };
    let link = uplink::new(app, state_dir, to_daemon, chats);
    async move {
        tokio::select! {
            never = Box::pin(link.run(from_daemon)) => match never {},
            never = uplink(host, asks, replies) => match never {},
        }
    }
}

/// Answers each ask the uplink hands over, under its envelope id: queries on tasks of their
/// own, a few at once, and commands one after another in the order they came.
pub(super) async fn uplink(
    host: Host,
    mut asks: mpsc::Receiver<Envelope>,
    replies: mpsc::Sender<Envelope>,
) -> ! {
    let (commands, in_order) = mpsc::channel(uplink::QUEUE);
    tokio::spawn(one_by_one(host.clone(), in_order, replies.clone()));
    let room = Arc::new(Semaphore::new(QUERIES_AT_ONCE));
    while let Some(ask) = asks.recv().await {
        if matches!(ask.message, Message::MachineSettingsCommand(_)) {
            drop(commands.send(ask).await);
            continue;
        }
        let (host, replies, room) = (host.clone(), replies.clone(), Arc::clone(&room));
        tokio::spawn(async move {
            let Ok(_turn) = room.acquire().await else {
                return;
            };
            if let Some(reply) = relayed(&host, ask).await {
                drop(replies.send(reply).await);
            }
        });
    }
    // The uplink holds the other end for as long as the daemon runs.
    std::future::pending().await
}

async fn one_by_one(
    host: Host,
    mut commands: mpsc::Receiver<Envelope>,
    replies: mpsc::Sender<Envelope>,
) {
    while let Some(command) = commands.recv().await {
        if let Some(reply) = relayed(&host, command).await {
            drop(replies.send(reply).await);
        }
    }
}

/// The reply to an ask the server relays; none to an error it sends of its own, which a reply
/// would only echo.
async fn relayed(host: &Host, ask: Envelope) -> Option<Envelope> {
    let Envelope { id, message } = ask;
    let answered = if let Message::Error(_) = message {
        return None;
    } else if let Message::MachineSettingsQuery(query) = message {
        async { machine_settings::query(host, &owner(host).await?, query).await }.await
    } else if let Message::MachineSettingsCommand(command) = message {
        async { machine_settings::command(host, &owner(host).await?, command).await }.await
    } else {
        Err(Fault::new(
            code!("request.unknown"),
            "This core doesn't answer that request",
        ))
    };
    Some(answered.map_or_else(
        |fault| {
            host.log
                .warn("ask failed", &[("err", Field::error(&fault))]);
            failure(id, fault.code().clone())
        },
        |answer| Envelope {
            id,
            message: Message::MachineSettingsAnswer(answer),
        },
    ))
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
        AccountId, Asker, ErrorBody, MachineSettingsAction, MachineSettingsAnswerKind,
        MachineSettingsCommand, MachineSettingsQuery, MachineSettingsQueryKind, SessionsQuery,
        SessionsQueryKind,
    };
    use weacon_testing::TempDir;

    use super::*;
    use crate::machine::{Key, Paired};
    use crate::reports;
    use crate::test_support;

    fn id<T: TryFrom<String>>(id: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(id.to_owned()).unwrap()
    }

    fn host(dir: &TempDir) -> (Host, weacon_testing::Records) {
        let app = test_support::app(Some(dir), None);
        let (log, records) = test_support::recording_logger();
        let host = Host {
            state_dir: dir.to_path_buf(),
            path: Some(dir.join("bin").into_os_string()),
            reporter: Arc::clone(&app.reporter),
            log,
        };
        (host, records)
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

        let reply = relayed(&host, settings("a-1")).await.unwrap();

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

        let unpaired = relayed(&host, settings("a-1")).await.unwrap();
        pair(&dir);
        let stranger = relayed(&host, settings("a-2")).await.unwrap();

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
            message: Message::SessionsQuery(SessionsQuery {
                request: id("r-3"),
                machine: id("mc-1"),
                query: SessionsQueryKind::List,
                by: Some(id("a-1")),
            }),
        };
        let error = Envelope {
            id: 5,
            message: Message::Error(ErrorBody {
                code: code!("relay.busy"),
                retryable: true,
            }),
        };

        let unknown = relayed(&host, other).await.unwrap();
        let echoed = relayed(&host, error).await;

        assert_eq!(unknown.id, 4);
        assert_eq!(
            test_support::error_code(&unknown.message),
            Some("request.unknown")
        );
        assert_eq!(echoed, None);
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
