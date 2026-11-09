use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, sleep_until};
use weacon_observability::Fault;
use weacon_protocol::{SignInFinish, SignInStart, SignedIn, code};

use crate::db::Attempt;
use crate::email::Email;
use crate::http::{Shared, failure, refused, report};
use crate::mail::mail_failed;

/// How long a session lasts: 30 days, in seconds.
pub(crate) const SESSION_LIFETIME: i64 = 2_592_000;
/// How many sign-ins may be worked on at once after their answer.
pub(crate) const AT_ONCE: usize = 64;
/// How long finishing a sign-in takes at least: longer than any of its database work.
pub(crate) const FINISH_WITHIN: Duration = Duration::from_millis(250);

/// `POST /sign-in`: sends a code to a member. Answers the same, and as fast, whether or not
/// the address is a member, so it can't be used to find out.
pub(crate) async fn start(
    State(shared): State<Shared>,
    Json(body): Json<SignInStart>,
) -> StatusCode {
    if let Ok(room) = Arc::clone(&shared.sign_ins).try_acquire_owned() {
        drop(tokio::spawn(async move {
            if let Err(fault) = send_code(&shared, &body).await {
                report(&shared, fault);
            }
            drop(room);
        }));
    }
    StatusCode::ACCEPTED
}

async fn send_code(shared: &Shared, body: &SignInStart) -> Result<(), Fault> {
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
    if !workspace
        .save_code(&member, &hash(&code), shared.now())
        .await?
    {
        return Ok(());
    }
    let outbox = shared.outbox.clone();
    tokio::task::spawn_blocking(move || outbox.send(&email, &code))
        .await
        .map_err(|error| mail_failed().because(error))?
}

/// `POST /sign-in/finish`: trades a code for a session token, in the same time whether or not
/// there was a code to try.
pub(crate) async fn finish(
    State(shared): State<Shared>,
    Json(body): Json<SignInFinish>,
) -> Response {
    let answer_at = Instant::now() + shared.finish_within;
    let answer = match signed_in(&shared, &body).await {
        Ok(Some(signed_in)) => Json(signed_in).into_response(),
        Ok(None) => refused(code!("sign_in.failed")),
        Err(fault) => failure(&shared, fault),
    };
    sleep_until(answer_at).await;
    answer
}

