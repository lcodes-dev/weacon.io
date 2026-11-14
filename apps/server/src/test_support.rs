use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, broadcast, oneshot};
use tokio::task::JoinHandle;
use weacon_testing::TempDir;
use weacon_version::Version;

pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

use crate::connections;
use crate::db::{Db, Workspace};
use crate::email::Email;
use crate::feed;
use crate::http::{self, Shared};
use crate::mail::Outbox;
use crate::secret::Secret;
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
        let ada = Email::parse("ada@example.com").unwrap();
        let (m1, account) = (member_id("m1"), fresh_account());
        workspace.add_member(&m1, &account, &ada).await.unwrap();
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
            sign_up: Arc::new([]),
            web: WEB,
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
        let letter: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        fs::remove_file(&file).unwrap();
        let text = |key: &str| letter[key].as_str().unwrap().to_owned();
        (text("to"), text("code"))
    }
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

/// One HTTP/1.1 exchange: the response's lowercased head lines, and its body.
pub(crate) async fn request(address: SocketAddr, head: &str, body: &str) -> (Vec<String>, String) {
    request_with(address, head, "", body).await
}

/// One HTTP/1.1 exchange with `headers` added, each ending in `\r\n`.
pub(crate) async fn request_with(
    address: SocketAddr,
    head: &str,
    headers: &str,
    body: &str,
) -> (Vec<String>, String) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{head} HTTP/1.1\r\n{headers}host: test\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
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

/// A response's lowercased status line.
pub(crate) fn status(head: &[String]) -> &str {
    head.first().map_or("", String::as_str)
}

/// The value of the header `name`, written in lower case, in a response's `head`.
pub(crate) fn header<'a>(head: &'a [String], name: &str) -> Option<&'a str> {
    head.iter()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
}

/// Starts a sign-in for `email`: the answer's status line, and its challenge.
pub(crate) async fn start_sign_in(address: SocketAddr, email: &str) -> (String, Value) {
    let start = json!({ "email": email }).to_string();
    let (head, body) = request(address, "POST /api/sign-in", &start).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    (status(&head).to_owned(), body["challenge"].clone())
}

/// A `POST /api/sign-in/finish` body.
pub(crate) fn finish_body(email: &str, challenge: &Value, code: &str) -> String {
    json!({ "email": email, "challenge": challenge, "code": code }).to_string()
}

/// Signs `email` in with the code the server sends: the answer's status line and body.
pub(crate) async fn sign_in(
    fixture: &Fixture,
    address: SocketAddr,
    email: &str,
) -> (String, Value) {
    let (_, challenge) = start_sign_in(address, email).await;
    let (_, code) = fixture.letter().await;
    let finish = finish_body(email, &challenge, &code);
    let (head, body) = request(address, "POST /api/sign-in/finish", &finish).await;
    (
        status(&head).to_owned(),
        serde_json::from_str(&body).unwrap(),
    )
}

/// The running server, which stops once dropped, its address, and the header that signs ada
/// in to it.
pub(crate) async fn ada(fixture: &Fixture) -> (Running, SocketAddr, String) {
    let server = run(fixture.shared.clone()).await;
    let (_, body) = sign_in(fixture, server.address, "ada@example.com").await;
    let address = server.address;
    (server, address, bearer(&body))
}

/// The code of the refusal whose JSON body is `body`.
pub(crate) fn refusal(body: &str) -> String {
    let error: weacon_protocol::ErrorBody = serde_json::from_str(body).unwrap();
    error.code.as_str().to_owned()
}

/// The header that proves a sign-in whose answer was `body`.
pub(crate) fn bearer(body: &Value) -> String {
    let token = body["token"].as_str().unwrap();
    format!("authorization: Bearer {token}\r\n")
}

/// One request with `headers` and `body`: the answer's status line, and its JSON body or
/// null.
pub(crate) async fn ask(
    address: SocketAddr,
    head: &str,
    headers: &str,
    body: &str,
) -> (String, Value) {
    let (head, body) = request_with(address, head, headers, body).await;
    let body = serde_json::from_str(&body).unwrap_or_default();
    (status(&head).to_owned(), body)
}

/// Workspaces w1 and w2 on a new database. ada@example.com is m1 in w1 and m2 in w2, and
/// bo@example.com is m3 in w1 only.
pub(crate) async fn two_teams(dir: &TempDir) -> (Workspace, Workspace) {
    let db = Db::create(&dir.join("weacon.db")).await.unwrap();
    let one = db
        .create_workspace(&workspace_id("w1"), "One")
        .await
        .unwrap();
    let two = db
        .create_workspace(&workspace_id("w2"), "Two")
        .await
        .unwrap();
    for (workspace, member, address) in [
        (&one, "m1", "ada@example.com"),
        (&two, "m2", "ada@example.com"),
        (&one, "m3", "bo@example.com"),
    ] {
        let email = Email::parse(address).unwrap();
        workspace
            .add_member(&member_id(member), &fresh_account(), &email)
            .await
            .unwrap();
    }
    (one, two)
}

pub(crate) fn workspace_id(id: &str) -> weacon_protocol::WorkspaceId {
    weacon_protocol::WorkspaceId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn member_id(id: &str) -> weacon_protocol::MemberId {
    weacon_protocol::MemberId::try_from(id.to_owned()).unwrap()
}

/// An account id nothing else has.
pub(crate) fn fresh_account() -> weacon_protocol::AccountId {
    weacon_protocol::AccountId::try_from(crate::app::new_id()).unwrap()
}
