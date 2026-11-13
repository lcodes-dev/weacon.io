use std::future::pending;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::{Service, service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::{Sleep, sleep};
use weacon_observability::{Field, Logger};

/// How long a connection may take to send a request's head, or sit idle between requests.
pub(crate) const HEAD_WITHIN: Duration = Duration::from_secs(10);
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);
/// How long a connection may go without taking any of what the server is sending it.
const WRITE_WITHIN: Duration = Duration::from_secs(30);

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
        .serve_connection(TokioIo::new(Unstalled::new(stream, WRITE_WITHIN)), service)
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

/// A stream whose writes fail once the other end has taken nothing for `within`, so that a
/// client that stops reading can't hold its connection, also once it has become a feed.
struct Unstalled<S> {
    stream: S,
    within: Duration,
    /// When the write that is waiting gives up.
    gives_up: Option<Pin<Box<Sleep>>>,
}

impl<S> Unstalled<S> {
    const fn new(stream: S, within: Duration) -> Self {
        Self {
            stream,
            within,
            gives_up: None,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Unstalled<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Unstalled<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Poll::Ready(written) = Pin::new(&mut self.stream).poll_write(context, bytes) {
            self.gives_up = None;
            return Poll::Ready(written);
        }
        let within = self.within;
        let gives_up = self.gives_up.get_or_insert_with(|| Box::pin(sleep(within)));
        ready!(gives_up.as_mut().poll(context));
        self.gives_up = None;
        Poll::Ready(Err(ErrorKind::TimedOut.into()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
    use tokio::time::{Instant, timeout};

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

    #[tokio::test(start_paused = true)]
    async fn a_write_nobody_takes_gives_up_and_one_taken_in_time_does_not() {
        let within = Duration::from_secs(30);
        let (ours, mut theirs) = duplex(4);
        let mut ours = Unstalled::new(ours, within);
        let started = Instant::now();

        // Four bytes fit; the rest wait for the other end to read.
        let stalled = ours.write_all(b"12345678").await.unwrap_err();

        assert_eq!(stalled.kind(), ErrorKind::TimedOut);
        assert_eq!(started.elapsed(), within);
        let reading = tokio::spawn(async move {
            let mut read = [0; 12];
            sleep(within.checked_sub(Duration::from_secs(1)).unwrap()).await;
            theirs.read_exact(&mut read[..8]).await.unwrap();
            sleep(within.checked_sub(Duration::from_secs(1)).unwrap()).await;
            theirs.read_exact(&mut read[8..]).await.unwrap();
            read
        });
        // Each wait is under the limit, though together they are over it.
        ours.write_all(b"abcdefgh").await.unwrap();
        assert_eq!(&reading.await.unwrap(), b"1234abcdefgh");
    }

    #[tokio::test(start_paused = true)]
    async fn reads_flushes_and_the_end_pass_through() {
        let (ours, mut theirs) = duplex(64);
        let mut ours = Unstalled::new(tokio::io::BufWriter::new(ours), WRITE_WITHIN);
        theirs.write_all(b"in").await.unwrap();
        let mut read = [0; 2];
        ours.read_exact(&mut read).await.unwrap();
        ours.write_all(b"out").await.unwrap();

        // Until it is flushed, what was written waits in the buffer.
        let soon = Duration::from_secs(1);
        let early = timeout(soon, theirs.read(&mut [0])).await;
        early.unwrap_err();
        ours.flush().await.unwrap();
        timeout(soon, theirs.read_exact(&mut read))
            .await
            .expect("not flushed")
            .unwrap();
        ours.shutdown().await.unwrap();
        let mut rest = Vec::new();
        timeout(soon, theirs.read_to_end(&mut rest))
            .await
            .expect("not ended")
            .unwrap();

        assert_eq!(&read, b"ou");
        assert_eq!(rest, b"t");
    }
}
