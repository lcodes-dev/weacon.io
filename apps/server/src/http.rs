use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast, watch};
use tokio::time::timeout;
use weacon_observability::{Clock, Fault, Field, Logger, Reporter};
use weacon_protocol::{App, Code, ErrorBody, WorkspaceId, code};

use crate::client::Net;
use crate::connections;
use crate::db::Db;
use crate::feed;
use crate::limit::Limit;
use crate::mail::Outbox;
use crate::secret::Secret;
use crate::sign_in;
use crate::tasks;
use crate::web::{self, WebFile};
use crate::workspaces;

const REQUEST_ID: &str = "x-request-id";
const GRACE: Duration = Duration::from_secs(10);
/// How long a request may take from its head to its answer, its body included.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);
/// 16 KiB.
const MAX_BODY: usize = 16_384;
static METHODS: [Method; 9] = [
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::DELETE,
    Method::PATCH,
    Method::HEAD,
    Method::OPTIONS,
    Method::CONNECT,
    Method::TRACE,
];

#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) app: App,
    pub(crate) log: Logger,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) db: Db,
    pub(crate) outbox: Outbox,
    pub(crate) secret: Arc<Secret>,
    pub(crate) clock: Arc<Clock>,
    /// How long a connection may take to send a request's head, or sit idle between requests.
    pub(crate) head_within: Duration,
    /// Proxies whose `X-Forwarded-For` is believed.
    pub(crate) proxies: Arc<[Net]>,
    /// Room for open connections, feeds included; when it's full, more wait to be accepted.
    pub(crate) connections: Arc<Semaphore>,
    pub(crate) feed: feed::Timing,
    /// Which workspace just changed, for the feeds.
    pub(crate) changes: broadcast::Sender<WorkspaceId>,
    /// Room for sign-ins being worked on after their answer; when it's full, more are dropped.
    pub(crate) sign_ins: Arc<Semaphore>,
    /// How long finishing a sign-in takes at least, so its timing gives nothing away.
    pub(crate) finish_within: Duration,
    /// How many sign-ins each client may start.
    pub(crate) starts: Arc<Limit>,
    /// How many codes each client may try.
    pub(crate) tries: Arc<Limit>,
    /// The web app's built files it serves.
    pub(crate) web: &'static [WebFile],
}

impl Shared {
    /// Seconds since the Unix epoch.
    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
            })
    }

    /// Tells the open feeds that `workspace` changed; nobody listening is fine, as feeds read
    /// what they missed from the database.
    pub(crate) fn changed(&self, workspace: &WorkspaceId) {
        drop(self.changes.send(workspace.clone()));
    }
}

/// Serves `listener` until `shutdown` finishes, then lets open requests end for up to
/// [`GRACE`].
pub(crate) async fn serve(
    listener: TcpListener,
    shared: Shared,
    shutdown: impl Future<Output = ()> + Send + 'static,
) {
    let log = shared.log.clone();
    if let Ok(address) = listener.local_addr() {
        log.info("listening", &[("address", address.to_string().into())]);
    }
    let (head_within, room) = (shared.head_within, Arc::clone(&shared.connections));
    let router = router(shared);
    let (close, _) = watch::channel(());
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accepted = connections::accept(&listener, &room, &log) => {
                connections::serve(accepted, router.clone(), head_within, close.subscribe());
            }
            () = &mut shutdown => break,
        }
    }
    drop(listener);
    close.send_replace(());
    // Every connection's task holds a receiver until it ends or becomes a feed.
    if timeout(GRACE, close.closed()).await.is_err() {
        log.warn("shutdown timed out", &[]);
    }
}

