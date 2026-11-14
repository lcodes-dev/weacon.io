use std::fs::{self, File, TryLockError};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio::time::Instant;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{
    App, Envelope, LineDecoder, Message, Outcome, Pick, Receipt, code, encode_line,
};

use crate::disk;
use crate::memory;
use crate::picks::{KeepError, Picks};
use crate::platform::{self, Listener, Owner, Stream};
use crate::session::{Reply, Session};
use crate::update;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const READ_CHUNK: usize = 8192;
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const DISK_CHECK: Duration = Duration::from_mins(1);
const MEMORY_SAMPLE: Duration = Duration::from_mins(5);

pub(crate) fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("weacon.sock")
}

pub(crate) use weacon_appkit::build;

/// The single-threaded async runtime the core's commands run on.
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, Fault> {
    weacon_appkit::runtime(tokio::runtime::Builder::new_current_thread())
}

/// The update schedule, for a build whose version releases number.
fn updates(state_dir: &Path, app: &App) -> Option<update::Watch> {
    let own = app.version.parse().ok()?;
    let launch = crate::service::launch_update;
    Some(update::Watch::new(
        state_dir.to_owned(),
        own,
        update::latest,
        launch,
        unix_now,
    ))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Runs the daemon until Ctrl-C or the system asks it to stop.
pub(crate) fn run(app: &crate::app::App) -> Result<(), Fault> {
    let state_dir = app
        .state_dir
        .as_deref()
        .ok_or_else(crate::app::state_unavailable)?;
    runtime()?.block_on(async {
        let signals = weacon_appkit::Signals::new().map_err(|error| {
            Fault::new(
                code!("daemon.signals_failed"),
                "The daemon couldn't catch signals",
            )
            .because(error)
        })?;
        let daemon = Daemon::start(state_dir, build(&app.version), app.log.clone())?;
        daemon.serve(signals.wait()).await;
        Ok(())
    })
}

#[derive(Clone)]
struct Shared {
    app: App,
    picks: Picks,
    log: Logger,
}

/// A daemon that holds its state folder's lock and listens on its socket.
pub(crate) struct Daemon {
    listener: Listener,
    socket: PathBuf,
    owner: Owner,
    shared: Shared,
    disk: disk::Watch,
    memory: memory::Watch,
    updates: Option<update::Watch>,
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
        let lock = weacon_appkit::private_append(&state_dir.join("weacon.lock")).map_err(failed)?;
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
        let updates = updates(state_dir, &app);
        Ok(Self {
            listener,
            socket,
            owner,
            shared: Shared {
                app,
                picks: Picks::new(state_dir),
                log,
            },
            disk: disk::Watch::new(state_dir.to_owned(), platform::free_space),
            memory: memory::Watch::new(platform::processes),
            updates,
            _lock: lock,
        })
    }

    /// Serves clients until `shutdown` finishes, then closes every connection and the socket.
    pub(crate) async fn serve(mut self, shutdown: impl Future<Output = ()>) {
        let mut clients = JoinSet::new();
        let mut disk_check = tokio::time::interval(DISK_CHECK);
        let mut memory_sample = tokio::time::interval(MEMORY_SAMPLE);
        let mut update_check = tokio::time::interval(update::CHECK_EVERY);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                _ = disk_check.tick() => self.disk.check(&self.shared.log).await,
                _ = memory_sample.tick(), if platform::PROCESSES => self.memory.record(&self.shared.log).await,
                _ = update_check.tick() => if let Some(updates) = &mut self.updates {
                    updates.check(&self.shared.log).await;
                },
                accepted = self.listener.accept() => match accepted {
                    Ok(stream) => self.admit(stream, &mut clients),
                    Err(error) => {
                        // Such as too many open files: wait, rather than spin and flood the log.
                        self.shared.log.warn("accept failed", &[("err", Field::error(&error))]);
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
        drop(self.listener);
        drop(fs::remove_file(&self.socket));
        clients.shutdown().await;
        self.shared.log.info("daemon stopped", &[]);
    }

    fn admit(&self, stream: Stream, clients: &mut JoinSet<()>) {
        if !platform::is_owner(&stream, self.owner) {
            return self.shared.log.warn("client refused", &[]);
        }
        let shared = self.shared.clone();
        clients.spawn(async move {
            if let Err(error) = serve_client(stream, &shared).await {
                shared
                    .log
                    .debug("client failed", &[("err", Field::error(&error))]);
            }
        });
    }
}

async fn serve_client(stream: Stream, shared: &Shared) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut session = Session::default();
    let mut decoder = LineDecoder::default();
    let mut buf = [0; READ_CHUNK];
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        // Until the hello, reading and answering both count against the deadline.
        let limit = (!session.greeted()).then_some(deadline);
        let Some(read) = within(limit, reader.read(&mut buf)).await.transpose()? else {
            return Ok(());
        };
        if read == 0 {
            return Ok(());
        }
        let bytes = buf.get(..read).unwrap_or_default();
        let answered = answer(&mut session, &mut decoder, bytes, shared, &mut writer);
        if !within(limit, answered).await.transpose()?.unwrap_or(false) {
            return Ok(());
        }
    }
}

