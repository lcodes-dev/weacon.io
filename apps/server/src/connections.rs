use std::io::{self, ErrorKind};
use std::time::Duration;

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::sleep;
use weacon_observability::{Field, Logger};

/// How long a connection may take to send a request's head, or sit idle between requests.
pub(crate) const HEAD_WITHIN: Duration = Duration::from_secs(10);
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

/// The next connection.
pub(crate) async fn accept(listener: &TcpListener, log: &Logger) -> TcpStream {
    retried(|| listener.accept(), log).await.0
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
                sleep(ACCEPT_BACKOFF).await;
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
    stream: TcpStream,
    router: Router,
    head_within: Duration,
    mut closing: watch::Receiver<()>,
) {
    let serving = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(head_within)
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
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
        assert_eq!(started.elapsed(), ACCEPT_BACKOFF);
        assert_eq!(messages(&records), ["accept failed"]);
        let records = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        assert!(!records.contains("MARKER"));
    }

    #[tokio::test]
    async fn a_request_head_that_never_ends_closes_the_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut slow = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (_open, closing) = watch::channel(());
        serve(stream, Router::new(), Duration::from_millis(50), closing);
        slow.write_all(b"GET / HTTP/1.1\r\nhost: test\r\n")
            .await
            .unwrap();

        // Hyper answers 408 or just hangs up; either way the connection ends.
        let mut rest = Vec::new();
        slow.read_to_end(&mut rest).await.unwrap();
    }
}
