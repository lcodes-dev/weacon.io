use std::future::pending;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::{Service, service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use weacon_observability::{Field, Logger};

/// How long a connection may take to send a request's head, or sit idle between requests.
pub(crate) const HEAD_WITHIN: Duration = Duration::from_secs(10);
const ACCEPT_PAUSE: Duration = Duration::from_secs(1);

/// A connection's place among the open ones, held until every copy is dropped. Each request
/// carries a copy, which a feed keeps for as long as it lasts.
#[derive(Clone)]
pub(crate) struct Place(
    #[expect(dead_code, reason = "held, never read")] Arc<OwnedSemaphorePermit>,
);

/// The next connection, once there is room for it.
pub(crate) async fn accept(
    listener: &TcpListener,
    room: &Arc<Semaphore>,
    log: &Logger,
) -> (TcpStream, SocketAddr, Place) {
    let Ok(place) = Arc::clone(room).acquire_owned().await else {
        // Nothing closes the semaphore.
        return pending().await;
    };
    let (stream, peer) = retried(|| listener.accept(), log).await;
    (stream, peer, Place(Arc::new(place)))
}

/// What `accept` gives once it succeeds. A connection that broke while it waited is skipped;
/// after any other failure, running out of file handles most likely, it pauses, not to spin.
async fn retried<T, F: Future<Output = io::Result<T>>>(
    mut accept: impl FnMut() -> F,
    log: &Logger,
) -> T {
    loop {
        match accept().await {
            Ok(accepted) => return accepted,
            Err(error) if broke_waiting(&error) => {}
            Err(error) => {
                log.warn("accept failed", &[("err", Field::error(&error))]);
                tokio::time::sleep(ACCEPT_PAUSE).await;
            }
        }
    }
}

fn broke_waiting(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused
    )
}

/// Serves `router` to one connection in a task of its own, which holds `closing` until the
/// connection closes; once `closing` changes, the connection ends after its open request.
pub(crate) fn serve(
    (stream, peer, place): (TcpStream, SocketAddr, Place),
    router: Router,
    head_within: Duration,
    mut closing: watch::Receiver<()>,
) {
    let router = TowerToHyperService::new(router);
    let service = service_fn(move |mut request: Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        request.extensions_mut().insert(place.clone());
        router.call(request)
    });
    let serving = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(head_within)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    drop(tokio::spawn(async move {
        tokio::pin!(serving);
        tokio::select! {
            // A connection that fails is the client's own trouble.
            _ = &mut serving => return,
            _ = closing.changed() => serving.as_mut().graceful_shutdown(),
        }
        drop(serving.await);
    }));
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use tokio::time::Instant;

    use super::*;
    use crate::test_support::{messages, recording_logger};

    #[tokio::test(start_paused = true)]
    async fn accepting_skips_broken_connections_and_pauses_after_other_failures() {
        let (log, records) = recording_logger();
        let mut results = vec![
            Ok(7),
            Err(io::Error::from(ErrorKind::ConnectionRefused)),
            Err(io::Error::other("MARKER")),
            Err(io::Error::from(ErrorKind::ConnectionReset)),
            Err(io::Error::from(ErrorKind::ConnectionAborted)),
        ];
        let started = Instant::now();

        let accepted = retried(|| ready(results.pop().unwrap()), &log).await;

        assert_eq!(accepted, 7);
        assert_eq!(started.elapsed(), ACCEPT_PAUSE);
        assert_eq!(messages(&records), ["accept failed"]);
        let records = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(!records.contains("MARKER"));
    }
}
