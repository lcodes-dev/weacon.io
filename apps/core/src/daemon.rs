use std::fs::{self, File, TryLockError};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinSet;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{App, Decoder, code, encode};
use weacon_version::Version;

use crate::platform::{self, Listener, Stream};
use crate::session::Session;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_CHUNK: usize = 8 * 1024;

pub(crate) fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("weacon.sock")
}

/// How this build introduces itself on the socket.
pub(crate) fn build(version: &Version) -> App {
    App {
        name: version.name.to_owned(),
        version: version.version.to_owned(),
        commit: version.commit.to_owned(),
    }
}

/// Runs the daemon until Ctrl-C or SIGTERM.
pub(crate) fn run(app: &crate::app::App) -> Result<(), Fault> {
    let state_dir = app
        .state_dir
        .as_deref()
        .ok_or_else(crate::app::state_unavailable)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Fault::new(
                code!("daemon.runtime_failed"),
                "The daemon couldn't start its runtime",
            )
            .because(error)
        })?;
    runtime.block_on(async {
        let daemon = Daemon::start(state_dir, build(&app.version), app.log.clone())?;
        let log = app.log.clone();
        daemon
            .serve(async move {
                if let Err(error) = platform::shutdown_signal().await {
                    log.error("signals unavailable", &[("err", Field::error(&error))]);
                }
            })
            .await;
        Ok(())
    })
}

/// A daemon that holds its state folder's lock and listens on its socket.
pub(crate) struct Daemon {
    listener: Listener,
    socket: PathBuf,
    owner: u32,
    app: App,
    log: Logger,
    // Held for the daemon's life, so a second daemon can't start.
    _lock: File,
}

impl Daemon {
    /// Locks `state_dir`, replaces a stale socket and listens; must run inside a tokio runtime.
    pub(crate) fn start(state_dir: &Path, app: App, log: Logger) -> Result<Self, Fault> {
        let failed = |error: io::Error| {
            Fault::new(code!("daemon.start_failed"), "The daemon couldn't start").because(error)
        };
        platform::private_dir(state_dir).map_err(failed)?;
        let owner = platform::private_owner(state_dir)
            .map_err(failed)?
            .ok_or_else(|| {
                Fault::new(
                    code!("state.not_private"),
                    "Other users can reach the state folder; make it readable only by you",
                )
            })?;
        let lock = platform::private_append(&state_dir.join("weacon.lock")).map_err(failed)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => Fault::new(
                code!("daemon.running"),
                "Another daemon is already running for this state folder",
            ),
            TryLockError::Error(error) => failed(error),
        })?;
        let socket = socket_path(state_dir);
        if let Err(error) = fs::remove_file(&socket)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(failed(error));
        }
        let listener = platform::bind(&socket).map_err(failed)?;
        log.info("daemon started", &[]);
        Ok(Self {
            listener,
            socket,
            owner,
            app,
            log,
            _lock: lock,
        })
    }

    /// Serves clients until `shutdown` finishes, then closes every connection and the socket.
    pub(crate) async fn serve(self, shutdown: impl Future<Output = ()>) {
        let mut clients = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => self.admit(stream, &mut clients),
                    Err(error) => self.log.warn("accept failed", &[("err", Field::error(&error))]),
                },
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
        drop(self.listener);
        drop(fs::remove_file(&self.socket));
        clients.shutdown().await;
        self.log.info("daemon stopped", &[]);
    }

    fn admit(&self, stream: Stream, clients: &mut JoinSet<()>) {
        match platform::peer_uid(&stream) {
            Ok(uid) if uid == self.owner => {}
            _ => return self.log.warn("client refused", &[]),
        }
        let (app, log) = (self.app.clone(), self.log.clone());
        clients.spawn(async move {
            if let Err(error) = serve_client(stream, app).await {
                log.debug("client failed", &[("err", Field::error(&error))]);
            }
        });
    }
}

async fn serve_client(stream: Stream, app: App) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let mut session = Session::default();
    let mut decoder = Decoder::default();
    let mut buf = [0; READ_CHUNK];
    let handshake = tokio::time::sleep(HANDSHAKE_TIMEOUT);
    tokio::pin!(handshake);
    loop {
        let read = tokio::select! {
            () = &mut handshake, if !session.greeted() => return Ok(()),
            read = reader.read(&mut buf) => read?,
        };
        let bytes = buf.get(..read).unwrap_or_default();
        if read == 0 || !answer(&mut session, &mut decoder, bytes, &app, &mut writer).await? {
            return Ok(());
        }
    }
}

