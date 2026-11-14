//! What the server's web sockets share: the handshake, sends that can't stall, and pings.

use std::time::Duration;

use axum::extract::ws::{Message as Frame, Utf8Bytes, WebSocket};
use futures_util::{Sink, SinkExt};
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at, timeout};
use weacon_protocol::{Capability, Code, Envelope, ErrorBody, Hello, Message, code};

use crate::http::Shared;

/// How long a web socket waits for the client's hello, and how often it checks that the
/// client answers and may still have it; a send that takes longer than that ends it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub(crate) hello_within: Duration,
    pub(crate) ping_every: Duration,
}

pub(crate) const TIMING: Timing = Timing {
    hello_within: Duration::from_secs(10),
    ping_every: Duration::from_secs(30),
};

/// The socket has ended: its client left, may have it no more, or something failed.
pub(crate) struct Closed;

/// A web socket's pings, which end it once two go unanswered: the client is gone, or its
/// connection is.
pub(crate) struct Pings {
    every: Duration,
    interval: Interval,
    unanswered: u8,
}

impl Pings {
    pub(crate) fn new(every: Duration) -> Self {
        let mut interval = interval_at(Instant::now() + every, every);
        // After a stall, one late tick rather than a burst that would count as missed answers.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            every,
            interval,
            unanswered: 0,
        }
    }

    /// When the next ping is due; safe to cancel.
    pub(crate) async fn due(&mut self) {
        self.interval.tick().await;
    }

    /// Pings, unless two pings went unanswered or `allowed` finds the client may no longer
    /// have the socket.
    pub(crate) async fn ping<S: Sink<Frame> + Unpin>(
        &mut self,
        socket: &mut S,
        allowed: impl Future<Output = Result<(), Closed>>,
    ) -> Result<(), Closed> {
        if self.unanswered >= 2 {
            return Err(Closed);
        }
        allowed.await?;
        send(socket, self.every, Frame::Ping(Vec::new().into())).await?;
        self.unanswered = self.unanswered.saturating_add(1);
        Ok(())
    }

    /// Takes `frame` from the client as an answer, or ends the socket on a close or an error.
    pub(crate) fn heard(&mut self, frame: &Result<Frame, axum::Error>) -> Result<(), Closed> {
        match frame {
            Ok(Frame::Close(_)) | Err(_) => Err(Closed),
            Ok(_) => {
                self.unanswered = 0;
                Ok(())
            }
        }
    }
}

/// Answers the client's hello, offering `offers`, and gives it once the client is welcomed:
/// only one that speaks this protocol and offers `needs`.
pub(crate) async fn greet(
    socket: &mut WebSocket,
    shared: &Shared,
    offers: Vec<Capability>,
    needs: &Capability,
) -> Result<Hello, Closed> {
    let Some(Ok(Frame::Text(text))) = socket.recv().await else {
        return Err(Closed);
    };
    let Ok(Envelope {
        id,
        message: Message::Hello(hello),
    }) = Envelope::from_json(text.as_bytes())
    else {
        reply(socket, shared, 0, error(code!("handshake.required"))).await?;
        return Err(Closed);
    };
    let answer = hello.answer(shared.app.clone(), offers);
    if !matches!(answer, Message::Welcome(_)) {
        reply(socket, shared, id, answer).await?;
        return Err(Closed);
    }
    if !hello.capabilities.contains(needs) {
        reply(socket, shared, id, error(code!("capability.missing"))).await?;
        return Err(Closed);
    }
    reply(socket, shared, id, answer).await?;
    Ok(hello)
}

pub(crate) fn error(code: Code) -> Message {
    Message::Error(ErrorBody {
        code,
        retryable: false,
    })
}

pub(crate) async fn reply(
    socket: &mut WebSocket,
    shared: &Shared,
    id: u32,
    message: Message,
) -> Result<(), Closed> {
    let json = (Envelope { id, message }).to_json().map_err(|_| Closed)?;
    let text = Utf8Bytes::try_from(json).map_err(|_| Closed)?;
    send(socket, shared.sockets.ping_every, Frame::Text(text)).await
}

/// Sends `frame`, or gives up after `within`: a client that stops reading can't hold the
/// socket.
pub(crate) async fn send<S: Sink<Frame> + Unpin>(
    socket: &mut S,
    within: Duration,
    frame: Frame,
) -> Result<(), Closed> {
    timeout(within, socket.send(frame))
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(Closed)
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn two_unanswered_pings_end_a_socket_and_an_answer_resets_them() {
        let mut socket = futures_util::sink::drain();
        let mut pings = Pings::new(TIMING.ping_every);
        let mut sent = Vec::new();
        for _ in 0..3 {
            pings.due().await;
            sent.push(pings.ping(&mut socket, ready(Ok(()))).await.is_ok());
        }
        let heard = pings.heard(&Ok(Frame::Pong(Vec::new().into()))).is_ok();
        let again = pings.ping(&mut socket, ready(Ok(()))).await.is_ok();
        let refused = pings.ping(&mut socket, ready(Err(Closed))).await.is_ok();

        assert_eq!(sent, [true, true, false]);
        assert!(heard);
        assert!(again);
        assert!(!refused);
    }

    #[tokio::test]
    async fn a_close_or_an_error_is_the_end_of_a_socket() {
        let mut pings = Pings::new(TIMING.ping_every);
        let close = Ok(Frame::Close(None));
        let failed = Err(axum::Error::new("gone"));
        let text = Ok(Frame::Text("hi".into()));

        assert!(pings.heard(&close).is_err());
        assert!(pings.heard(&failed).is_err());
        assert!(pings.heard(&text).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_the_client_never_takes_gives_up() {
        let mut stuck = futures_util::sink::unfold((), |(), _: Frame| {
            std::future::pending::<Result<(), std::convert::Infallible>>()
        });
        let mut taken = futures_util::sink::drain();
        let frame = || Frame::Ping(Vec::new().into());

        assert!(send(&mut stuck, TIMING.ping_every, frame()).await.is_err());
        assert!(send(&mut taken, TIMING.ping_every, frame()).await.is_ok());
    }
}