// `future`'s output, or None when `deadline` comes first.
async fn within<F: Future>(deadline: Option<Instant>, future: F) -> Option<F::Output> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future).await.ok(),
        None => Some(future.await),
    }
}

// Answers what `bytes` completes; false once the connection should close.
async fn answer(
    session: &mut Session,
    decoder: &mut LineDecoder,
    bytes: &[u8],
    shared: &Shared,
    writer: &mut (impl AsyncWrite + Unpin),
) -> io::Result<bool> {
    for received in decoder.push(bytes) {
        if let Ok(Envelope {
            message: Message::Error(body),
            ..
        }) = &received
            && session.greeted()
        {
            let code = body.code.as_str().into();
            shared
                .log
                .warn("client reported an error", &[("code", code)]);
        }
        let reply = match received {
            Ok(Envelope {
                id,
                message: Message::Pick(pick),
            }) if session.greeted() => keep(shared, id, pick).await,
            received => session.answer(received, &shared.app),
        };
        if let Some(envelope) = reply.envelope
            && let Ok(line) = encode_line(&envelope)
        {
            writer.write_all(&line).await?;
        }
        if reply.close {
            return Ok(false);
        }
    }
    Ok(true)
}

// Keeps `pick` off the runtime's thread and answers with its receipt.
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

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncRead};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;
    use weacon_protocol::{ErrorBody, Hello, PROTOCOL, Welcome};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, Records, VERSION, error_code, messages, recording_logger};

    struct Running {
        socket: PathBuf,
        stop: oneshot::Sender<()>,
        served: JoinHandle<()>,
        records: Records,
    }

    impl Running {
        async fn stop(self) -> Records {
            self.stop.send(()).unwrap();
            self.served.await.unwrap();
            self.records
        }
    }

    fn start(dir: &Path) -> Result<Running, Fault> {
        start_as(dir, |_| {})
    }

    // Starts a daemon, letting `change` adjust it before it serves.
    fn start_as(dir: &Path, change: impl FnOnce(&mut Daemon)) -> Result<Running, Fault> {
        let (log, records) = recording_logger();
        let mut daemon = Daemon::start(dir, build(&VERSION), log)?;
        // Tests never reach the release server.
        daemon.updates = None;
        change(&mut daemon);
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

    async fn ask(socket: &Path) -> Result<Welcome, Fault> {
        weacon_client::handshake(socket, build(&VERSION)).await
    }

    fn hello(protocol: u32) -> Vec<u8> {
        encode_line(&Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol,
                app: build(&VERSION),
                capabilities: Vec::new(),
            }),
        })
        .unwrap()
    }

    // Sends `bytes` and reads replies until the daemon hangs up or `count` arrive.
    async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
        stream: S,
        bytes: &[u8],
        count: usize,
    ) -> (Vec<Message>, S) {
        let (reader, mut writer) = tokio::io::split(stream);
        writer.write_all(bytes).await.unwrap();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let mut replies = Vec::new();
        // Longer than the handshake deadline, so a daemon that never answers fails the test.
        let wait = HANDSHAKE_TIMEOUT * 2;
        while replies.len() < count
            && let Some(line) = timeout(wait, lines.next_line()).await.unwrap().unwrap()
        {
            replies.push(serde_json::from_str::<Envelope>(&line).unwrap().message);
        }
        let reader = lines.into_inner().into_inner();
        (replies, reader.unsplit(writer))
    }

    // Waits for `message`, since the disk is read on another thread; bounded so a miss fails.
    async fn logs(records: &Records, message: &str) {
        logs_times(records, message, 1).await;
    }

    async fn logs_times(records: &Records, message: &str, times: usize) {
        let count = || {
            messages(records)
                .iter()
                .filter(|logged| **logged == message)
                .count()
        };
        let logged = async {
            while count() < times {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        timeout(Duration::from_secs(5), logged).await.unwrap();
    }

    // Reads the free space the test wrote into the folder.
    fn free_in_file(dir: &Path) -> io::Result<u64> {
        let free = fs::read_to_string(dir.join("free"))?;
        free.parse().map_err(io::Error::other)
    }

    #[tokio::test]
    async fn the_update_schedule_is_checked_at_start() {
        let dir = TempDir::new().unwrap();
        let watched = dir.to_path_buf();
        let running = start_as(&dir, |daemon| {
            let unreachable = || Err(Fault::new(code!("update.fetch_failed"), "x"));
            let own = "1.0.0".parse().unwrap();
            let watch =
                update::Watch::new(watched, own, unreachable, |_| Ok(false), || 1_800_000_000);
            daemon.updates = Some(watch);
        })
        .unwrap();
        logs(&running.records, "update check failed").await;
        running.stop().await;
    }

    #[test]
    fn only_a_build_numbered_like_a_release_watches_for_updates() {
        let dir = TempDir::new().unwrap();
        let released = App {
            version: "1.2.3".to_owned(),
            ..build(&VERSION)
        };
        assert!(updates(&dir, &released).is_some());
        let local = App {
            version: "dev".to_owned(),
            ..build(&VERSION)
        };
        assert!(updates(&dir, &local).is_none());
        assert!(unix_now() > 1_700_000_000);
    }

    #[tokio::test(start_paused = true)]
    async fn the_disk_is_checked_at_start_and_again_each_minute() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("free"), "0").unwrap();
        let watched = dir.to_path_buf();
        let running = start_as(&dir, |daemon| {
            daemon.disk = disk::Watch::new(watched, free_in_file);
        })
        .unwrap();

        logs(&running.records, "disk full").await;
        fs::write(dir.join("free"), (8_u64 << 30).to_string()).unwrap();
        tokio::time::sleep(DISK_CHECK).await;
        logs(&running.records, "disk ok").await;
        let records = running.stop().await;

        let disk: Vec<_> = messages(&records)
            .into_iter()
            .filter(|message| message.starts_with("disk "))
            .collect();
        assert_eq!(disk, ["disk full", "disk ok"]);
    }

    #[tokio::test(start_paused = true)]
    async fn memory_is_sampled_at_start_and_every_five_minutes() {
        if !platform::PROCESSES {
            return;
        }
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();

        logs(&running.records, "memory sampled").await;
        tokio::time::sleep(MEMORY_SAMPLE).await;
        logs_times(&running.records, "memory sampled", 2).await;

        running.stop().await;
    }

    async fn connect(socket: &Path) -> impl AsyncRead + AsyncWrite + Unpin + use<> {
        weacon_client::connect(socket).await.unwrap()
    }

    #[tokio::test]
    async fn a_client_shakes_hands_and_shutdown_cleans_up() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let socket = running.socket.clone();

        assert_eq!(ask(&socket).await.unwrap().app, build(&VERSION));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let records = running.stop().await;
        assert!(!socket.exists());
        assert_eq!(
            ask(&socket).await.unwrap_err().code().as_str(),
            "daemon.not_running"
        );
        let lifecycle: Vec<_> = messages(&records)
            .into_iter()
            .filter(|message| message.starts_with("daemon "))
            .collect();
        assert_eq!(lifecycle, ["daemon started", "daemon stopped"]);
    }

    #[tokio::test]
    async fn one_daemon_per_state_folder_and_a_stale_socket_is_replaced() {
        let dir = TempDir::new().unwrap();
        fs::write(socket_path(&dir), "stale").unwrap();
        let first = start(&dir).unwrap();
        let second = start(&dir).err().unwrap();

        assert_eq!(second.code().as_str(), "daemon.running");
        ask(&first.socket).await.unwrap();
        first.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_state_folder_others_can_reach_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            start(&dir).err().unwrap().code().as_str(),
            "state.not_private"
        );
        assert!(!socket_path(&dir).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn another_user_is_refused() {
        let dir = TempDir::new().unwrap();
        let running = start_as(&dir, |daemon| daemon.owner += 1).unwrap();
        let refused = ask(&running.socket).await.unwrap_err();
        let records = running.stop().await;

        assert_eq!(refused.code().as_str(), "daemon.no_answer");
        assert!(messages(&records).contains(&"client refused"));
    }

    #[tokio::test]
    async fn bad_frames_get_errors_and_the_connection_goes_on() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let mut bytes = b"not json\n".to_vec();
        bytes.extend(hello(PROTOCOL));
        let (replies, client) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let (later, _) = exchange(client, b"[]\n", 1).await;

        assert_eq!(error_code(&replies[0]), Some("frame.invalid"));
        assert!(matches!(replies[1], Message::Welcome(_)));
        assert_eq!(error_code(&later[0]), Some("frame.invalid"));
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn another_protocol_is_hung_up_on_at_once() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let other = hello(PROTOCOL + 1);
        let talk = exchange(connect(&running.socket).await, &other, 9);
        let (replies, _) = timeout(HANDSHAKE_TIMEOUT / 2, talk).await.unwrap();

        assert_eq!(replies.len(), 1);
        assert_eq!(error_code(&replies[0]), Some("protocol.unsupported"));
        running.stop().await;
    }

    #[tokio::test]
    async fn shutdown_hangs_up_on_connected_clients() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let (replies, mut client) =
            exchange(connect(&running.socket).await, &hello(PROTOCOL), 1).await;
        running.stop().await;

        assert!(matches!(replies[0], Message::Welcome(_)));
        assert_eq!(client.read(&mut [0; 16]).await.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_hello_may_come_late_but_within_the_deadline() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let client = connect(&running.socket).await;
        tokio::time::sleep(HANDSHAKE_TIMEOUT / 2).await;
        let (replies, _) = exchange(client, &hello(PROTOCOL), 1).await;

        assert!(matches!(replies[0], Message::Welcome(_)));
        running.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_never_says_hello_is_hung_up_on() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let mut client = connect(&running.socket).await;
        let read = timeout(HANDSHAKE_TIMEOUT * 2, client.read(&mut [0; 16])).await;

        assert_eq!(read.unwrap().unwrap(), 0);
        running.stop().await;
    }

    fn pick(id: u32, request: &str, html: &str) -> Vec<u8> {
        let message = Message::Pick(test_support::pick(request, html));
        encode_line(&Envelope { id, message }).unwrap()
    }

    fn receipt(message: &Message) -> Option<&Outcome> {
        let Message::Receipt(receipt) = message else {
            return None;
        };
        Some(&receipt.outcome)
    }

    #[tokio::test]
    async fn a_pick_is_kept_and_answered_with_a_receipt() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let mut bytes = hello(PROTOCOL);
        bytes.extend(pick(2, "p-1", "<b>MARKER-HTML</b>"));
        bytes.extend(pick(3, "p-1", "<b>MARKER-HTML</b>"));
        bytes.extend(pick(4, "p-1", "<i>other</i>"));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 4).await;
        let records = running.stop().await;
        let kept = Picks::new(&dir).read(&"p-1".to_owned().try_into().unwrap());

        assert!(matches!(&replies[0], Message::Welcome(welcome)
            if welcome.capabilities == [code!("element.pick").into()]));
        assert_eq!(receipt(&replies[1]), Some(&Outcome::Completed));
        assert_eq!(receipt(&replies[2]), Some(&Outcome::Completed));
        assert_eq!(error_code(&replies[3]), Some("request.conflict"));
        assert_eq!(kept.unwrap().unwrap().html, "<b>MARKER-HTML</b>");
        let picked = messages(&records);
        assert_eq!(
            picked
                .iter()
                .filter(|msg| **msg == "element picked")
                .count(),
            2
        );
        let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(logged.contains("p-1"), "{logged}");
        assert!(!logged.contains("MARKER"), "{logged}");
    }

    #[tokio::test]
    async fn a_pick_that_cant_be_kept_fails_its_receipt() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        fs::write(dir.join("picks"), "a file, not a folder").unwrap();
        let mut bytes = hello(PROTOCOL);
        bytes.extend(pick(2, "p-2", "<p/>"));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let records = running.stop().await;

        assert_eq!(
            receipt(&replies[1]),
            Some(&Outcome::Failed {
                code: code!("pick.not_kept")
            })
        );
        assert!(messages(&records).contains(&"pick not kept"));
    }

    #[tokio::test]
    async fn a_pick_before_the_hello_is_refused() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let (replies, _) =
            exchange(connect(&running.socket).await, &pick(1, "p-3", "<p/>"), 1).await;
        running.stop().await;

        assert_eq!(error_code(&replies[0]), Some("handshake.required"));
        assert!(!dir.join("picks").exists());
    }

    #[tokio::test]
    async fn a_clients_errors_are_logged_by_code_after_its_hello() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let error = encode_line(&Envelope {
            id: 2,
            message: Message::Error(ErrorBody {
                code: code!("extension.capture_failed"),
                retryable: false,
            }),
        })
        .unwrap();
        let mut bytes = hello(PROTOCOL);
        bytes.extend(&error);
        bytes.extend(pick(3, "p-4", "<p/>"));
        let (replies, _) = exchange(connect(&running.socket).await, &bytes, 2).await;
        let records = running.stop().await;
        let reported: Vec<_> = records
            .lock()
            .unwrap()
            .iter()
            .filter(|record| record.msg == "client reported an error")
            .cloned()
            .collect();

        assert_eq!(receipt(&replies[1]), Some(&Outcome::Completed));
        assert_eq!(reported.len(), 1);
        let reported = serde_json::to_string(&reported).unwrap();
        assert!(reported.contains("extension.capture_failed"), "{reported}");
    }

    // Writes 8 MiB of bad lines without reading the replies; far more than the socket buffers.
    async fn flood(mut client: impl AsyncWrite + Unpin) -> io::Result<()> {
        let junk = b"x\n".repeat(32 * 1024);
        for _ in 0..128 {
            client.write_all(&junk).await?;
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_floods_before_its_hello_is_hung_up_on() {
        let dir = TempDir::new().unwrap();
        let running = start(&dir).unwrap();
        let client = connect(&running.socket).await;

        // The daemon stops reading while its replies back up, then hangs up at the deadline.
        timeout(HANDSHAKE_TIMEOUT * 2, flood(client))
            .await
            .unwrap()
            .unwrap_err();
        running.stop().await;
    }
}
