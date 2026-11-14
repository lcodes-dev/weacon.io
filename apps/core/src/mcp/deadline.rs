//! A stream whose writes fail once they make no progress for too long, so a client that stops
//! reading can't keep its connection.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Sleep, sleep};

pub(crate) struct Deadline<S> {
    stream: S,
    limit: Duration,
    /// Set while a write waits, and cleared once one goes through.
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<S> Deadline<S> {
    pub(crate) const fn new(stream: S, limit: Duration) -> Self {
        Self {
            stream,
            limit,
            stalled: None,
        }
    }

    fn progress<T>(
        &mut self,
        poll: Poll<io::Result<T>>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<T>> {
        if poll.is_ready() {
            self.stalled = None;
            return poll;
        }
        let limit = self.limit;
        let stalled = self.stalled.get_or_insert_with(|| Box::pin(sleep(limit)));
        match stalled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::ErrorKind::TimedOut.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Deadline<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Deadline<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.stream).poll_write(cx, buf);
        this.progress(poll, cx)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.stream).poll_flush(cx);
        this.progress(poll, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(5);

    #[tokio::test(start_paused = true)]
    async fn a_write_nobody_reads_fails_after_the_limit() {
        let (near, _far) = duplex(8);
        let mut stream = Deadline::new(near, LIMIT);
        let started = tokio::time::Instant::now();

        let stalled = stream.write_all(&[0; 64]).await.unwrap_err();

        assert_eq!(stalled.kind(), io::ErrorKind::TimedOut);
        assert_eq!(started.elapsed(), LIMIT);
    }

    /// Never takes a write or a flush, and refuses to shut down.
    struct Stuck;

    impl AsyncWrite for Stuck {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_flush_fails_and_a_failed_shutdown_says_so() {
        let mut stream = Deadline::new(Stuck, LIMIT);

        let flushed = stream.flush().await.unwrap_err();
        let shut = stream.shutdown().await.unwrap_err();

        assert_eq!(flushed.kind(), io::ErrorKind::TimedOut);
        assert_eq!(shut.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test(start_paused = true)]
    async fn writes_that_are_read_go_through_and_reads_pass_on() {
        let (near, mut far) = duplex(8);
        let mut stream = Deadline::new(near, LIMIT);
        let reader = tokio::spawn(async move {
            let mut got = vec![0; 64];
            far.read_exact(&mut got).await.unwrap();
            far.write_all(b"ok").await.unwrap();
            got
        });

        let mut answer = [0; 2];
        let exchange = async {
            stream.write_all(&[7; 64]).await?;
            stream.flush().await?;
            stream.read_exact(&mut answer).await
        };
        tokio::time::timeout(LIMIT, exchange)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(reader.await.unwrap(), [7; 64]);
        assert_eq!(&answer, b"ok");
        stream.shutdown().await.unwrap();
    }
}