fn router(shared: Shared) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/api/sign-in", post(sign_in::start))
        .route("/api/sign-in/finish", post(sign_in::finish))
        .route("/api/session", get(sign_in::session))
        .route("/api/sign-out", post(sign_in::sign_out))
        .route(
            "/api/workspaces",
            get(workspaces::list).post(workspaces::create),
        )
        .route("/api/workspaces/{workspace}", get(workspaces::read))
        .route("/api/workspaces/{workspace}/feed", get(feed::feed))
        .route(
            "/api/workspaces/{workspace}/tasks",
            get(tasks::list).post(tasks::create),
        )
        .route(
            "/api/workspaces/{workspace}/tasks/{task}",
            post(tasks::change),
        )
        .merge(web::routes(shared.web))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::map_response(uncached))
        .layer(middleware::from_fn(in_time))
        .layer(middleware::from_fn_with_state(shared.clone(), log_request))
        .with_state(shared)
}

async fn version(State(shared): State<Shared>) -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(shared.app))
}

/// Keeps every answer out of caches unless its handler says otherwise: most are one person's.
async fn uncached(mut response: Response) -> Response {
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

/// Answers that the request took too long, when it has: a body that never comes, for one.
async fn in_time(request: Request, next: Next) -> Response {
    timeout(ANSWER_WITHIN, next.run(request))
        .await
        .unwrap_or_else(|_| error(StatusCode::REQUEST_TIMEOUT, code!("request.too_slow"), true))
}

/// Logs one record per request: never its path or query, which can hold content, only the
/// route it matched.
async fn log_request(State(shared): State<Shared>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = crate::app::new_id();
    // Any other method is a client's own text.
    let method = METHODS
        .iter()
        .find(|known| *known == request.method())
        .map_or("other", Method::as_str);
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let mut response = next.run(request).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    shared.log.info(
        "request handled",
        &[
            ("method", method.into()),
            ("route", route.into()),
            ("status", response.status().as_u16().into()),
            ("duration_ms", duration_ms.into()),
            ("request_id", request_id.into()),
        ],
    );
    response
}

/// Answers that the client must sign in, or try again, with `code`.
pub(crate) fn refused(code: Code) -> Response {
    error(StatusCode::UNAUTHORIZED, code, false)
}

/// Answers that the request's body isn't one the server can take, with `code`.
pub(crate) fn invalid(code: Code) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, code, false)
}

/// Answers that the request isn't from the server's own pages.
pub(crate) fn cross_site() -> Response {
    error(StatusCode::FORBIDDEN, code!("request.cross_site"), false)
}

/// Answers that this client has asked too often for now.
pub(crate) fn too_many(code: Code) -> Response {
    error(StatusCode::TOO_MANY_REQUESTS, code, true)
}

/// Logs and reports `fault`, and answers that the server failed.
pub(crate) fn failure(shared: &Shared, fault: Fault) -> Response {
    let code = fault.code().clone();
    report(shared, fault);
    error(StatusCode::INTERNAL_SERVER_ERROR, code, true)
}

pub(crate) fn error(status: StatusCode, code: Code, retryable: bool) -> Response {
    (status, Json(ErrorBody { code, retryable })).into_response()
}

/// Logs and reports `fault`.
pub(crate) fn report(shared: &Shared, fault: Fault) {
    shared
        .log
        .error("request failed", &[("err", Field::error(&fault))]);
    let (reporter, log) = (Arc::clone(&shared.reporter), shared.log.clone());
    // Sending a report blocks, so it runs beside the request rather than holding it up.
    drop(tokio::task::spawn_blocking(move || {
        reporter.capture_error(&fault, fault.code(), &log);
    }));
}

#[cfg(test)]
mod tests {

    use std::net::SocketAddr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::test_support::{Fixture, Running, header, messages, request, run};

    struct Server {
        running: Running,
        fixture: Fixture,
    }

    async fn start() -> Server {
        start_with(|_| {}).await
    }

    async fn start_with(change: impl FnOnce(&mut Shared)) -> Server {
        let mut fixture = Fixture::new().await;
        change(&mut fixture.shared);
        let running = run(fixture.shared.clone()).await;
        Server { running, fixture }
    }

    async fn get(address: SocketAddr, target: &str) -> (Vec<String>, String) {
        request(address, &format!("GET {target}"), "").await
    }

