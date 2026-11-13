use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, sleep_until};
use weacon_observability::Fault;
use weacon_protocol::{ChallengeId, SignInFinish, SignInStart, SignInStarted, SignedIn, code};

use crate::db::Attempt;
use crate::email::Email;
use crate::http::{Shared, failure, refused, report, too_many};
use crate::limit::Limit;
use crate::mail::mail_failed;
use crate::secret::{os_secrets, random_failed};

/// How long a session lasts: 30 days, in seconds.
pub(crate) const SESSION_LIFETIME: i64 = 2_592_000;
/// How many sign-ins may be worked on at once after their answer.
pub(crate) const AT_ONCE: usize = 64;
/// How long finishing a sign-in takes at least: longer than any of its database work.
pub(crate) const FINISH_WITHIN: Duration = Duration::from_millis(250);
const HOUR: Duration = Duration::from_hours(1);

/// How many sign-ins each client may start.
pub(crate) fn starts() -> Limit {
    Limit::new(30, HOUR)
}

/// How many codes each client may try.
pub(crate) fn tries() -> Limit {
    Limit::new(60, HOUR)
}

/// `POST /sign-in`: sends a code to a member, and answers with the challenge to finish with.
/// Answers the same, and as fast, whether or not the address is a member, so it can't be used
/// to find out.
pub(crate) async fn start(
    State(shared): State<Shared>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Json(body): Json<SignInStart>,
) -> Response {
    if !shared.starts.allow(from.ip(), Instant::now()) {
        return too_many(code!("sign_in.too_many"));
    }
    let challenge = match new_challenge() {
        Ok(challenge) => challenge,
        Err(fault) => return failure(&shared, fault),
    };
    if let Ok(room) = Arc::clone(&shared.sign_ins).try_acquire_owned() {
        let sent = challenge.clone();
        drop(tokio::spawn(async move {
            if let Err(fault) = send_code(&shared, &body, &sent).await {
                report(&shared, fault);
            }
            drop(room);
        }));
    }
    (StatusCode::ACCEPTED, Json(SignInStarted { challenge })).into_response()
}

async fn send_code(
    shared: &Shared,
    body: &SignInStart,
    challenge: &ChallengeId,
) -> Result<(), Fault> {
    let Ok(email) = Email::parse(&body.email) else {
        return Ok(());
    };
    let Some(workspace) = shared.db.find_workspace(&body.workspace).await? else {
        return Ok(());
    };
    let Some(member) = workspace.member_with(&email).await? else {
        return Ok(());
    };
    let code = code_text(&os_secrets()?);
    let code_hash = shared.secret.code_hash(challenge, &code);
    workspace
        .start_challenge(&member, challenge, &code_hash, shared.now())
        .await?;
    let outbox = shared.outbox.clone();
    tokio::task::spawn_blocking(move || outbox.send(&email, &code))
        .await
        .map_err(|error| mail_failed().because(error))?
}

/// `POST /sign-in/finish`: trades a challenge's code for a session token, in the same time
/// whether or not there was a code to try.
pub(crate) async fn finish(
    State(shared): State<Shared>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Json(body): Json<SignInFinish>,
) -> Response {
    let now = Instant::now();
    if !shared.tries.allow(from.ip(), now) {
        return too_many(code!("sign_in.too_many"));
    }
    let answer = match signed_in(&shared, &body).await {
        Ok(Some(signed_in)) => Json(signed_in).into_response(),
        Ok(None) => refused(code!("sign_in.failed")),
        Err(fault) => failure(&shared, fault),
    };
    sleep_until(now + shared.finish_within).await;
    answer
}

async fn signed_in(shared: &Shared, body: &SignInFinish) -> Result<Option<SignedIn>, Fault> {
    let Some(challenge) = &body.challenge else {
        return Ok(None);
    };
    let Ok(email) = Email::parse(&body.email) else {
        return Ok(None);
    };
    let Some(workspace) = shared.db.find_workspace(&body.workspace).await? else {
        return Ok(None);
    };
    let token = hex(&os_secrets()?);
    let now = shared.now();
    let attempt = Attempt {
        email: &email,
        challenge,
        code_hash: &shared.secret.code_hash(challenge, body.code.trim()),
        token_hash: &token_hash(&token),
        now,
        token_expires_at: now.saturating_add(SESSION_LIFETIME),
    };
    let Some(finished) = workspace.finish_sign_in(&attempt).await? else {
        return Ok(None);
    };
    if finished.joined {
        // Nobody listening is fine: feeds read what they missed from the database.
        shared
            .changes
            .send(workspace.id().clone())
            .unwrap_or_default();
    }
    Ok(Some(SignedIn {
        member: finished.member,
        token,
    }))
}

/// A challenge nobody can guess: 32 random bytes, in hex.
fn new_challenge() -> Result<ChallengeId, Fault> {
    ChallengeId::try_from(hex(&os_secrets()?)).map_err(|_| random_failed())
}

