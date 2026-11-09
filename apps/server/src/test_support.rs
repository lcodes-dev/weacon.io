use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, broadcast, oneshot};
use tokio::task::JoinHandle;
use weacon_observability::Fault;
use weacon_testing::TempDir;
use weacon_version::Version;

pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

use crate::db::Db;
use crate::email::Email;
use crate::feed;
use crate::http::{self, Shared};
use crate::mail::Outbox;
use crate::sign_in;

pub(crate) const VERSION: Version = Version {
    name: "weacon-server",
    version: "0.1.0",
    commit: "1a2b3c4",
};

/// Seconds since the Unix epoch when a fixture starts.
const START: u64 = 1_762_077_600;

/// A server's shared state on a temporary database holding workspace `w1`, whose member `m1`
/// is ada@example.com, with an outbox tests read and a clock they move.
pub(crate) struct Fixture {
    dir: TempDir,
    pub(crate) shared: Shared,
    pub(crate) records: Records,
    clock: Arc<AtomicU64>,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let workspace = db
            .create_workspace(&workspace_id("w1"), "Team")
            .await
            .unwrap();
        workspace
            .add_member(&member_id("m1"), &Email::parse("ada@example.com").unwrap())
            .await
            .unwrap();
        let (log, records) = recording_logger();
        fs::create_dir(dir.join("outbox")).unwrap();
        let clock = Arc::new(AtomicU64::new(START));
        let now = Arc::clone(&clock);
        let shared = Shared {
            app: weacon_appkit::build(&VERSION),
            log,
            reporter: Arc::new(weacon_appkit::reporter(None, true, Box::new(|_| Ok(())))),
            db,
            outbox: Outbox::new(dir.join("outbox")),
            // Short, so tests that wait for the feed's checks don't wait long.
            feed: feed::Timing {
                hello_within: Duration::from_millis(100),
                ping_every: Duration::from_millis(100),
            },
            clock: Arc::new(Box::new(move || {
                UNIX_EPOCH + Duration::from_secs(now.load(Ordering::Relaxed))
            })),
            changes: broadcast::channel(16).0,
            sign_ins: Arc::new(Semaphore::new(sign_in::AT_ONCE)),
            finish_within: Duration::ZERO,
            starts: Arc::new(sign_in::starts()),
            tries: Arc::new(sign_in::tries()),
        };
        Self {
            dir,
            shared,
            records,
            clock,
        }
    }

    /// Moves the clock on by `seconds`.
    pub(crate) fn advance(&self, seconds: i64) {
        self.clock
            .fetch_add(seconds.unsigned_abs(), Ordering::Relaxed);
    }

    /// Waits until every sign-in has done its work after its answer.
    pub(crate) async fn settled(&self) {
        let every = u32::try_from(sign_in::AT_ONCE).unwrap();
        drop(self.shared.sign_ins.acquire_many(every).await.unwrap());
    }

    pub(crate) fn letters(&self) -> Vec<PathBuf> {
        fs::read_dir(self.dir.join("outbox"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect()
    }

    /// The one code the server sent once every sign-in has done its work, and who it is for.
    pub(crate) async fn letter(&self) -> (String, String) {
        self.settled().await;
        let [file] = self.letters().try_into().unwrap();
        let letter: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        fs::remove_file(&file).unwrap();
        let text = |key: &str| letter[key].as_str().unwrap().to_owned();
        (text("to"), text("code"))
    }
}

/// A server running on a port of its own until `stop` is sent.
pub(crate) struct Running {
    pub(crate) address: SocketAddr,
    pub(crate) stop: oneshot::Sender<()>,
    pub(crate) done: JoinHandle<Result<(), Fault>>,
}

pub(crate) async fn run(shared: Shared) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let done = tokio::spawn(http::serve(listener, shared, async {
        drop(stopped.await);
    }));
    Running {
        address,
        stop,
        done,
    }
}

/// One HTTP/1.1 exchange: the response's lowercased head lines, and its body.
pub(crate) async fn request(address: SocketAddr, head: &str, body: &str) -> (Vec<String>, String) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{head} HTTP/1.1\r\nhost: test\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.read_to_string(&mut response),
    )
    .await
    .expect("no answer")
    .unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let head = head.lines().map(str::to_lowercase).collect();
    (head, body.to_owned())
}

pub(crate) fn workspace_id(id: &str) -> weacon_protocol::WorkspaceId {
    weacon_protocol::WorkspaceId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn member_id(id: &str) -> weacon_protocol::MemberId {
    weacon_protocol::MemberId::try_from(id.to_owned()).unwrap()
}