    #[tokio::test]
    async fn version_answers_the_build_uncached() {
        let server = start().await;
        let (head, body) = get(server.running.address, "/version").await;
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(head[0], "http/1.1 200 ok");
        assert_eq!(header(&head, "cache-control"), Some("no-store"));
        assert_eq!(
            body,
            serde_json::json!({"name": "weacon-server", "version": "0.1.0", "commit": "1a2b3c4"})
        );
    }

    #[tokio::test]
    async fn each_request_is_logged_once_with_its_route_and_id() {
        let server = start().await;
        let (head, _) = get(server.running.address, "/version?token=MARKER").await;
        let (missing, _) = get(server.running.address, "/MARKER-PATH").await;
        let records = server.fixture.records.lock().unwrap().clone();
        let requests: Vec<_> = records
            .iter()
            .filter(|record| record.msg == "request handled")
            .collect();
        let id = header(&head, REQUEST_ID).unwrap();

        assert_eq!(records[0].msg, "listening");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].fields["method"], "GET");
        assert_eq!(requests[0].fields["route"], "/version");
        assert_eq!(requests[0].fields["status"], 200);
        assert!(requests[0].fields["duration_ms"].is_u64());
        assert_eq!(requests[0].fields["request_id"], id);
        assert_eq!(id.len(), 32);
        assert_eq!(missing[0], "http/1.1 404 not found");
        assert_eq!(requests[1].fields["route"], "unmatched");
        assert_ne!(requests[1].fields["request_id"], id);
        assert!(!serde_json::to_string(&records).unwrap().contains("MARKER"));
    }

    async fn ask(stream: &mut TcpStream, target: &str) {
        let request = format!("GET {target} HTTP/1.1\r\nhost: test\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
    }

    // Asks for `target` on a connection that stays open, and reads the answer's status line.
    async fn exchange(stream: &mut TcpStream, target: &str) -> String {
        ask(stream, target).await;
        answer(stream).await
    }

    async fn answer(stream: &mut TcpStream) -> String {
        let mut answer = String::new();
        // Every answer here is a JSON object, or has no body.
        let whole = |answer: &str| {
            answer.ends_with('}')
                || answer.contains("content-length: 0\r\n") && answer.ends_with("\r\n\r\n")
        };
        while !whole(&answer) {
            let mut some = [0; 1024];
            let read = timeout(Duration::from_secs(10), stream.read(&mut some))
                .await
                .expect("no answer")
                .unwrap();
            assert_ne!(read, 0, "closed mid-answer");
            answer.push_str(&String::from_utf8_lossy(&some[..read]).to_lowercase());
        }
        answer.lines().next().unwrap().to_owned()
    }

    // What the server sends until it closes the connection, and how long that took.
    async fn until_closed(stream: &mut TcpStream) -> (String, Duration) {
        let started = tokio::time::Instant::now();
        let mut rest = String::new();
        timeout(Duration::from_hours(1), stream.read_to_string(&mut rest))
            .await
            .expect("never closed")
            .unwrap();
        (rest.to_lowercase(), started.elapsed())
    }

    // Timers fire up to a millisecond late.
    fn about(took: Duration, limit: Duration) -> bool {
        (limit..limit + Duration::from_millis(10)).contains(&took)
    }

    #[tokio::test]
    async fn a_head_that_never_ends_closes_the_connection() {
        let server = start().await;
        tokio::time::pause();
        let mut slow = TcpStream::connect(server.running.address).await.unwrap();
        slow.write_all(b"GET /version HTTP/1.1\r\nhost: test\r\n")
            .await
            .unwrap();

        let (_, took) = until_closed(&mut slow).await;

        assert!(about(took, connections::HEAD_WITHIN), "{took:?}");
        assert_eq!(messages(&server.fixture.records), ["listening"]);
    }

    #[tokio::test]
    async fn an_idle_connection_is_closed() {
        let server = start().await;
        tokio::time::pause();
        let mut idle = TcpStream::connect(server.running.address).await.unwrap();
        assert_eq!(exchange(&mut idle, "/version").await, "http/1.1 200 ok");

        let (rest, took) = until_closed(&mut idle).await;

        assert_eq!(rest, "");
        assert!(about(took, connections::HEAD_WITHIN), "{took:?}");
    }

    #[tokio::test]
    async fn a_body_that_never_comes_is_answered_as_too_slow() {
        let server = start().await;
        tokio::time::pause();
        let mut slow = TcpStream::connect(server.running.address).await.unwrap();
        slow.write_all(
            b"POST /api/sign-in HTTP/1.1\r\nhost: test\r\ncontent-type: application/json\r\ncontent-length: 9\r\n\r\n",
        )
        .await
        .unwrap();

        let (answer, took) = until_closed(&mut slow).await;

        assert!(answer.starts_with("http/1.1 408 "), "{answer}");
        assert!(answer.ends_with(r#"{"code":"request.too_slow","retryable":true}"#));
        assert!(about(took, ANSWER_WITHIN), "{took:?}");
        let records = server.fixture.records.lock().unwrap();
        assert_eq!(records[1].fields["status"], 408);
    }

    #[tokio::test]
    async fn connections_past_the_cap_wait_for_room() {
        let server = start_with(|shared| shared.connections = Arc::new(Semaphore::new(1))).await;
        let mut first = TcpStream::connect(server.running.address).await.unwrap();
        assert_eq!(exchange(&mut first, "/version").await, "http/1.1 200 ok");
        let mut second = TcpStream::connect(server.running.address).await.unwrap();
        ask(&mut second, "/nowhere").await;
        // Long after the second asked, the first is still the only one served.
        for _ in 0..20 {
            assert_eq!(exchange(&mut first, "/version").await, "http/1.1 200 ok");
        }
        drop(first);

        assert_eq!(answer(&mut second).await, "http/1.1 404 not found");
        let records = server.fixture.records.lock().unwrap();
        let routes: Vec<_> = records
            .iter()
            .filter(|record| record.msg == "request handled")
            .map(|record| record.fields["route"].as_str().unwrap())
            .collect();
        assert_eq!(routes.len(), 22);
        assert_eq!(routes.last(), Some(&"unmatched"));
    }

    #[tokio::test]
    async fn shutdown_closes_idle_connections_at_once() {
        let server = start().await;
        let mut idle = TcpStream::connect(server.running.address).await.unwrap();
        assert_eq!(exchange(&mut idle, "/version").await, "http/1.1 200 ok");
        server.running.stop.send(()).unwrap();

        server.running.done.await.unwrap();
        assert_eq!(until_closed(&mut idle).await.0, "");
        let messages = messages(&server.fixture.records);
        assert_eq!(messages, ["listening", "request handled"]);
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_stalled_client_only_so_long() {
        let server = start_with(|shared| shared.head_within = GRACE * 3).await;
        // The pools time out on a paused clock, so pause only after they're open.
        tokio::time::pause();
        let mut stalled = TcpStream::connect(server.running.address).await.unwrap();
        stalled
            .write_all(b"GET /version HTTP/1.1\r\nhost: test\r\n")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        server.running.stop.send(()).unwrap();

        timeout(GRACE * 2, server.running.done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            messages(&server.fixture.records),
            ["listening", "shutdown timed out"]
        );
    }

    #[tokio::test]
    async fn other_methods_are_not_logged_as_sent() {
        let server = start().await;
        let mut stream = TcpStream::connect(server.running.address).await.unwrap();
        stream
            .write_all(b"MARKER /version HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let records = server.fixture.records.lock().unwrap();

        assert_eq!(records[1].fields["method"], "other");
        assert_eq!(records[1].fields["route"], "/version");
    }

    #[tokio::test]
    async fn shutdown_stops_serving() {
        let server = start().await;
        server.running.stop.send(()).unwrap();

        server.running.done.await.unwrap();
        TcpStream::connect(server.running.address)
            .await
            .unwrap_err();
    }
}
