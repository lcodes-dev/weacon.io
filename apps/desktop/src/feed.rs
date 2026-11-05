use std::path::PathBuf;
use std::thread;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use weacon_client::Connection;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{App, Capability, Message, RunUpdate, code};

/// What the core's connection brings, in order.
#[derive(Debug)]
pub(crate) enum Feed {
    Connected(App),
    Update(RunUpdate),
    /// The core hung up.
    Ended,
    Failed(Fault),
}

/// Connects to the core at `socket` as `me` on a thread of its own, and sends what arrives.
pub(crate) fn start(socket: PathBuf, me: App, log: Logger) -> UnboundedReceiver<Feed> {
    let (sender, receiver) = unbounded_channel();
    let failed = sender.clone();
    let spawned = thread::Builder::new()
        .name("core-feed".to_owned())
        .spawn(move || run(&socket, me, &log, &sender));
    if let Err(error) = spawned {
        let fault = Fault::new(code!("feed.start_failed"), "The connection couldn't start");
        drop(failed.send(Feed::Failed(fault.because(error))));
    }
    receiver
}

fn run(socket: &std::path::Path, me: App, log: &Logger, sender: &UnboundedSender<Feed>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let last = match runtime {
        Ok(runtime) => runtime.block_on(pump(socket, me, sender)),
        Err(error) => Err(
            Fault::new(code!("runtime.failed"), "Weacon couldn't start its runtime").because(error),
        ),
    };
    let feed = match last {
        Ok(()) => Feed::Ended,
        Err(fault) => {
            log.warn("core feed failed", &[("err", Field::error(&fault))]);
            Feed::Failed(fault)
        }
    };
    drop(sender.send(feed));
}

/// Sends the core's welcome and then each run update, until it hangs up or nobody listens.
pub(crate) async fn pump(
    socket: &std::path::Path,
    me: App,
    sender: &UnboundedSender<Feed>,
) -> Result<(), Fault> {
    let (welcome, mut connection) = Connection::open(socket, me).await?;
    if !welcome
        .capabilities
        .contains(&Capability::from(code!("run.stream")))
    {
        return Err(Fault::new(
            code!("core.no_run_stream"),
            "The core can't stream runs; update it",
        ));
    }
    if sender.send(Feed::Connected(welcome.app)).is_err() {
        return Ok(());
    }
    while let Some(message) = connection.next().await? {
        if let Message::Run(update) = message
            && sender.send(Feed::Update(update)).is_err()
        {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use weacon_client::socket_path;
    #[cfg(unix)]
    use weacon_testing::{fake_core, platform::listen};

    use super::*;
    use crate::test_support::{VERSION, messages, recording_logger};
    #[cfg(unix)]
    use crate::{test_support::updates, transcript};

    fn me() -> App {
        App {
            name: VERSION.name.to_owned(),
            version: VERSION.version.to_owned(),
            commit: VERSION.commit.to_owned(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weacon-feed-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn without_a_core_the_feed_fails_and_logs_it() {
        let dir = temp_dir("none");
        let (log, records) = recording_logger();
        let mut receiver = start(socket_path(&dir), me(), log);
        let Some(Feed::Failed(fault)) = receiver.blocking_recv() else {
            panic!("the feed didn't fail");
        };

        assert_eq!(fault.code().as_str(), "daemon.not_running");
        assert!(receiver.blocking_recv().is_none());
        assert_eq!(messages(&records), ["core feed failed"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_recorded_session_streams_over_the_socket_in_order() {
        let dir = temp_dir("session");
        let socket = socket_path(&dir);
        let listener = listen(&socket).unwrap();
        let core = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let session = fake_core::recording(fake_core::SESSION, 100_000).unwrap();
            fake_core::serve(stream, session, Duration::ZERO).await
        });
        let (sender, mut receiver) = unbounded_channel();
        pump(&socket, me(), &sender).await.unwrap();
        drop(sender);
        core.await.unwrap().unwrap();

        let Some(Feed::Connected(core)) = receiver.recv().await else {
            panic!("no welcome first");
        };
        let mut updates_sent = Vec::new();
        while let Some(Feed::Update(update)) = receiver.recv().await {
            updates_sent.push(update);
        }
        let mut transcript = transcript::Transcript::default();
        for update in &updates_sent {
            transcript.apply(&update.event);
        }
        let texts = transcript
            .rows()
            .iter()
            .filter(|row| **row == transcript::Row::Text)
            .count();

        assert_eq!(core.name, "weacon-core");
        assert_eq!(updates_sent, updates(100_000));
        assert!(texts >= 30 * 30, "{texts} messages");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_core_that_cant_stream_runs_is_refused() {
        let dir = temp_dir("old-core");
        let socket = socket_path(&dir);
        let listener = listen(&socket).unwrap();
        let core = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            stream.read_line(&mut String::new()).await.unwrap();
            let welcome = r#"{"id":1,"type":"welcome","app":{"name":"weacon-core","version":"0.1.0","commit":"1a2b3c4"}}"#;
            stream
                .write_all(format!("{welcome}\n").as_bytes())
                .await
                .unwrap();
        });
        let (sender, mut receiver) = unbounded_channel();
        let error = pump(&socket, me(), &sender).await.unwrap_err();
        core.await.unwrap();

        assert_eq!(error.code().as_str(), "core.no_run_stream");
        receiver.try_recv().unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
    }
}
