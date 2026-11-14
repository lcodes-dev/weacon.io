//! A test client for the server's web sockets.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message as Wire};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use weacon_protocol::{Envelope, Hello, Message, PROTOCOL};

use super::VERSION;

/// How long a test waits for a socket before taking it as broken.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

pub(crate) type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the WebSocket at `path` with `token` as its bearer token.
pub(crate) async fn open(
    address: SocketAddr,
    path: &str,
    token: &str,
) -> Result<Client, tungstenite::Error> {
    let mut request = format!("ws://{address}{path}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
    connect_async(request).await.map(|(client, _)| client)
}

pub(crate) async fn say(client: &mut Client, capabilities: &[&str]) {
    say_as(client, PROTOCOL, capabilities).await;
}

pub(crate) async fn say_as(client: &mut Client, protocol: u32, capabilities: &[&str]) {
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
pub(crate) async fn send(client: &mut Client, envelope: Envelope) {
    let json = String::from_utf8(envelope.to_json().unwrap()).unwrap();
    client.send(Wire::text(json)).await.unwrap();
}

// The client's next frame.
pub(crate) async fn frame(client: &mut Client) -> Option<Result<Wire, tungstenite::Error>> {
    timeout(PATIENCE, client.next())
        .await
        .expect("the socket went quiet")
}

// The client's next frame other than a ping: they come every 100 ms.
pub(crate) async fn past_pings(client: &mut Client) -> Option<Result<Wire, tungstenite::Error>> {
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
pub(crate) async fn next(client: &mut Client) -> Message {
    next_envelope(client).await.message
}

// The client's next envelope, pings aside.
pub(crate) async fn next_envelope(client: &mut Client) -> Envelope {
    match past_pings(client).await {
        Some(Ok(Wire::Text(text))) => Envelope::from_json(text.as_bytes()).unwrap(),
        other => panic!("no message: {other:?}"),
    }
}

// Whether the socket ends, pings aside.
pub(crate) async fn ends(client: &mut Client) -> bool {
    matches!(
        past_pings(client).await,
        None | Some(Ok(Wire::Close(_)) | Err(_))
    )
}

// The status a WebSocket upgrade was refused with.
pub(crate) fn refused<T>(result: Result<T, tungstenite::Error>) -> u16 {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(error) => panic!("{error}"),
        Ok(_) => panic!("not refused"),
    }
}