// Answers what `bytes` completes; false once the connection should close.
async fn answer(
    session: &mut Session,
    decoder: &mut Decoder,
    bytes: &[u8],
    app: &App,
    writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<bool> {
    for received in decoder.push(bytes) {
        let reply = session.answer(received, app);
        if let Some(envelope) = reply.envelope
            && let Ok(line) = encode(&envelope)
        {
            writer.write_all(&line).await?;
        }
        if reply.close {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::AsyncBufReadExt;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use weacon_protocol::{Envelope, Hello, Message, PROTOCOL};

    use super::*;
    use crate::status::query;
    use crate::test_support::{Records, VERSION, error_code, messages, recording_logger, temp_dir};

    struct Running {
        socket: PathBuf,
        stop: oneshot::Sender<()>,
        served: JoinHandle<()>,
        records: Records,
    }

    fn start(dir: &Path) -> Result<Running, Fault> {
        let (log, records) = recording_logger();
        let daemon = Daemon::start(dir, build(&VERSION), log)?;
        let socket = daemon.socket.clone();
        let (stop, stopped) = oneshot::channel();
        let served = tokio::spawn(daemon.serve(async move { drop(stopped.await) }));
        Ok(Running {
            socket,
            stop,
            served,
            records,
        })
    }

    async fn ask(socket: &Path) -> Result<App, Fault> {
        let socket = socket.to_owned();
        let me = build(&VERSION);
        tokio::task::spawn_blocking(move || query(&socket, me))
            .await
            .unwrap()
    }

    fn hello(protocol: u32) -> Vec<u8> {
        encode(&Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol,
                app: build(&VERSION),
            }),
        })
        .unwrap()
    }

    // Sends `bytes` and reads replies until the daemon hangs up or `count` arrive.
    async fn exchange(socket: &Path, bytes: &[u8], count: usize) -> Vec<Message> {
        let stream = Stream::connect(socket).await.unwrap();
        let (reader, mut writer) = stream.into_split();
        writer.write_all(bytes).await.unwrap();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let mut replies = Vec::new();
        while replies.len() < count
            && let Some(line) = lines.next_line().await.unwrap()
        {
            replies.push(serde_json::from_str::<Envelope>(&line).unwrap().message);
        }
        replies
    }

    #[tokio::test]
    async fn a_client_shakes_hands_and_shutdown_cleans_up() {
        let dir = temp_dir("daemon-hello");
        let running = start(&dir).unwrap();
        let mode = fs::metadata(&running.socket).unwrap().permissions().mode() & 0o777;

        assert_eq!(ask(&running.socket).await.unwrap(), build(&VERSION));
        assert_eq!(mode, 0o600);
        running.stop.send(()).unwrap();
        running.served.await.unwrap();
        assert!(!running.socket.exists());
        assert_eq!(
            ask(&running.socket).await.unwrap_err().code().as_str(),
            "daemon.not_running"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn one_daemon_per_state_folder_and_a_stale_socket_is_replaced() {
        let dir = temp_dir("daemon-twice");
        fs::write(socket_path(&dir), "stale").unwrap();
        let first = start(&dir).unwrap();
        let second = start(&dir).err().unwrap();

        assert_eq!(second.code().as_str(), "daemon.running");
        ask(&first.socket).await.unwrap();
        first.stop.send(()).unwrap();
        first.served.await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_state_folder_others_can_reach_is_refused() {
        let dir = temp_dir("daemon-open");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            start(&dir).err().unwrap().code().as_str(),
            "state.not_private"
        );
        assert!(!socket_path(&dir).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn bad_frames_get_errors_and_another_protocol_is_hung_up_on() {
        let dir = temp_dir("daemon-frames");
        let running = start(&dir).unwrap();
        let mut bytes = b"not json\n".to_vec();
        bytes.extend(hello(PROTOCOL));
        let recovered = exchange(&running.socket, &bytes, 2).await;
        let refused = exchange(&running.socket, &hello(PROTOCOL + 1), 9).await;

        assert_eq!(error_code(&recovered[0]), Some("frame.invalid"));
        assert!(matches!(recovered[1], Message::Welcome(_)));
        assert_eq!(refused.len(), 1);
        assert_eq!(error_code(&refused[0]), Some("protocol.unsupported"));
        running.stop.send(()).unwrap();
        running.served.await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn shutdown_hangs_up_on_connected_clients() {
        let dir = temp_dir("daemon-idle");
        let running = start(&dir).unwrap();
        let client = Stream::connect(&running.socket).await.unwrap();
        let (mut reader, _writer) = client.into_split();
        running.stop.send(()).unwrap();
        running.served.await.unwrap();

        assert_eq!(reader.read(&mut [0; 16]).await.unwrap(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_never_says_hello_is_hung_up_on() {
        let dir = temp_dir("daemon-silent");
        let running = start(&dir).unwrap();
        let client = Stream::connect(&running.socket).await.unwrap();
        let (mut reader, _writer) = client.into_split();

        assert_eq!(reader.read(&mut [0; 16]).await.unwrap(), 0);
        running.stop.send(()).unwrap();
        running.served.await.unwrap();
        assert_eq!(
            messages(&running.records),
            ["daemon started", "daemon stopped"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
