use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use weacon_testing::TempDir;
use weacon_version::Version;

pub(crate) mod data;
pub(crate) mod proposals;
pub(crate) mod requests;
pub(crate) mod socket;

pub(crate) use data::{agent, email, fresh_account, member_id, new_task, two_teams, workspace_id};
pub(crate) use requests::{
    Client, Paired, ask, bearer, claim_body, confirm_body, error_code, finish_body, header,
    lookup_body, pair_machine, request, request_with, sign_in, start_pairing, start_sign_in,
    status,
};
pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

use crate::connections;
use crate::db::Db;
use crate::http;
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
/// is ada@example.com, with the mail it prints for tests to read and a clock they move.
pub(crate) struct Fixture {
    dir: TempDir,
    pub(crate) shared: Shared,
    pub(crate) records: Records,
    clock: Arc<AtomicU64>,
    /// How many of the printed mails tests have taken.
    taken: AtomicUsize,
}

impl Fixture {
    /// Every byte the server stored, its database and anything else in its folder.
    pub(crate) fn stored(&self) -> Vec<u8> {
        let mut stored = Vec::new();
        every_file(&self.dir, &mut stored);
        stored
    }

    pub(crate) async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = with_ada(&dir).await;
        let (log, records) = recording_logger();
        // Its own logger: off Unix it warns, and tests read the fixture's records.
        let secret = Secret::open(&dir.join("secret"), &recording_logger().0).unwrap();
        let clock = Arc::new(AtomicU64::new(START));
        let now = Arc::clone(&clock);
        let shared = Shared {
            app: weacon_appkit::build(&VERSION),
            log,
            reporter: Arc::new(weacon_appkit::reporter(
                None,
                &VERSION,
                Box::new(|_| Ok(())),
            )),
            db,
            secret: Arc::new(secret),
            head_within: connections::HEAD_WITHIN,
            // A short hello; pings far apart, since a client answers them only while its test
            // reads it. Tests about pings ask for them with `pinging`.
            sockets: crate::socket::Timing {
                hello_within: Duration::from_millis(100),
                ping_every: Duration::from_hours(1),
            },
            clock: Arc::new(Box::new(move || {
                UNIX_EPOCH + Duration::from_secs(now.load(Ordering::Relaxed))
            })),
            changes: broadcast::channel(16).0,
            chats: crate::chats::Hub::default(),
            links: Arc::default(),
            relay: Arc::default(),
            terminals: Arc::default(),
            web: WEB,
        };
        Self {
            dir,
            shared,
            records,
            clock,
            taken: AtomicUsize::new(0),
        }
    }

    /// Pings sockets every 100 ms, for tests about pings, whose clients read throughout.
    pub(crate) fn pinging(&mut self) {
        self.shared.sockets.ping_every = Duration::from_millis(100);
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

    /// Who each printed sign-in mail that no test has taken is for, and the code in it.
    pub(crate) fn letters(&self) -> Vec<(String, String)> {
        let records = self.records.lock().unwrap();
        let text = |record: &weacon_observability::Record, key: &str| {
            record.fields[key].as_str().unwrap().to_owned()
        };
        let mails = records.iter().filter(|record| {
            record.msg == "mail printed" && text(record, "subject") == sign_in::SUBJECT
        });
        mails
            .skip(self.taken.load(Ordering::Relaxed))
            .map(|mail| (text(mail, "to"), code_in(&text(mail, "text"))))
            .collect()
    }

    /// The one code the server sent that no test has taken, and who it is for.
    pub(crate) fn letter(&self) -> (String, String) {
        let [letter] = self.letters().try_into().unwrap();
        self.taken.fetch_add(1, Ordering::Relaxed);
        letter
    }
}

/// The code in a sign-in mail's `text`: its one word with a dash in the middle.
fn code_in(text: &str) -> String {
    let words = text.split(|letter: char| !letter.is_ascii_alphanumeric() && letter != '-');
    let [code] = words
        .filter(|word| word.len() == 9 && word.get(4..5) == Some("-"))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    code.to_owned()
}

/// A database holding workspace `w1`, whose member `m1` is ada@example.com.
// Appends each file's bytes under `dir`, at any depth, to `stored`.
fn every_file(dir: &Path, stored: &mut Vec<u8>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            every_file(&path, stored);
        } else {
            stored.extend(fs::read(path).unwrap());
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn what_is_stored_includes_the_files_in_the_folders_inside() {
        let fixture = Fixture::new().await;
        fs::create_dir(fixture.dir.join("nested")).unwrap();
        fs::write(fixture.dir.join("nested/file"), "MARKER-NESTED").unwrap();

        let stored = String::from_utf8_lossy(&fixture.stored()).into_owned();
        assert!(stored.contains("MARKER-NESTED"));
    }
}
