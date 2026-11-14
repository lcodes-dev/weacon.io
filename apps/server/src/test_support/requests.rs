//! Requests tests make to a running server.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::Fixture;

/// One HTTP/1.1 exchange: the response's lowercased head lines, and its body.
pub(crate) async fn request(address: SocketAddr, head: &str, body: &str) -> (Vec<String>, String) {
    request_with(address, head, "", body).await
}

/// One HTTP/1.1 exchange with `headers` added, each ending in `\r\n`.
pub(crate) async fn request_with(
    address: SocketAddr,
    head: &str,
    headers: &str,
    body: &str,
) -> (Vec<String>, String) {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "{head} HTTP/1.1\r\n{headers}host: test\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.read_to_string(&mut response),
    )
    .await
    .expect("no answer")
    .unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let head = head.lines().map(str::to_lowercase).collect();
    (head, body.to_owned())
}

/// A response's lowercased status line.
pub(crate) fn status(head: &[String]) -> &str {
    head.first().map_or("", String::as_str)
}

/// The value of the header `name`, written in lower case, in a response's `head`.
pub(crate) fn header<'a>(head: &'a [String], name: &str) -> Option<&'a str> {
    head.iter()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
}

/// Starts a sign-in for `email`: the answer's status line, and its challenge.
pub(crate) async fn start_sign_in(address: SocketAddr, email: &str) -> (String, Value) {
    let start = json!({ "email": email }).to_string();
    let (head, body) = request(address, "POST /api/sign-in", &start).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    (status(&head).to_owned(), body["challenge"].clone())
}

/// A `POST /api/sign-in/finish` body.
pub(crate) fn finish_body(email: &str, challenge: &Value, code: &str) -> String {
    json!({ "email": email, "challenge": challenge, "code": code }).to_string()
}

/// Signs `email` in with the code the server sends: the answer's status line and body.
pub(crate) async fn sign_in(
    fixture: &Fixture,
    address: SocketAddr,
    email: &str,
) -> (String, Value) {
    let (_, challenge) = start_sign_in(address, email).await;
    let (_, code) = fixture.letter().await;
    let finish = finish_body(email, &challenge, &code);
    let (head, body) = request(address, "POST /api/sign-in/finish", &finish).await;
    (
        status(&head).to_owned(),
        serde_json::from_str(&body).unwrap(),
    )
}

/// The code of the refusal whose JSON body is `body`.
pub(crate) fn refusal(body: &str) -> String {
    let error: weacon_protocol::ErrorBody = serde_json::from_str(body).unwrap();
    error.code.as_str().to_owned()
}

/// The header that proves a sign-in whose answer was `body`.
pub(crate) fn bearer(body: &Value) -> String {
    let token = body["token"].as_str().unwrap();
    format!("authorization: Bearer {token}\r\n")
}

/// One request with `headers` and `body`: the answer's status line, and its JSON body or
/// null.
pub(crate) async fn ask(
    address: SocketAddr,
    head: &str,
    headers: &str,
    body: &str,
) -> (String, Value) {
    let (head, body) = request_with(address, head, headers, body).await;
    let body = serde_json::from_str(&body).unwrap_or_default();
    (status(&head).to_owned(), body)
}

/// A machine paired with ada: the header proving her sign-in, her session's token, the
/// machine as the confirmation gave it, and its token.
pub(crate) struct Paired {
    pub(crate) proof: String,
    pub(crate) session: String,
    pub(crate) machine: Value,
    pub(crate) token: String,
}

/// A `POST /api/machines/pair` body.
pub(crate) fn pairing_start(os: &str) -> String {
    json!({ "name": "Studio", "os": os, "key": "ab".repeat(32) }).to_string()
}

/// Pairs a machine with ada, and claims it.
pub(crate) async fn pair_machine(fixture: &Fixture, address: SocketAddr) -> Paired {
    let (_, ada) = sign_in(fixture, address, "ada@example.com").await;
    let proof = bearer(&ada);
    let (_, pairing) = ask(
        address,
        "POST /api/machines/pair",
        "",
        &pairing_start("linux"),
    )
    .await;
    let confirm = json!({ "pairing": pairing["pairing"], "code": pairing["code"] }).to_string();
    let (_, machine) = ask(address, "POST /api/machines/pair/confirm", &proof, &confirm).await;
    let claim = json!({ "pairing": pairing["pairing"], "secret": pairing["secret"] }).to_string();
    let (_, claimed) = ask(address, "POST /api/machines/pair/claim", "", &claim).await;
    Paired {
        proof,
        session: ada["token"].as_str().unwrap_or_default().to_owned(),
        machine,
        token: claimed["token"].as_str().unwrap_or_default().to_owned(),
    }
}

/// A client of the server at `address` that proves a sign-in with `auth`, or sends no proof
/// when it is empty.
pub(crate) struct Client {
    pub(crate) address: SocketAddr,
    pub(crate) auth: String,
}

impl Client {
    /// One request: the answer's status line, and its body.
    pub(crate) async fn send(&self, head: &str, body: &str) -> (String, String) {
        let (head, body) = request_with(self.address, head, &self.auth, body).await;
        (status(&head).to_owned(), body)
    }
}
