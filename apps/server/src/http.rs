use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
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
use tokio::sync::{Semaphore, broadcast, oneshot};
use weacon_observability::{Clock, Fault, Field, Logger, Reporter};
use weacon_protocol::{App, Code, ErrorBody, WorkspaceId, code};

use crate::db::Db;
use crate::feed;
use crate::limit::Limit;
use crate::mail::Outbox;
use crate::sign_in;

const REQUEST_ID: &str = "x-request-id";
const GRACE: Duration = Duration::from_secs(10);
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
    pub(crate) clock: Arc<Clock>,
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
}

/// Serves `listener` until `shutdown` finishes, then lets open requests end for up to
/// [`GRACE`].
pub(crate) async fn serve(
    listener: TcpListener,
    shared: Shared,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Fault> {
    if let Ok(address) = listener.local_addr() {
        shared
            .log
            .info("listening", &[("address", address.to_string().into())]);
    }
    let log = shared.log.clone();
    let failed = |error| {
        Fault::new(code!("server.serve_failed"), "The server stopped serving").because(error)
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let app = router(shared).into_make_service_with_connect_info::<SocketAddr>();
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(async { drop(stopped.await) })
        .into_future();
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => return result.map_err(failed),
        () = shutdown => {}
    }
    stop.send(()).unwrap_or_default();
    tokio::time::timeout(GRACE, serving)
        .await
        .unwrap_or_else(|_| {
            log.warn("shutdown timed out", &[]);
            Ok(())
        })
        .map_err(failed)
}

fn router(shared: Shared) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/sign-in", post(sign_in::start))
        .route("/sign-in/finish", post(sign_in::finish))
        .route("/workspaces/{workspace}/feed", get(feed::feed))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(shared.clone(), log_request))
        .with_state(shared)
}

async fn version(State(shared): State<Shared>) -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(shared.app))
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

fn error(status: StatusCode, code: Code, retryable: bool) -> Response {
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

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::test_support::{Fixture, Running, messages, request, run};

    struct Server {
        running: Running,
        fixture: Fixture,
    }

    async fn start() -> Server {
        let fixture = Fixture::new().await;
        let running = run(fixture.shared.clone()).await;
        Server { running, fixture }
    }

    async fn get(address: SocketAddr, target: &str) -> (Vec<String>, String) {
        request(address, &format!("GET {target}"), "").await
    }

    fn header<'a>(head: &'a [String], name: &str) -> Option<&'a str> {
        head.iter()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
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

    #[tokio::test]
    async fn shutdown_waits_for_a_stalled_client_only_so_long() {
        let server = start().await;
        // The pools time out on a paused clock, so pause only after they're open.
        tokio::time::pause();
        let mut stalled = TcpStream::connect(server.running.address).await.unwrap();
        stalled
            .write_all(b"GET /version HTTP/1.1\r\nhost: test\r\n")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        server.running.stop.send(()).unwrap();

        tokio::time::timeout(GRACE * 2, server.running.done)
            .await
            .unwrap()
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

        server.running.done.await.unwrap().unwrap();
        TcpStream::connect(server.running.address)
            .await
            .unwrap_err();
    }
}
