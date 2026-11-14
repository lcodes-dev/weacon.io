use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast, oneshot};
use tokio::task::JoinHandle;
use weacon_testing::TempDir;
use weacon_version::Version;

pub(crate) mod data;
pub(crate) mod requests;
pub(crate) mod socket;

pub(crate) use data::{agent, email, fresh_account, member_id, new_task, two_teams, workspace_id};
pub(crate) use requests::{
    Client, Paired, ask, bearer, error_code, finish_body, header, pair_machine, pairing_start,
    request, request_with, sign_in, start_sign_in, status,
};
pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

use crate::connections;
use crate::db::Db;
use crate::http;
use crate::outbox::Outbox;
use crate::pairing;
use crate::secret::Secret;
use crate::shared::Shared;
use crate::sign_in;

pub(crate) const VERSION: Version = Version {
    name: "weacon-server",
    version: "0.1.0",
    commit: "1a2b3c4",
};

/// Made-up built files of the web app, which tests serve.
pub(crate) const WEB: &[crate::web::WebFile] = &[
    (
        "/",
        "text/html; charset=utf-8",
        b"<!doctype html><title>Made up</title>",
    ),
    ("/app.js", "text/javascript; charset=utf-8", b"made_up();"),
    ("/app.css", "text/css; charset=utf-8", b"body {}"),
];

/// How long a test waits for an answer before taking the server as broken.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

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
        let db = with_ada(&dir).await;
        let (log, records) = recording_logger();
        // Its own logger: off Unix it warns, and tests read the fixture's records.
        let secret = Secret::open(&dir.join("secret"), &recording_logger().0).unwrap();
        fs::create_dir(dir.join("outbox")).unwrap();
        let clock = Arc::new(AtomicU64::new(START));
        let now = Arc::clone(&clock);
        let shared = Shared {
            app: weacon_appkit::build(&VERSION),
            log,
            reporter: Arc::new(weacon_appkit::reporter(None, true, Box::new(|_| Ok(())))),
            db,
            outbox: Outbox::new(dir.join("outbox")),
            secret: Arc::new(secret),
            head_within: connections::HEAD_WITHIN,
            proxies: Arc::new([]),
            connections: Arc::new(Semaphore::new(64)),
            // Short, so tests that wait for the feed's checks don't wait long.
            sockets: crate::socket::Timing {
                hello_within: Duration::from_millis(100),
                ping_every: Duration::from_millis(100),
            },
            clock: Arc::new(Box::new(move || {
                UNIX_EPOCH + Duration::from_secs(now.load(Ordering::Relaxed))
            })),
            changes: broadcast::channel(16).0,
            unpaired: broadcast::channel(16).0,
            links: Arc::default(),
            relay: Arc::default(),
            sign_ins: Arc::new(Semaphore::new(sign_in::AT_ONCE)),
            finish_takes: Duration::ZERO,
            sign_in_limits: Arc::new(sign_in::limits()),
            sign_up: Arc::new([]),
            pairing_limits: Arc::new(pairing::limits()),
            web: WEB,
        };
        Self {
            dir,
            shared,
            records,
            clock,
        }
    }

    /// Pings sockets no more while a test runs, for tests whose client stops reading or that
    /// wait on something other than a ping.
    pub(crate) fn quiet(&mut self) {
        self.shared.sockets.ping_every = Duration::from_hours(1);
    }

    /// A server running on the fixture's state.
    pub(crate) async fn run(&self) -> Running {
        run(self.shared.clone()).await
    }

    /// A server running on the fixture's state, which stops once dropped, and a client that
    /// `email` signed in to it.
    pub(crate) async fn client(&self, email: &str) -> (Running, Client) {
        let server = self.run().await;
        let client = self.sign_in_client(server.address, email).await;
        (server, client)
    }

    /// A client that `email` signed in to the server at `address`.
    pub(crate) async fn sign_in_client(&self, address: SocketAddr, email: &str) -> Client {
        let (_, body) = sign_in(self, address, email).await;
        Client {
            address,
            token: body["token"].as_str().unwrap().to_owned(),
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
        letter_in(&file)
    }
}

/// Who the letter in `file` is for and its code, taking it out of the outbox.
pub(crate) fn letter_in(file: &Path) -> (String, String) {
    let letter: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    fs::remove_file(file).unwrap();
    let text = |key: &str| letter[key].as_str().unwrap().to_owned();
    (text("to"), text("code"))
}

/// A database holding workspace `w1`, whose member `m1` is ada@example.com.
async fn with_ada(dir: &TempDir) -> Db {
    let db = Db::create(&dir.join("weacon.db")).await.unwrap();
    let workspace = db
        .create_workspace(&workspace_id("w1"), "Team")
        .await
        .unwrap();
    let ada = email("ada@example.com");
    let (m1, account) = (member_id("m1"), fresh_account());
    workspace.add_member(&m1, &account, &ada).await.unwrap();
    db
}

/// A server running on a port of its own until `stop` is sent.
pub(crate) struct Running {
    pub(crate) address: SocketAddr,
    pub(crate) stop: oneshot::Sender<()>,
    pub(crate) done: JoinHandle<()>,
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
