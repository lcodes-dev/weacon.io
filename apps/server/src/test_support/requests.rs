//! Requests tests make to a running server.

use std::net::SocketAddr;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use weacon_protocol::{
    AccountSignInFinish, AccountSignInStart, ChallengeId, MachineId, MachineKey, MachineOs,
    PairClaim, PairConfirm, PairLookup, PairStart, PairStarted, SignInStarted,
};

use super::{Fixture, PATIENCE};

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
    tokio::time::timeout(PATIENCE, stream.read_to_string(&mut response))
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
pub(crate) async fn start_sign_in(address: SocketAddr, email: &str) -> (String, ChallengeId) {
    let start = body(&AccountSignInStart {
        email: email.to_owned(),
    });
    let (head, body) = request(address, "POST /api/sign-in", &start).await;
    let started: SignInStarted = serde_json::from_str(&body).unwrap();
    (status(&head).to_owned(), started.challenge)
}

/// A `POST /api/sign-in/finish` body.
pub(crate) fn finish_body(email: &str, challenge: &ChallengeId, code: &str) -> String {
    body(&AccountSignInFinish {
        email: email.to_owned(),
        code: code.to_owned(),
        challenge: challenge.clone(),
    })
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
pub(crate) fn error_code(body: &str) -> String {
    let error: weacon_protocol::ErrorBody = serde_json::from_str(body).unwrap();
    error.code.as_str().to_owned()
}

/// The header that proves a sign-in whose answer was `body`.
pub(crate) fn bearer(body: &Value) -> String {
    authorization(body["token"].as_str().unwrap())
}

/// The header that proves the sign-in whose session token is `token`, or none when it is empty.
fn authorization(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
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

impl Paired {
    pub(crate) fn machine_id(&self) -> MachineId {
        self.machine["id"].as_str().unwrap().parse().unwrap()
    }
}

/// A `POST /api/machines/pair` body.
pub(crate) fn pairing_start(os: MachineOs) -> String {
    body(&PairStart {
        name: "Studio".parse().unwrap(),
        os,
        key: MachineKey::from_bytes([0xab; 32]),
    })
}

/// Starts pairing a machine running `os`: the answer's status line, and the pairing.
pub(crate) async fn start_pairing(address: SocketAddr, os: MachineOs) -> (String, PairStarted) {
    let (head, body) = request(address, "POST /api/machines/pair", &pairing_start(os)).await;
    (
        status(&head).to_owned(),
        serde_json::from_str(&body).unwrap(),
    )
}

/// A `POST /api/machines/pair/lookup` body.
pub(crate) fn lookup_body(code: &str) -> String {
    body(&PairLookup {
        code: code.to_owned(),
    })
}

/// A `POST /api/machines/pair/confirm` body for `started`, with its code.
pub(crate) fn confirm_body(started: &PairStarted) -> String {
    body(&PairConfirm {
        pairing: started.pairing.clone(),
        code: started.code.clone(),
    })
}

/// A `POST /api/machines/pair/claim` body for `started`, with its secret.
pub(crate) fn claim_body(started: &PairStarted) -> String {
    body(&PairClaim {
        pairing: started.pairing.clone(),
        secret: started.secret.clone(),
    })
}

fn body(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

/// Pairs a machine with ada, and claims it.
pub(crate) async fn pair_machine(fixture: &Fixture, address: SocketAddr) -> Paired {
    let (_, ada) = sign_in(fixture, address, "ada@example.com").await;
    let proof = bearer(&ada);
    let (_, started) = start_pairing(address, MachineOs::Linux).await;
    let confirm = confirm_body(&started);
    let (_, machine) = ask(address, "POST /api/machines/pair/confirm", &proof, &confirm).await;
    let claim = claim_body(&started);
    let (_, claimed) = ask(address, "POST /api/machines/pair/claim", "", &claim).await;
    Paired {
        proof,
        session: ada["token"].as_str().unwrap_or_default().to_owned(),
        machine,
        token: claimed["token"].as_str().unwrap_or_default().to_owned(),
    }
}

/// A client of the server at `address` that proves the sign-in whose session token is
/// `token`, or sends no proof when it is empty.
pub(crate) struct Client {
    pub(crate) address: SocketAddr,
    pub(crate) token: String,
}

impl Client {
    /// The header that proves the client's sign-in, if it has one.
    pub(crate) fn proof(&self) -> String {
        authorization(&self.token)
    }

    /// One request: the answer's status line, and its body.
    pub(crate) async fn send(&self, head: &str, body: &str) -> (String, String) {
        let (head, body) = request_with(self.address, head, &self.proof(), body).await;
        (status(&head).to_owned(), body)
    }
}
