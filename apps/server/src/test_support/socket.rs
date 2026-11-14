//! A test client for the server's web sockets.

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message as Wire, Utf8Bytes};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use weacon_protocol::{Envelope, Hello, Message, PROTOCOL};

use super::VERSION;

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
    let hello = Envelope {
        id: 1,
        message: Message::Hello(Hello {
            protocol,
            app: weacon_appkit::build(&VERSION),
            capabilities: serde_json::from_value(json!(capabilities)).unwrap(),
        }),
    };
    let json = String::from_utf8(hello.to_json().unwrap()).unwrap();
    client.send(Wire::text(json)).await.unwrap();
}

// The client's next frame; a socket that sends nothing for this long is
// broken.
pub(crate) async fn frame(client: &mut Client) -> Option<Result<Wire, tungstenite::Error>> {
    timeout(Duration::from_secs(10), client.next())
        .await
        .expect("the socket went quiet")
}

// The client's next message, pings aside: they come every 100 ms.
pub(crate) async fn next(client: &mut Client) -> Message {
    let text = timeout(Duration::from_secs(10), text(client))
        .await
        .expect("no message in time");
    Envelope::from_json(text.as_bytes()).unwrap().message
}

pub(crate) async fn text(client: &mut Client) -> Utf8Bytes {
    loop {
        match client.next().await {
            Some(Ok(Wire::Ping(_))) => {}
            Some(Ok(Wire::Text(text))) => return text,
            other => panic!("no message: {other:?}"),
        }
    }
}

// Whether the socket ends, pings aside, before it has had ten seconds to.
pub(crate) async fn closed(client: &mut Client) -> bool {
    timeout(Duration::from_secs(10), ends(client))
        .await
        .unwrap_or(false)
}

pub(crate) async fn ends(client: &mut Client) -> bool {
    loop {
        match client.next().await {
            Some(Ok(Wire::Ping(_))) => {}
            None | Some(Ok(Wire::Close(_)) | Err(_)) => return true,
            Some(Ok(_)) => return false,
        }
    }
}
