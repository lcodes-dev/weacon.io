//! A test client for the server's web sockets.

use std::net::SocketAddr;

use axum::http::header::AUTHORIZATION;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message as Wire};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use weacon_protocol::{Envelope, Hello, Message, PROTOCOL};

use super::{PATIENCE, VERSION};

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the WebSocket at `path` with `token` as its bearer token.
pub(crate) async fn open(
    address: SocketAddr,
    path: &str,
    token: &str,
) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
    connect_async(request).await.map(|(client, _)| client)
}

pub(crate) async fn say(client: &mut Socket, capabilities: &[&str]) {
    say_as(client, PROTOCOL, capabilities).await;
}

pub(crate) async fn say_as(client: &mut Socket, protocol: u32, capabilities: &[&str]) {
    let hello = Message::Hello(Hello {
        protocol,
        app: weacon_appkit::build(&VERSION),
        capabilities: serde_json::from_value(json!(capabilities)).unwrap(),
    });
    send(
        client,
        Envelope {
            id: 1,
            message: hello,
        },
    )
    .await;
}

/// Sends `envelope` as one text frame.
pub(crate) async fn send(client: &mut Socket, envelope: Envelope) {
    let json = String::from_utf8(envelope.to_json().unwrap()).unwrap();
    client.send(Wire::text(json)).await.unwrap();
}

// The client's next frame.
pub(crate) async fn frame(client: &mut Socket) -> Option<Result<Wire, tungstenite::Error>> {
    timeout(PATIENCE, client.next())
        .await
        .expect("the socket went quiet")
}

// The client's next frame other than a ping: they come every 100 ms.
pub(crate) async fn past_pings(client: &mut Socket) -> Option<Result<Wire, tungstenite::Error>> {
    let past = async {
        loop {
            let frame = client.next().await;
            if !matches!(frame, Some(Ok(Wire::Ping(_)))) {
                return frame;
            }
        }
    };
    timeout(PATIENCE, past)
        .await
        .expect("the socket went quiet")
}

// The client's next message, pings aside.
pub(crate) async fn next(client: &mut Socket) -> Message {
    next_envelope(client).await.message
}

// The client's next envelope, pings aside.
pub(crate) async fn next_envelope(client: &mut Socket) -> Envelope {
    match past_pings(client).await {
        Some(Ok(Wire::Text(text))) => Envelope::from_json(text.as_bytes()).unwrap(),
        other => panic!("no message: {other:?}"),
    }
}

// Whether the socket ends, pings aside.
pub(crate) async fn ends(client: &mut Socket) -> bool {
    matches!(
        past_pings(client).await,
        None | Some(Ok(Wire::Close(_)) | Err(_))
    )
}

// Whether the peer answers a ping before anything but its own pings: it reads in order, so
// by then it has read all it was sent.
pub(crate) async fn pongs(client: &mut Socket) -> bool {
    client.send(Wire::Ping(Vec::new().into())).await.unwrap();
    matches!(past_pings(client).await, Some(Ok(Wire::Pong(_))))
}

// The status a WebSocket upgrade was refused with.
pub(crate) fn upgrade_status<T>(result: Result<T, tungstenite::Error>) -> u16 {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("not refused"),
    }
}