/// A one-time code: eight digits.
fn code_text(secret: &[u8; 32]) -> String {
    let word = secret.first_chunk().copied().unwrap_or_default();
    format!("{:08}", u64::from_le_bytes(word) % 100_000_000)
}

// A lookup table, as clippy refuses `format!` collected into a string.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xf])
        .filter_map(|nibble| DIGITS.get(usize::from(nibble)).copied().map(char::from))
        .collect()
}

/// Unkeyed: a token is too long to guess.
pub(crate) fn token_hash(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::CODE_LIFETIME;
    use crate::secret::Secret;
    use crate::test_support::{
        Fixture, finish_body, request, run, start_sign_in, status, workspace_id,
    };

    async fn finish_with(address: SocketAddr, body: &str) -> (String, Value) {
        let (head, body) = request(address, "POST /sign-in/finish", body).await;
        let body = serde_json::from_str(&body).unwrap_or_default();
        (status(&head).to_owned(), body)
    }

    #[test]
    fn codes_are_eight_digits_and_tokens_are_hex() {
        assert_eq!(code_text(&[1; 32]), "38076673");
        assert_eq!(code_text(&[0; 32]), "00000000");
        assert_eq!(hex(&[0, 10, 255]), "000aff");
        assert_eq!(token_hash("a"), token_hash("a"));
        assert_ne!(token_hash("a"), token_hash("b"));
        assert_eq!(new_challenge().unwrap().as_str().len(), 64);
    }

    #[tokio::test]
    async fn a_member_signs_in_with_the_code_sent_to_them() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let mut changes = fixture.shared.changes.subscribe();
        let (started, challenge) = start_sign_in(server.address, " Ada@Example.com ").await;
        let (to, code) = fixture.letter().await;
        let finish = finish_body("w1", "ada@example.com", &challenge, &code);
        let (finished, body) = finish_with(server.address, &finish).await;
        let token = body["token"].as_str().unwrap();
        let session_end = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap()
            .session_end(&token_hash(token), fixture.shared.now())
            .await
            .unwrap();

        assert_eq!(started, "http/1.1 202 accepted");
        assert_eq!(to, "ada@example.com");
        assert_eq!(code.len(), 8);
        assert_eq!(finished, "http/1.1 200 ok");
        assert_eq!(body["member"], "m1");
        assert_eq!(token.len(), 64);
        assert_eq!(session_end, Some(fixture.shared.now() + SESSION_LIFETIME));
        assert_eq!(changes.try_recv().unwrap().as_str(), "w1");
    }

    #[tokio::test]
    async fn a_code_sent_under_another_secret_is_refused() {
        let mut fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let dir = TempDir::new().unwrap();
        fixture.shared.secret = Arc::new(
            Secret::open(
                &dir.join("secret"),
                &crate::test_support::recording_logger().0,
            )
            .unwrap(),
        );
        let rotated = run(fixture.shared.clone()).await;
        let finish = finish_body("w1", "ada@example.com", &challenge, &code);

        let (refused, _) = finish_with(rotated.address, &finish).await;
        let (finished, _) = finish_with(server.address, &finish).await;

        assert_eq!(refused, "http/1.1 401 unauthorized");
        assert_eq!(finished, "http/1.1 200 ok");
    }

    #[tokio::test]
    async fn strangers_get_the_same_answer_and_no_code() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let mut challenges = Vec::new();
        for body in [
            json!({ "workspace": "w1", "email": "bo@example.com" }),
            json!({ "workspace": "w9", "email": "ada@example.com" }),
            json!({ "workspace": "w1", "email": "not an email" }),
            json!({ "workspace": "w1", "email": "ada@example.com" }),
        ] {
            let (head, answer) = request(server.address, "POST /sign-in", &body.to_string()).await;
            let answer: Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(status(&head), "http/1.1 202 accepted", "{body}");
            assert_eq!(answer.as_object().unwrap().len(), 1, "{body}");
            challenges.push(answer["challenge"].as_str().unwrap().len());
        }
        let (bad_id, _) = request(
            server.address,
            "POST /sign-in",
            &json!({ "workspace": "../w1", "email": "ada@example.com" }).to_string(),
        )
        .await;

        // Only the member's request leaves a letter.
        assert_eq!(fixture.letter().await.0, "ada@example.com");
        assert_eq!(challenges, [64; 4]);
        assert_eq!(status(&bad_id), "http/1.1 422 unprocessable entity");
    }

    #[tokio::test]
    async fn a_wrong_or_late_code_is_refused() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let (_, other) = start_sign_in(server.address, "ada@example.com").await;
        fixture.settled().await;
        let finish = |workspace: &str, email: &str, code: &str| {
            finish_body(workspace, email, &challenge, code)
        };
        let mut refusals = Vec::new();
        for body in [
            finish("w1", "ada@example.com", "00000000"),
            finish("w9", "ada@example.com", &code),
            finish("w1", "bo@example.com", &code),
            finish("w1", "not an email", &code),
            finish_body("w1", "ada@example.com", &other, &code),
            finish_body("w1", "ada@example.com", &Value::Null, &code),
        ] {
            refusals.push(finish_with(server.address, &body).await);
        }
        fixture.advance(CODE_LIFETIME);
        refusals.push(finish_with(server.address, &finish("w1", "ada@example.com", &code)).await);

        for (head, body) in refusals {
            assert_eq!(head, "http/1.1 401 unauthorized");
            assert_eq!(
                body,
                json!({ "code": "sign_in.failed", "retryable": false })
            );
        }
    }

    #[tokio::test]
    async fn a_strangers_guesses_cant_stop_a_member_signing_in() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, theirs) = start_sign_in(server.address, "ada@example.com").await;
        fixture.letter().await;
        let (_, mine) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let unknown = json!("0".repeat(64));
        for (guess, challenge) in (0..10).zip([&theirs, &unknown].into_iter().cycle()) {
            let wrong = finish_body("w1", "ada@example.com", challenge, &format!("{guess:08}"));
            finish_with(server.address, &wrong).await;
        }
        let (finished, _) = finish_with(
            server.address,
            &finish_body("w1", "ada@example.com", &mine, &code),
        )
        .await;

        assert_eq!(finished, "http/1.1 200 ok");
    }

    #[tokio::test]
    async fn each_client_may_start_and_try_only_so_many_times() {
        let mut fixture = Fixture::new().await;
        fixture.shared.starts = Arc::new(Limit::new(1, HOUR));
        fixture.shared.tries = Arc::new(Limit::new(1, HOUR));
        let server = run(fixture.shared.clone()).await;
        let (first, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let (head, body) = request(
            server.address,
            "POST /sign-in",
            &json!({ "workspace": "w1", "email": "ada@example.com" }).to_string(),
        )
        .await;
        fixture.settled().await;
        let finish = finish_body("w1", "ada@example.com", &challenge, "00000000");
        let wrong = finish_with(server.address, &finish).await;
        let right = finish_body("w1", "ada@example.com", &challenge, &code);
        let (too_many, answer) = finish_with(server.address, &right).await;
        let refused = json!({ "code": "sign_in.too_many", "retryable": true });

        assert_eq!(first, "http/1.1 202 accepted");
        assert_eq!(status(&head), "http/1.1 429 too many requests");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), refused);
        assert!(fixture.letters().is_empty());
        assert_eq!(wrong.0, "http/1.1 401 unauthorized");
        assert_eq!(too_many, "http/1.1 429 too many requests");
        assert_eq!(answer, refused);
    }

    #[tokio::test]
    async fn a_code_that_cant_be_sent_is_logged_and_answered_the_same() {
        let fixture = Fixture::new().await;
        let mut shared = fixture.shared.clone();
        shared.outbox = crate::mail::Outbox::new("missing-outbox".into());
        let server = run(shared).await;
        let (started, _) = start_sign_in(server.address, "ada@example.com").await;
        fixture.settled().await;
        let failed = fixture
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.msg == "request failed")
            .map(|record| record.fields["err"]["code"].clone());

        assert_eq!(started, "http/1.1 202 accepted");
        assert_eq!(failed.unwrap(), "mail.failed");
    }

    #[tokio::test]
    async fn when_too_many_are_waiting_more_are_dropped_with_the_same_answer() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let every = u32::try_from(AT_ONCE).unwrap();
        let busy = Arc::clone(&fixture.shared.sign_ins)
            .try_acquire_many_owned(every)
            .unwrap();
        let (started, _) = start_sign_in(server.address, "ada@example.com").await;
        drop(busy);
        fixture.settled().await;

        assert_eq!(started, "http/1.1 202 accepted");
        assert!(fixture.letters().is_empty());
    }

    #[tokio::test]
    async fn finishing_takes_the_same_time_for_a_stranger() {
        let mut fixture = Fixture::new().await;
        fixture.shared.finish_within = Duration::from_millis(200);
        let server = run(fixture.shared.clone()).await;
        let stranger = json!({ "workspace": "w9", "email": "bo@example.com", "code": "00000000" });
        let started = std::time::Instant::now();
        let (head, _) = request(
            server.address,
            "POST /sign-in/finish",
            &stranger.to_string(),
        )
        .await;

        assert_eq!(status(&head), "http/1.1 401 unauthorized");
        assert!(started.elapsed() >= fixture.shared.finish_within);
    }

    #[tokio::test]
    async fn a_failing_database_is_answered_as_worth_trying_again() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        fixture.shared.db.close().await;
        let finish = finish_body("w1", "ada@example.com", &json!("0".repeat(64)), "00000000");
        let (head, body) = request(server.address, "POST /sign-in/finish", &finish).await;

        assert_eq!(status(&head), "http/1.1 500 internal server error");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({ "code": "db.failed", "retryable": true })
        );
    }
}
