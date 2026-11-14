//! What the server's web sockets share: the handshake, sends that can't stall, and pings.

use std::time::Duration;

use axum::extract::ws::{Message as Frame, Utf8Bytes, WebSocket};
use futures_util::{Sink, SinkExt};
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at, timeout};
use weacon_protocol::{Capability, Code, Envelope, ErrorBody, Hello, Message, code};

use crate::http::Shared;

/// How long a WebSocket waits for the client's hello, and how often it checks that the client
/// answers and may still have it; a send that takes longer than that ends it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub(crate) hello_within: Duration,
    pub(crate) ping_every: Duration,
}

pub(crate) const TIMING: Timing = Timing {
    hello_within: Duration::from_secs(10),
    ping_every: Duration::from_secs(30),
};
/// The largest message a client may send: 16 KiB.
pub(crate) const MAX_MESSAGE: usize = 16_384;

/// A WebSocket's pings, which end it once two go unanswered: the client is gone, or its
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

    /// Pings, and says whether the socket goes on: not once two pings went unanswered, nor
    /// when `allowed` finds the client may no longer have it.
    pub(crate) async fn ping<S: Sink<Frame> + Unpin>(
        &mut self,
        socket: &mut S,
        allowed: impl Future<Output = bool>,
    ) -> bool {
        let open = self.unanswered < 2
            && allowed.await
            && send(socket, self.every, Frame::Ping(Vec::new().into())).await;
        self.unanswered = self.unanswered.saturating_add(1);
        open
    }

    /// Whether the socket goes on after the client sent `frame`, which counts as an answer.
    pub(crate) fn heard(&mut self, frame: Option<&Result<Frame, axum::Error>>) -> bool {
        let open = matches!(frame, Some(Ok(frame)) if !matches!(frame, Frame::Close(_)));
        if open {
            self.unanswered = 0;
        }
        open
    }
}

/// Answers the client's hello, offering `offers`, and gives it once the client is welcomed:
/// only one that speaks this protocol and offers `needs`.
pub(crate) async fn greet(
    socket: &mut WebSocket,
    shared: &Shared,
    offers: Vec<Capability>,
    needs: &Capability,
) -> Option<Hello> {
    let Some(Ok(Frame::Text(text))) = socket.recv().await else {
        return None;
    };
    let Ok(Envelope {
        id,
        message: Message::Hello(hello),
    }) = Envelope::from_json(text.as_bytes())
    else {
        reply(socket, shared, 0, error(code!("handshake.required"))).await;
        return None;
    };
    let answer = hello.answer(shared.app.clone(), offers);
    let welcomed = matches!(answer, Message::Welcome(_));
    if welcomed && !hello.capabilities.contains(needs) {
        reply(socket, shared, id, error(code!("capability.missing"))).await;
        return None;
    }
    let sent = reply(socket, shared, id, answer).await;
    (sent && welcomed).then_some(hello)
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
) -> bool {
    let Ok(json) = (Envelope { id, message }).to_json() else {
        return false;
    };
    let Ok(text) = Utf8Bytes::try_from(json) else {
        return false;
    };
    send(socket, shared.sockets.ping_every, Frame::Text(text)).await
}

/// Sends `frame`, or gives up after `within`: a client that stops reading can't hold
/// the connection.
pub(crate) async fn send<S: Sink<Frame> + Unpin>(
    socket: &mut S,
    within: Duration,
    frame: Frame,
) -> bool {
    timeout(within, socket.send(frame))
        .await
        .is_ok_and(|sent| sent.is_ok())
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
            sent.push(pings.ping(&mut socket, ready(true)).await);
        }
        let pong = Ok(Frame::Pong(Vec::new().into()));
        let heard = pings.heard(Some(&pong));
        let again = pings.ping(&mut socket, ready(true)).await;
        let refused = pings.ping(&mut socket, ready(false)).await;

        assert_eq!(sent, [true, true, false]);
        assert!(heard);
        assert!(again);
        assert!(!refused);
    }

    #[tokio::test]
    async fn a_close_or_an_end_is_the_end_of_a_socket() {
        let mut pings = Pings::new(TIMING.ping_every);
        let close = Ok(Frame::Close(None));
        let failed = Err(axum::Error::new("gone"));
        let text = Ok(Frame::Text("hi".into()));

        assert!(!pings.heard(None));
        assert!(!pings.heard(Some(&close)));
        assert!(!pings.heard(Some(&failed)));
        assert!(pings.heard(Some(&text)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_send_the_client_never_takes_gives_up() {
        let mut stuck = futures_util::sink::unfold((), |(), _: Frame| {
            std::future::pending::<Result<(), std::convert::Infallible>>()
        });
        let mut taken = futures_util::sink::drain();
        let frame = || Frame::Ping(Vec::new().into());

        assert!(!send(&mut stuck, TIMING.ping_every, frame()).await);
        assert!(send(&mut taken, TIMING.ping_every, frame()).await);
    }
}
