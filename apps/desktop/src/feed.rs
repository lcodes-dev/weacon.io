use std::path::{Path, PathBuf};
use std::thread;

use tokio::sync::mpsc::{Receiver, Sender, channel};
use weacon_client::Connection;
use weacon_observability::Fault;
use weacon_protocol::{App, Capability, Code, Message, RunUpdate, code};

// Updates queue here while the view draws; a full queue holds back reading from the core.
const QUEUE: usize = 4096;

/// What the core's connection brings, in order.
#[derive(Debug)]
pub(crate) enum Feed {
    Connected(App),
    Update(RunUpdate),
    /// A frame from the core that wasn't a valid message.
    Skipped(Code),
    /// The core hung up.
    Ended,
    Failed(Fault),
}

/// Connects to the core at `socket` as `me` on a thread of its own, and sends what arrives.
pub(crate) fn start(socket: PathBuf, me: App) -> Receiver<Feed> {
    let (sender, receiver) = channel(QUEUE);
    let failed = sender.clone();
    let spawned = thread::Builder::new()
        .name("core-feed".to_owned())
        .spawn(move || run(&socket, me, &sender));
    if let Err(error) = spawned {
        let fault = Fault::new(code!("feed.start_failed"), "The connection couldn't start");
        drop(failed.try_send(Feed::Failed(fault.because(error))));
    }
    receiver
}

// The view logs and reports a failure, once.
fn run(socket: &Path, me: App, sender: &Sender<Feed>) {
    let last = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Fault::new(code!("runtime.failed"), "Weacon couldn't start its runtime").because(error)
        })
        .and_then(|runtime| runtime.block_on(pump(socket, me, sender)));
    drop(sender.blocking_send(last.map_or_else(Feed::Failed, |()| Feed::Ended)));
}

/// Sends the core's welcome and then each run update, until it hangs up or nobody listens.
pub(crate) async fn pump(socket: &Path, me: App, sender: &Sender<Feed>) -> Result<(), Fault> {
    let stream = Capability::from(code!("run.stream"));
    let (welcome, mut connection) = Connection::open(socket, me, vec![stream.clone()]).await?;
    if !welcome.capabilities.contains(&stream) {
        return Err(Fault::new(
            code!("core.no_run_stream"),
            "The core can't stream runs; update it",
        ));
    }
    if sender.send(Feed::Connected(welcome.app)).await.is_err() {
        return Ok(());
    }
    while let Some(frame) = connection.next().await? {
        let item = match frame {
            Ok(Message::Run(update)) => Feed::Update(update),
            Ok(_) => continue,
            Err(error) => Feed::Skipped(error.code()),
        };
        if sender.send(item).await.is_err() {
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
    use crate::app::build;
    #[cfg(unix)]
    use crate::test_support::updates;
    use crate::test_support::{VERSION, temp_dir};

    fn me() -> App {
        build(&VERSION)
    }

    #[test]
    fn without_a_core_the_feed_fails() {
        let dir = temp_dir("feed-none");
        let mut receiver = start(socket_path(&dir), me());
        let Some(Feed::Failed(fault)) = receiver.blocking_recv() else {
            panic!("the feed didn't fail");
        };

        assert_eq!(fault.code().as_str(), "daemon.not_running");
        assert!(receiver.blocking_recv().is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_recorded_session_streams_over_the_socket_in_order() {
        let dir = temp_dir("feed-session");
        let socket = socket_path(&dir);
        let listener = listen(&socket).unwrap();
        let core_served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let session = fake_core::recording(fake_core::SESSION, 100_000).unwrap();
            fake_core::serve(stream, session, Duration::ZERO).await
        });
        let (sender, mut receiver) = channel(QUEUE);
        let feed = tokio::spawn(async move { pump(&socket, me(), &sender).await });

        let Some(Feed::Connected(core)) = receiver.recv().await else {
            panic!("no welcome first");
        };
        let mut updates_sent = Vec::new();
        while let Some(Feed::Update(update)) = receiver.recv().await {
            updates_sent.push(update);
        }
        feed.await.unwrap().unwrap();
        core_served.await.unwrap().unwrap();

        assert_eq!(core.name, "weacon-core");
        assert_eq!(updates_sent, updates(100_000));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_garbled_frame_is_passed_on_and_other_messages_skipped() {
        let dir = temp_dir("feed-garbled");
        let socket = socket_path(&dir);
        let listener = listen(&socket).unwrap();
        let core_served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            stream.read_line(&mut String::new()).await.unwrap();
            let lines = [
                r#"{"id":1,"type":"welcome","app":{"name":"weacon-core","version":"0.1.0","commit":"1a2b3c4"},"capabilities":["run.stream"]}"#,
                "not json",
                r#"{"id":0,"type":"error","code":"store.full","retryable":false}"#,
                r#"{"id":0,"type":"run","run":"r-1","event":{"event":"text_delta","text":"Hi"}}"#,
            ];
            stream
                .write_all(format!("{}\n", lines.join("\n")).as_bytes())
                .await
                .unwrap();
        });
        let (sender, mut receiver) = channel(QUEUE);
        pump(&socket, me(), &sender).await.unwrap();
        drop(sender);
        core_served.await.unwrap();
        let mut kinds = Vec::new();
        while let Some(item) = receiver.recv().await {
            kinds.push(match item {
                Feed::Connected(_) => "connected".to_owned(),
                Feed::Skipped(code) => code.as_str().to_owned(),
                Feed::Update(_) => "update".to_owned(),
                Feed::Ended | Feed::Failed(_) => "end".to_owned(),
            });
        }

        assert_eq!(kinds, ["connected", "frame.invalid", "update"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_core_that_cant_stream_runs_is_refused() {
        let dir = temp_dir("feed-old-core");
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
        let (sender, mut receiver) = channel(QUEUE);
        let error = pump(&socket, me(), &sender).await.unwrap_err();
        core.await.unwrap();

        assert_eq!(error.code().as_str(), "core.no_run_stream");
        receiver.try_recv().unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
    }
}
