use std::future::{Future, IntoFuture};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use weacon_appkit::random_id;
use weacon_observability::{Fault, Logger};
use weacon_protocol::{App, code};

const REQUEST_ID: &str = "x-request-id";
const GRACE: Duration = Duration::from_secs(10);
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
    let serving = axum::serve(listener, router(shared))
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
    let request_id = format!("{:032x}", random_id());
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::test_support::{Records, VERSION, messages, recording_logger};

    struct Running {
        address: SocketAddr,
        records: Records,
        stop: oneshot::Sender<()>,
        done: JoinHandle<Result<(), Fault>>,
    }

    async fn start() -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (log, records) = recording_logger();
        let (stop, stopped) = oneshot::channel();
        let shared = Shared {
            app: weacon_appkit::build(&VERSION),
            log,
        };
        let done = tokio::spawn(serve(listener, shared, async {
            drop(stopped.await);
        }));
        Running {
            address,
            records,
            stop,
            done,
        }
    }

    // One HTTP/1.1 exchange: the response's lowercased head lines, and its body.
    async fn get(address: SocketAddr, target: &str) -> (Vec<String>, String) {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let request = format!("GET {target} HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let head = head.lines().map(str::to_lowercase).collect();
        (head, body.to_owned())
    }

    fn header<'a>(head: &'a [String], name: &str) -> Option<&'a str> {
        head.iter()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
    }

    #[tokio::test]
    async fn version_answers_the_build_uncached() {
        let server = start().await;
        let (head, body) = get(server.address, "/version").await;
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
        let (head, _) = get(server.address, "/version?token=MARKER").await;
        let (missing, _) = get(server.address, "/MARKER-PATH").await;
        let records = server.records.lock().unwrap().clone();
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

    #[tokio::test(start_paused = true)]
    async fn shutdown_waits_for_a_stalled_client_only_so_long() {
        let server = start().await;
        let mut stalled = TcpStream::connect(server.address).await.unwrap();
        stalled
            .write_all(b"GET /version HTTP/1.1\r\nhost: test\r\n")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        server.stop.send(()).unwrap();

        tokio::time::timeout(GRACE * 2, server.done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            messages(&server.records),
            ["listening", "shutdown timed out"]
        );
    }

    #[tokio::test]
    async fn other_methods_are_not_logged_as_sent() {
        let server = start().await;
        let mut stream = TcpStream::connect(server.address).await.unwrap();
        stream
            .write_all(b"MARKER /version HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let records = server.records.lock().unwrap();

        assert_eq!(records[1].fields["method"], "other");
        assert_eq!(records[1].fields["route"], "/version");
    }

    #[tokio::test]
    async fn shutdown_stops_serving() {
        let server = start().await;
        server.stop.send(()).unwrap();

        server.done.await.unwrap().unwrap();
        TcpStream::connect(server.address).await.unwrap_err();
    }
}
