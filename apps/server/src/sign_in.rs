use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, sleep_until};
use weacon_observability::Fault;
use weacon_protocol::{
    AccountSignInFinish, AccountSignInStart, AccountSignedIn, ChallengeId, SignInStarted, code,
};

use crate::auth::{Session, Unproven, proof, same_origin, set_session_cookie};
use crate::client::Client;
use crate::db::Attempt;
use crate::email::Email;
use crate::http::{Shared, cross_site, failure, refused, report, too_many};
use crate::limit::Limit;
use crate::mail::mail_failed;
use crate::secret::{os_secrets, random_failed};
use crate::workspaces;

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

/// `POST /api/sign-in`: sends a code to an account's address, and answers with the challenge
/// to finish with. Answers the same, and as fast, whether or not the address has an account,
/// so it can't be used to find out.
pub(crate) async fn start(
    State(shared): State<Shared>,
    Client(from): Client,
    Json(body): Json<AccountSignInStart>,
) -> Response {
    if !shared.starts.allow(from, Instant::now()) {
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
    body: &AccountSignInStart,
    challenge: &ChallengeId,
) -> Result<(), Fault> {
    let Ok(email) = Email::parse(&body.email) else {
        return Ok(());
    };
    let Some(account) = shared.db.account_with(&email).await? else {
        return Ok(());
    };
    let code = code_text(&os_secrets()?);
    let code_hash = shared.secret.code_hash(challenge, &code);
    shared
        .db
        .start_challenge(&account, challenge, &code_hash, shared.now())
        .await?;
    let outbox = shared.outbox.clone();
    tokio::task::spawn_blocking(move || outbox.send(&email, &code))
        .await
        .map_err(|error| mail_failed().because(error))?
}

/// `POST /api/sign-in/finish`: trades a challenge's code for a session, in the same time
/// whether or not there was a code to try. A browser's page gets the session as a cookie its
/// scripts can't read; any other client gets the token.
pub(crate) async fn finish(
    State(shared): State<Shared>,
    Client(from): Client,
    headers: HeaderMap,
    Json(body): Json<AccountSignInFinish>,
) -> Response {
    let now = Instant::now();
    if !shared.tries.allow(from, now) {
        return too_many(code!("sign_in.too_many"));
    }
    let page = same_origin(&headers);
    if page == Some(false) {
        return cross_site();
    }
    let answer = match signed_in(&shared, &body).await {
        Ok(Some((who, token))) if page.is_some() => {
            let cookie = set_session_cookie(&token, SESSION_LIFETIME);
            ([(SET_COOKIE, cookie)], Json(who)).into_response()
        }
        Ok(Some((who, token))) => Json(AccountSignedIn {
            token: Some(token),
            ..who
        })
        .into_response(),
        Ok(None) => refused(code!("sign_in.failed")),
        Err(fault) => failure(&shared, fault),
    };
    sleep_until(now + shared.finish_within).await;
    answer
}

/// `GET /api/session`: who is signed in.
pub(crate) async fn session(session: Session) -> Json<AccountSignedIn> {
    Json(AccountSignedIn {
        account: session.account,
        email: session.email.as_str().to_owned(),
        token: None,
    })
}

/// `POST /api/sign-out`: ends the session the request carries, if it is still one, and
/// clears the browser's cookie either way, unless another site's page sent it.
pub(crate) async fn sign_out(
    State(shared): State<Shared>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let ended = match proof(&method, &headers) {
        Ok(token_hash) => shared.db.end_session(&token_hash).await,
        // Nothing to end, but another site's page may not clear the cookie either.
        Err(Unproven::Missing) if same_origin(&headers) != Some(false) => Ok(()),
        Err(_) => return Unproven::CrossSite.into(),
    };
    match ended {
        Ok(()) => {
            let cookie = set_session_cookie("", 0);
            (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response()
        }
        Err(fault) => failure(&shared, fault),
    }
}

/// The account `body` signs in to, and its new session's token.
async fn signed_in(
    shared: &Shared,
    body: &AccountSignInFinish,
) -> Result<Option<(AccountSignedIn, String)>, Fault> {
    let Ok(email) = Email::parse(&body.email) else {
        return Ok(None);
    };
    let token = hex(&os_secrets()?);
    let now = shared.now();
    let attempt = Attempt {
        email: &email,
        challenge: &body.challenge,
        code_hash: &shared.secret.code_hash(&body.challenge, body.code.trim()),
        token_hash: &token_hash(&token),
        now,
        token_expires_at: now.saturating_add(SESSION_LIFETIME),
        personal: &workspaces::personal()?,
    };
    let account = shared.db.finish_sign_in(&attempt).await?;
    Ok(account.map(|account| {
        let who = AccountSignedIn {
            account,
            email: email.as_str().to_owned(),
            token: None,
        };
        (who, token)
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
pub(crate) fn hex(bytes: &[u8]) -> String {
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
    use std::net::SocketAddr;

    use serde_json::{Value, json};
    use weacon_testing::TempDir;

    use super::*;
    use crate::auth::SESSION_COOKIE;
    use crate::db::CODE_LIFETIME;
    use crate::secret::Secret;
    use crate::test_support::{
        Fixture, ask, bearer, finish_body, fresh_account, member_id, request, request_with, run,
        sign_in, start_sign_in, status, workspace_id,
    };

    async fn finish_with(address: SocketAddr, body: &str) -> (String, Value) {
        let (head, body) = request(address, "POST /api/sign-in/finish", body).await;
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
    async fn a_person_signs_in_with_the_code_sent_to_them() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (started, challenge) = start_sign_in(server.address, " Ada@Example.com ").await;
        let (to, code) = fixture.letter().await;
        let finish = finish_body("ada@example.com", &challenge, &code);
        let (finished, body) = finish_with(server.address, &finish).await;
        let token = body["token"].as_str().unwrap();
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ends = fixture.shared.now() + SESSION_LIFETIME;
        let live = w1.session_member(&token_hash(token), ends - 1).await;
        let ended = w1.session_member(&token_hash(token), ends).await;

        assert_eq!(started, "http/1.1 202 accepted");
        assert_eq!(to, "ada@example.com");
        assert_eq!(code.len(), 8);
        assert_eq!(finished, "http/1.1 200 ok");
        assert_eq!(body["email"], "ada@example.com");
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert_eq!(token.len(), 64);
        assert_eq!(body["account"].as_str().unwrap().len(), 32);
        assert_eq!(live.unwrap(), Some(member_id("m1")));
        assert_eq!(ended.unwrap(), None);
    }

    const PAGE: &str = "origin: https://test\r\n";

    // Signs ada in from the server's own page: the answer's head and body.
    async fn browser_sign_in(fixture: &Fixture, address: SocketAddr) -> (Vec<String>, Value) {
        let (_, challenge) = start_sign_in(address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let finish = finish_body("ada@example.com", &challenge, &code);
        let (head, body) = request_with(address, "POST /api/sign-in/finish", PAGE, &finish).await;
        (head, serde_json::from_str(&body).unwrap())
    }

    // The session cookie a response sets, as a request's `cookie` header. The test client
    // lowercases what it reads, so the cookie's name is written again.
    fn cookie(head: &[String]) -> String {
        let set = head
            .iter()
            .find_map(|line| line.strip_prefix("set-cookie: __host-weacon_session="))
            .unwrap();
        let token = set.split(';').next().unwrap();
        format!("cookie: theme=dark; {SESSION_COOKIE}={token}\r\n")
    }

    #[tokio::test]
    async fn a_browser_gets_its_session_as_a_cookie_its_scripts_cant_read() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (head, body) = browser_sign_in(&fixture, server.address).await;
        let set = head
            .iter()
            .find_map(|line| line.strip_prefix("set-cookie: "))
            .unwrap();
        let (name, rest) = set.split_once('=').unwrap();
        let (token, attributes) = rest.split_once(';').unwrap();
        let (found, session) = ask(server.address, "GET /api/session", &cookie(&head), "").await;

        assert_eq!(status(&head), "http/1.1 200 ok");
        assert_eq!(name, "__host-weacon_session");
        assert_eq!(token.len(), 64);
        assert_eq!(
            attributes,
            " path=/; max-age=2592000; httponly; secure; samesite=strict"
        );
        assert_eq!(body.as_object().unwrap().len(), 2);
        assert_eq!(found, "http/1.1 200 ok");
        assert_eq!(session, body);
        assert_eq!(session["email"], "ada@example.com");
    }

    #[tokio::test]
    async fn another_sites_page_cant_sign_in_or_use_the_cookie() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let finish = finish_body("ada@example.com", &challenge, &code);
        let other = "origin: https://other.example\r\n";
        let (crossed, _) =
            request_with(server.address, "POST /api/sign-in/finish", other, &finish).await;
        // The code wasn't tried, so it still works from the server's own page.
        let (head, _) =
            request_with(server.address, "POST /api/sign-in/finish", PAGE, &finish).await;
        let cookie = cookie(&head);
        let forbidden = json!({ "code": "request.cross_site", "retryable": false });
        let mut refusals = Vec::new();
        for (request, headers) in [
            ("GET /api/session", format!("{cookie}{other}")),
            (
                "GET /api/session",
                format!("{cookie}origin: http://test\r\n"),
            ),
            ("POST /api/sign-out", format!("{cookie}{other}")),
            // Without the cookie, as a form on another site sends it, it still can't clear it.
            ("POST /api/sign-out", other.to_owned()),
            // No page sent this, so it may read but not change.
            ("POST /api/sign-out", cookie.clone()),
        ] {
            refusals.push(ask(server.address, request, &headers, "").await);
        }
        let (read, _) = ask(server.address, "GET /api/session", &cookie, "").await;

        assert_eq!(status(&crossed), "http/1.1 403 forbidden");
        assert_eq!(status(&head), "http/1.1 200 ok");
        for refusal in refusals {
            assert_eq!(
                refusal,
                ("http/1.1 403 forbidden".to_owned(), forbidden.clone())
            );
        }
        assert_eq!(read, "http/1.1 200 ok");
    }

    #[tokio::test]
    async fn signing_out_ends_the_session_here_and_in_the_browser() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (head, _) = browser_sign_in(&fixture, server.address).await;
        let cookie = cookie(&head);
        let (_, other) = sign_in(&fixture, server.address, "ada@example.com").await;
        let bearer = bearer(&other);

        let page = format!("{cookie}{PAGE}");
        let (out, _) = request_with(server.address, "POST /api/sign-out", &page, "").await;
        let cleared = out
            .iter()
            .find_map(|line| line.strip_prefix("set-cookie: "));
        let (after, _) = ask(server.address, "GET /api/session", &cookie, "").await;
        let (again, _) = ask(server.address, "POST /api/sign-out", &page, "").await;
        let (unproven, _) = ask(server.address, "POST /api/sign-out", PAGE, "").await;
        let (bare, _) = ask(server.address, "POST /api/sign-out", "", "").await;
        let (others, _) = ask(server.address, "GET /api/session", &bearer, "").await;
        let (token_out, _) = ask(server.address, "POST /api/sign-out", &bearer, "").await;
        let (token_after, _) = ask(server.address, "GET /api/session", &bearer, "").await;

        assert_eq!(status(&out), "http/1.1 204 no content");
        assert_eq!(
            cleared,
            Some("__host-weacon_session=; path=/; max-age=0; httponly; secure; samesite=strict")
        );
        assert_eq!(after, "http/1.1 401 unauthorized");
        // Signing out again, or with nothing to sign out of, is as good.
        assert_eq!(again, "http/1.1 204 no content");
        assert_eq!(unproven, "http/1.1 204 no content");
        assert_eq!(bare, "http/1.1 204 no content");
        // Each session ends on its own, and a bearer token needs no page behind it.
        assert_eq!(others, "http/1.1 200 ok");
        assert_eq!(token_out, "http/1.1 204 no content");
        assert_eq!(token_after, "http/1.1 401 unauthorized");
    }

    #[tokio::test]
    async fn nobody_is_signed_in_without_a_live_token() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, body) = sign_in(&fixture, server.address, "ada@example.com").await;
        let bearer = bearer(&body);
        let required = json!({ "code": "sign_in.required", "retryable": false });

        let (found, session) = ask(server.address, "GET /api/session", &bearer, "").await;
        let none = ask(server.address, "GET /api/session", "", "").await;
        let wrong = ask(server.address, "GET /api/session", "cookie: a=b\r\n", "").await;
        fixture.advance(SESSION_LIFETIME);
        let ended = ask(server.address, "GET /api/session", &bearer, "").await;

        assert_eq!(found, "http/1.1 200 ok");
        assert_eq!(session["account"], body["account"]);
        assert_eq!(session.as_object().unwrap().len(), 2);
        for refusal in [none, wrong, ended] {
            assert_eq!(
                refusal,
                ("http/1.1 401 unauthorized".to_owned(), required.clone())
            );
        }
    }

    #[tokio::test]
    async fn an_account_in_no_workspace_signs_in_too() {
        let fixture = Fixture::new().await;
        let bo = Email::parse("bo@example.com").unwrap();
        fixture
            .shared
            .db
            .add_account(&fresh_account(), &bo)
            .await
            .unwrap();
        let server = run(fixture.shared.clone()).await;

        let (finished, _) = sign_in(&fixture, server.address, "bo@example.com").await;

        assert_eq!(finished, "http/1.1 200 ok");
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
        let finish = finish_body("ada@example.com", &challenge, &code);

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
            json!({ "email": "bo@example.com" }),
            json!({ "email": "not an email" }),
            json!({ "email": "ada@example.com" }),
        ] {
            let (head, answer) =
                request(server.address, "POST /api/sign-in", &body.to_string()).await;
            let answer: Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(status(&head), "http/1.1 202 accepted", "{body}");
            assert_eq!(answer.as_object().unwrap().len(), 1, "{body}");
            challenges.push(answer["challenge"].as_str().unwrap().len());
        }
        let (unreadable, _) = request(server.address, "POST /api/sign-in", "{}").await;

        // Only the request for an account's address leaves a letter.
        assert_eq!(fixture.letter().await.0, "ada@example.com");
        assert_eq!(challenges, [64; 3]);
        assert_eq!(status(&unreadable), "http/1.1 422 unprocessable entity");
    }

    #[tokio::test]
    async fn a_wrong_or_late_code_is_refused() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter().await;
        let (_, other) = start_sign_in(server.address, "ada@example.com").await;
        fixture.settled().await;
        let finish = |email: &str, code: &str| finish_body(email, &challenge, code);
        let mut refusals = Vec::new();
        for body in [
            finish("ada@example.com", "00000000"),
            finish("bo@example.com", &code),
            finish("not an email", &code),
            finish_body("ada@example.com", &other, &code),
        ] {
            refusals.push(finish_with(server.address, &body).await);
        }
        fixture.advance(CODE_LIFETIME);
        refusals.push(finish_with(server.address, &finish("ada@example.com", &code)).await);
        let unchallenged = finish_body("ada@example.com", &Value::Null, &code);
        let (unreadable, _) = finish_with(server.address, &unchallenged).await;

        assert_eq!(unreadable, "http/1.1 422 unprocessable entity");
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
            let wrong = finish_body("ada@example.com", challenge, &format!("{guess:08}"));
            finish_with(server.address, &wrong).await;
        }
        let (finished, _) = finish_with(
            server.address,
            &finish_body("ada@example.com", &mine, &code),
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
            "POST /api/sign-in",
            &json!({ "email": "ada@example.com" }).to_string(),
        )
        .await;
        fixture.settled().await;
        let finish = finish_body("ada@example.com", &challenge, "00000000");
        let wrong = finish_with(server.address, &finish).await;
        let right = finish_body("ada@example.com", &challenge, &code);
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
    async fn behind_a_trusted_proxy_each_forwarded_client_has_its_own_limit() {
        let fixture = Fixture::new().await;
        let start = json!({ "email": "ada@example.com" }).to_string();
        let one_each = || Arc::new(Limit::new(1, HOUR));
        let direct = Shared {
            starts: one_each(),
            ..fixture.shared.clone()
        };
        let proxied = Shared {
            starts: one_each(),
            proxies: Arc::new(["127.0.0.1".parse().unwrap()]),
            ..fixture.shared.clone()
        };
        let (direct, proxied) = (run(direct).await, run(proxied).await);
        let mut statuses = Vec::new();
        for (server, forwarded) in [
            (&direct, "198.51.100.1"),
            (&direct, "198.51.100.2"),
            (&proxied, "198.51.100.1"),
            (&proxied, "198.51.100.2"),
            (&proxied, "198.51.100.1"),
        ] {
            let header = format!("x-forwarded-for: {forwarded}\r\n");
            let (head, _) =
                request_with(server.address, "POST /api/sign-in", &header, &start).await;
            statuses.push(status(&head).to_owned());
        }
        let (accepted, limited) = ("http/1.1 202 accepted", "http/1.1 429 too many requests");

        // Unless its sender is trusted, the header changes nothing.
        assert_eq!(statuses, [accepted, limited, accepted, accepted, limited]);
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
        let stranger = finish_body("bo@example.com", &json!("0".repeat(64)), "00000000");
        let started = std::time::Instant::now();
        let (head, _) = request(server.address, "POST /api/sign-in/finish", &stranger).await;

        assert_eq!(status(&head), "http/1.1 401 unauthorized");
        assert!(started.elapsed() >= fixture.shared.finish_within);
    }

    #[tokio::test]
    async fn a_failing_database_is_answered_as_worth_trying_again() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        fixture.shared.db.close().await;
        let finish = finish_body("ada@example.com", &json!("0".repeat(64)), "00000000");
        let (head, body) = request(server.address, "POST /api/sign-in/finish", &finish).await;

        assert_eq!(status(&head), "http/1.1 500 internal server error");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({ "code": "db.failed", "retryable": true })
        );
    }
}