async fn signed_in(shared: &Shared, body: &SignInFinish) -> Result<Option<SignedIn>, Fault> {
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
        code_hash: &hash(body.code.trim()),
        token_hash: &hash(&token),
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

/// A one-time code: six digits.
fn code_text(secret: &[u8; 32]) -> String {
    let word = secret.first_chunk().copied().unwrap_or_default();
    format!("{:06}", u64::from_le_bytes(word) % 1_000_000)
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

pub(crate) fn hash(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// The operating system's random bytes.
fn os_secrets() -> Result<[u8; 32], Fault> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        Fault::new(
            code!("random.failed"),
            "The server couldn't get random bytes",
        )
    })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::db::CODE_LIFETIME;
    use crate::test_support::{Fixture, request, run, workspace_id};

    fn status(head: &[String]) -> &str {
        head.first().map_or("", String::as_str)
    }

    #[test]
    fn codes_are_six_digits_and_tokens_are_hex() {
        assert_eq!(code_text(&[1; 32]), "076673");
        assert_eq!(code_text(&[0; 32]), "000000");
        assert_eq!(hex(&[0, 10, 255]), "000aff");
        assert_eq!(hash("a"), hash("a"));
        assert_ne!(hash("a"), hash("b"));
        assert_eq!(os_secrets().unwrap().len(), 32);
        assert_ne!(os_secrets().unwrap(), os_secrets().unwrap());
    }

    #[tokio::test]
    async fn a_member_signs_in_with_the_code_sent_to_them() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let mut changes = fixture.shared.changes.subscribe();
        let start = json!({ "workspace": "w1", "email": " Ada@Example.com " });
        let (started, _) = request(server.address, "POST /sign-in", &start.to_string()).await;
        let (to, code) = fixture.letter().await;
        let finish = json!({ "workspace": "w1", "email": "ada@example.com", "code": code });
        let (finished, body) =
            request(server.address, "POST /sign-in/finish", &finish.to_string()).await;
        let body: Value = serde_json::from_str(&body).unwrap();
        let token = body["token"].as_str().unwrap();
        let session_end = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap()
            .session_end(&hash(token), fixture.shared.now())
            .await
            .unwrap();

        assert_eq!(status(&started), "http/1.1 202 accepted");
        assert_eq!(to, "ada@example.com");
        assert_eq!(code.len(), 6);
        assert_eq!(status(&finished), "http/1.1 200 ok");
        assert_eq!(body["member"], "m1");
        assert_eq!(token.len(), 64);
        assert_eq!(session_end, Some(fixture.shared.now() + SESSION_LIFETIME));
        assert_eq!(changes.try_recv().unwrap().as_str(), "w1");
    }

    #[tokio::test]
    async fn strangers_get_the_same_answer_and_no_code() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        for body in [
            json!({ "workspace": "w1", "email": "bo@example.com" }),
            json!({ "workspace": "w9", "email": "ada@example.com" }),
            json!({ "workspace": "w1", "email": "not an email" }),
            json!({ "workspace": "w1", "email": "ada@example.com" }),
        ] {
            let (head, _) = request(server.address, "POST /sign-in", &body.to_string()).await;
            assert_eq!(status(&head), "http/1.1 202 accepted", "{body}");
        }
        let (bad_id, _) = request(
            server.address,
            "POST /sign-in",
            &json!({ "workspace": "../w1", "email": "ada@example.com" }).to_string(),
        )
        .await;

        // Only the member's request leaves a letter.
        assert_eq!(fixture.letter().await.0, "ada@example.com");
        assert_eq!(status(&bad_id), "http/1.1 422 unprocessable entity");
    }

    #[tokio::test]
    async fn a_wrong_or_late_code_is_refused() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let start = json!({ "workspace": "w1", "email": "ada@example.com" }).to_string();
        let finish = |workspace: &str, email: &str, code: &str| {
            json!({ "workspace": workspace, "email": email, "code": code }).to_string()
        };
        request(server.address, "POST /sign-in", &start).await;
        let (_, code) = fixture.letter().await;
        let mut refusals = Vec::new();
        for body in [
            finish("w1", "ada@example.com", "000000"),
            finish("w9", "ada@example.com", &code),
            finish("w1", "bo@example.com", &code),
            finish("w1", "not an email", &code),
        ] {
            refusals.push(request(server.address, "POST /sign-in/finish", &body).await);
        }
        fixture.advance(CODE_LIFETIME);
        let late = request(
            server.address,
            "POST /sign-in/finish",
            &finish("w1", "ada@example.com", &code),
        )
        .await;
        refusals.push(late);

        for (head, body) in refusals {
            assert_eq!(status(&head), "http/1.1 401 unauthorized");
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap(),
                json!({ "code": "sign_in.failed", "retryable": false })
            );
        }
    }

    #[tokio::test]
    async fn a_code_that_cant_be_sent_is_logged_and_answered_the_same() {
        let fixture = Fixture::new().await;
        let mut shared = fixture.shared.clone();
        shared.outbox = crate::mail::Outbox::new("missing-outbox".into());
        let server = run(shared).await;
        let start = json!({ "workspace": "w1", "email": "ada@example.com" }).to_string();
        let (head, _) = request(server.address, "POST /sign-in", &start).await;
        fixture.settled().await;
        let failed = fixture
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.msg == "request failed")
            .map(|record| record.fields["err"]["code"].clone());

        assert_eq!(status(&head), "http/1.1 202 accepted");
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
        let start = json!({ "workspace": "w1", "email": "ada@example.com" }).to_string();
        let (head, _) = request(server.address, "POST /sign-in", &start).await;
        drop(busy);
        fixture.settled().await;

        assert_eq!(status(&head), "http/1.1 202 accepted");
        assert!(fixture.letters().is_empty());
    }

    #[tokio::test]
    async fn finishing_takes_the_same_time_for_a_stranger() {
        let mut fixture = Fixture::new().await;
        fixture.shared.finish_within = Duration::from_millis(200);
        let server = run(fixture.shared.clone()).await;
        let stranger = json!({ "workspace": "w9", "email": "bo@example.com", "code": "000000" });
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
        let finish = json!({ "workspace": "w1", "email": "ada@example.com", "code": "000000" });
        let (head, body) =
            request(server.address, "POST /sign-in/finish", &finish.to_string()).await;

        assert_eq!(status(&head), "http/1.1 500 internal server error");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({ "code": "db.failed", "retryable": true })
        );
    }
}
