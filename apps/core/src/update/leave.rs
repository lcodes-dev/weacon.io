//! Asks the running daemon for leave to install a build that restarts it, and says how the
//! install went, over the daemon's own socket.

use std::path::Path;
use std::time::Duration;

use weacon_client::Connection;
use weacon_observability::Fault;
use weacon_protocol::{
    App, Capability, Envelope, InstallAction, InstallCommand, Message, Outcome, Receipt, RequestId,
    code,
};

/// What a daemon that gives leave offers.
pub(crate) const LEAVE: Capability = Capability::new(code!("update.leave"));

/// How long the daemon has to answer an ask or a report.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// Tells the daemon that gave leave how the install goes; true once it took the word. False
/// when it is gone or its leave ran out, so a restart would cut a turn.
pub(crate) type Report = Box<dyn FnMut(InstallAction) -> bool + Send>;

/// Leave from the daemon at `socket` to install and restart it, asked as `me`; none while a
/// chat is busy. With no daemon there, or one too old to give leave, nothing waits for it.
pub(crate) fn ask(socket: &Path, me: App) -> Result<Option<Report>, Fault> {
    let runtime = crate::app::runtime()?;
    let opened = runtime.block_on(Connection::open(socket, me, Vec::new()));
    let (welcome, mut daemon) = match opened {
        Err(fault) if fault.code() == &code!("daemon.not_running") => return Ok(Some(anyway())),
        opened => opened?,
    };
    if !welcome.capabilities.contains(&LEAVE) {
        return Ok(Some(anyway()));
    }
    // The hello went as envelope 1.
    let mut sent = 1;
    let mut say = move |action| {
        sent += 1;
        runtime.block_on(said(&mut daemon, sent, action))
    };
    if !say(InstallAction::Leave)? {
        return Ok(None);
    }
    Ok(Some(Box::new(move |action| say(action).unwrap_or(false))))
}

fn anyway() -> Report {
    Box::new(|_| true)
}

// Whether the daemon took `action`, sent as envelope `id`: true on its receipt, false when it
// refuses leave because a chat is busy.
async fn said(daemon: &mut Connection, id: u32, action: InstallAction) -> Result<bool, Fault> {
    let request = RequestId::try_from(format!("install-{id}")).map_err(|_| no_answer())?;
    let message = Message::InstallCommand(InstallCommand { request, action });
    daemon.send(&Envelope { id, message }).await?;
    tokio::time::timeout(ANSWER_WITHIN, answer(daemon, id))
        .await
        .map_err(|_| no_answer())?
}

async fn answer(daemon: &mut Connection, id: u32) -> Result<bool, Fault> {
    while let Some(frame) = daemon.next().await? {
        let Ok(envelope) = frame else { continue };
        if envelope.id != id {
            continue;
        }
        if let Message::Receipt(Receipt {
            outcome: Outcome::Completed,
            ..
        }) = envelope.message
        {
            return Ok(true);
        }
        if let Message::Error(body) = &envelope.message
            && body.code == code!("update.chat_busy")
        {
            return Ok(false);
        }
        return Err(no_answer());
    }
    Err(no_answer())
}

fn no_answer() -> Fault {
    Fault::new(
        code!("update.no_leave"),
        "The daemon didn't give leave to install",
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use weacon_agents::SpawnPolicy;
    use weacon_protocol::{ErrorBody, Welcome, encode_line};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::chats::Places;
    use crate::daemon::{Daemon, build, socket_path};
    use crate::platform;
    use crate::test_support::VERSION;

    fn me() -> App {
        build(&VERSION)
    }

    async fn asked(socket: PathBuf) -> Result<Option<Report>, Fault> {
        tokio::task::spawn_blocking(move || ask(&socket, me()))
            .await
            .unwrap()
    }

    // Reports `action` off the runtime, where the report's own runtime may run and drop.
    async fn reported(mut report: Report, action: InstallAction) -> bool {
        tokio::task::spawn_blocking(move || report(action))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn with_no_daemon_running_leave_is_not_needed() {
        let dir = TempDir::new().unwrap();
        let report = asked(socket_path(&dir)).await.unwrap().unwrap();
        assert!(reported(report, InstallAction::Restarting).await);
    }

    #[tokio::test]
    async fn an_idle_daemon_gives_leave_and_takes_the_report() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let places = Places::default();
        let daemon = Daemon::start(&dir, me(), SpawnPolicy::default(), places, log).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(daemon.serve(async move { drop(stopped.await) }));

        let report = asked(socket_path(&dir)).await.unwrap().unwrap();
        assert!(reported(report, InstallAction::Failed).await);
        let again = asked(socket_path(&dir)).await.unwrap().unwrap();
        assert!(reported(again, InstallAction::Restarting).await);
        stop.send(()).unwrap();
        served.await.unwrap();
    }

    // A daemon that welcomes with `capabilities`, then answers the first ask with `answer`.
    fn daemon(dir: &Path, capabilities: Vec<Capability>, answer: Option<Message>) {
        let mut listener = platform::bind(&socket_path(dir)).unwrap();
        tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let welcome = Message::Welcome(Welcome {
                app: me(),
                capabilities,
            });
            let welcome = encode_line(&Envelope {
                id: 1,
                message: welcome,
            })
            .unwrap();
            stream.write_all(&welcome).await.unwrap();
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            if let Some(message) = answer {
                let asked: Envelope = serde_json::from_str(&line).unwrap();
                assert_eq!(asked.id, 2, "the hello went as 1");
                let reply = encode_line(&Envelope { id: 2, message }).unwrap();
                stream.write_all(&reply).await.unwrap();
            }
        });
    }

    fn error(code: weacon_protocol::Code) -> Message {
        Message::Error(ErrorBody {
            code,
            retryable: false,
        })
    }

    #[tokio::test]
    async fn a_daemon_with_a_busy_chat_refuses_leave() {
        let dir = TempDir::new().unwrap();
        daemon(&dir, vec![LEAVE], Some(error(code!("update.chat_busy"))));
        assert!(asked(socket_path(&dir)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_daemon_that_answers_otherwise_or_hangs_up_gives_no_leave() {
        let dir = TempDir::new().unwrap();
        daemon(&dir, vec![LEAVE], Some(error(code!("message.unsupported"))));
        let refused = asked(socket_path(&dir)).await.err().unwrap();
        assert_eq!(refused.code().as_str(), "update.no_leave");

        let dir = TempDir::new().unwrap();
        daemon(&dir, vec![LEAVE], None);
        let refused = asked(socket_path(&dir)).await.err().unwrap();
        assert_eq!(refused.code().as_str(), "update.no_leave");
    }

    #[tokio::test]
    async fn a_daemon_that_refuses_the_handshake_gives_no_leave() {
        let dir = TempDir::new().unwrap();
        let mut listener = platform::bind(&socket_path(&dir)).unwrap();
        tokio::spawn(async move {
            let mut stream = BufReader::new(listener.accept().await.unwrap());
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let refused = error(code!("protocol.unsupported"));
            let refused = encode_line(&Envelope {
                id: 1,
                message: refused,
            })
            .unwrap();
            stream.write_all(&refused).await.unwrap();
        });
        let refused = asked(socket_path(&dir)).await.err().unwrap();
        assert_eq!(refused.code().as_str(), "daemon.other_protocol");
    }

    #[tokio::test]
    async fn a_daemon_too_old_to_give_leave_is_not_asked() {
        let dir = TempDir::new().unwrap();
        daemon(&dir, Vec::new(), None);
        let report = asked(socket_path(&dir)).await.unwrap().unwrap();
        assert!(reported(report, InstallAction::Restarting).await);
    }
}
