use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use weacon_observability::Fault;
use weacon_protocol::{
    AccountSignInFinish, AccountSignInStart, AccountSignedIn, ChallengeId, SignInStarted, code,
};

use crate::auth::{Sender, Session, Unproven, proof, sender, set_session_cookie};
use crate::codes::{new_code, normalised, shown};
use crate::db::{Attempt, NewChallenge};
use crate::email::Email;
use crate::http::{Body, cross_site, failure, invalid, not_found, unauthorized};
use crate::random::{random_failed, random_token};
use crate::shared::Shared;
use crate::workspaces;

/// The subject of the mail that carries a code.
pub(crate) const SUBJECT: &str = "Your Weacon sign-in code";
/// How long a mailed code works, in seconds.
pub(crate) const CODE_LIFETIME: i64 = 10 * 60;
/// How long a session lasts, in seconds.
pub(crate) const SESSION_LIFETIME: i64 = 30 * 24 * 60 * 60;

pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/sign-in", post(start))
        .route("/api/sign-in/finish", post(finish))
        .route("/api/session", get(session))
        .route("/api/sign-out", post(sign_out))
}

/// `POST /api/sign-in`: sends a code to an account's address, and answers with the challenge
/// to finish with.
pub(crate) async fn start(
    State(shared): State<Shared>,
    Body(body): Body<AccountSignInStart>,
) -> Response {
    let Ok(email) = Email::parse(body.email.expose()) else {
        return invalid(code!("email.invalid"));
    };
    match send_code(&shared, &email).await {
        Ok(Some(challenge)) => {
            (StatusCode::ACCEPTED, Json(SignInStarted { challenge })).into_response()
        }
        Ok(None) => not_found(code!("sign_in.no_account")),
        Err(fault) => failure(fault),
    }
}

/// The challenge whose code was sent to `email`, or none when no account has that address.
async fn send_code(shared: &Shared, email: &Email) -> Result<Option<ChallengeId>, Fault> {
    if shared.db.account_with(email).await?.is_none() {
        return Ok(None);
    }
    let (challenge, code) = (new_challenge()?, new_code()?);
    let now = shared.now();
    let new = NewChallenge {
        email,
        id: &challenge,
        code: &code,
        now,
        expires_at: now.saturating_add(CODE_LIFETIME),
    };
    shared.db.start_challenge(&new).await?;
    let text = format!(
        "Your code is {}. It works for {} minutes; if you didn't ask for it, ignore this mail.",
        shown(&code),
        CODE_LIFETIME / 60
    );
    shared.log.mail(email.as_str(), SUBJECT, &text);
    Ok(Some(challenge))
}

/// `POST /api/sign-in/finish`: trades a challenge's code for a session. A browser's page gets
/// the session as a cookie its scripts can't read; any other client gets the token.
pub(crate) async fn finish(
    State(shared): State<Shared>,
    headers: HeaderMap,
    Body(body): Body<AccountSignInFinish>,
) -> Response {
    let page = sender(&headers);
    if page == Sender::OtherSite {
        return cross_site();
    }
    match signed_in(&shared, &body).await {
        Ok(Some((who, token))) if page == Sender::OwnPage => {
            let cookie = set_session_cookie(&token, SESSION_LIFETIME);
            ([(SET_COOKIE, cookie)], Json(who)).into_response()
        }
        Ok(Some((who, token))) => Json(AccountSignedIn {
            token: Some(token.into()),
            ..who
        })
        .into_response(),
        Ok(None) => unauthorized(code!("sign_in.failed")),
        Err(fault) => failure(fault),
    }
}

/// `GET /api/session`: who is signed in.
pub(crate) async fn session(session: Session) -> Json<AccountSignedIn> {
    Json(AccountSignedIn {
        account: session.account,
        email: session.email.as_str().into(),
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
        Ok(token) => shared.db.end_session(token).await,
        // Nothing to end, but another site's page may not clear the cookie either.
        Err(Unproven::Missing) if sender(&headers) != Sender::OtherSite => Ok(()),
        Err(_) => return cross_site(),
    };
    match ended {
        Ok(()) => {
            let cookie = set_session_cookie("", 0);
            (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response()
        }
        Err(fault) => failure(fault),
    }
}

/// The account `body` signs in to, and its new session's token.
async fn signed_in(
    shared: &Shared,
    body: &AccountSignInFinish,
) -> Result<Option<(AccountSignedIn, String)>, Fault> {
    let Ok(email) = Email::parse(body.email.expose()) else {
        return Ok(None);
    };
    let token = random_token()?;
    let now = shared.now();
    let attempt = Attempt {
        email: &email,
        challenge: &body.challenge,
        code: &normalised(body.code.expose()),
        token: &token,
        now,
        token_expires_at: now.saturating_add(SESSION_LIFETIME),
        personal: &workspaces::personal()?,
    };
    let account = shared.db.finish_sign_in(&attempt).await?;
    Ok(account.map(|account| {
        let who = AccountSignedIn {
            account,
            email: email.as_str().into(),
            token: None,
        };
        (who, token)
    }))
}

/// A challenge nobody can guess.
fn new_challenge() -> Result<ChallengeId, Fault> {
    ChallengeId::try_from(random_token()?).map_err(|_| random_failed())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::{Value, json};

    use super::*;
    use crate::auth::SESSION_COOKIE;
    use crate::test_support::{
        Fixture, ask, bearer, email, finish_body, fresh_account, member_id, request, request_with,
        sign_in, start_sign_in, status, workspace_id,
    };

    async fn finish_with(address: SocketAddr, body: &str) -> (String, Value) {
        ask(address, "POST /api/sign-in/finish", "", body).await
    }

    fn unknown_challenge() -> ChallengeId {
        "0".repeat(64).parse().unwrap()
    }

    #[test]
    fn challenges_are_long_tokens() {
        assert_eq!(new_challenge().unwrap().as_str().len(), 64);
    }

    #[tokio::test]
    async fn a_person_signs_in_with_the_code_sent_to_them() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (started, challenge) = start_sign_in(server.address, " Ada@Example.com ").await;
        let (to, code) = fixture.letter();
        let finish = finish_body("ada@example.com", &challenge, &code);
        let (finished, body) = finish_with(server.address, &finish).await;
        let token = body["token"].as_str().unwrap();
        let text = format!(
            "Your code is {code}. It works for 10 minutes; if you didn't ask for it, ignore this mail."
        );
        let mailed = fixture.records.lock().unwrap().iter().any(|record| {
            record.msg == "mail printed"
                && record.fields["subject"] == "Your Weacon sign-in code"
                && record.fields["text"] == text
        });
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let ends = fixture.shared.now() + SESSION_LIFETIME;
        let live = w1.session_member(token, ends - 1).await;
        let ended = w1.session_member(token, ends).await;

        assert_eq!(started, "http/1.1 202 accepted");
        assert_eq!(to, "ada@example.com");
        assert!(mailed);
        assert_eq!(code.len(), 9);
        assert_eq!(code.get(4..5), Some("-"));
        assert_eq!(normalised(&code).len(), 8);
        assert_eq!(finished, "http/1.1 200 ok");
        assert_eq!(body["email"], "ada@example.com");
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert_eq!(token.len(), 64);
        assert_eq!(body["account"].as_str().unwrap().len(), 32);
        assert_eq!(live.unwrap(), Some(member_id("m1")));
        assert_eq!(ended.unwrap(), None);
    }

    #[tokio::test]
    async fn a_code_signs_in_however_its_letters_are_typed() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter();
        let typed = format!(" {} ", code.replace('-', "").to_lowercase());

        let finish = finish_body("ada@example.com", &challenge, &typed);
        let (finished, body) = finish_with(server.address, &finish).await;
        assert_eq!(finished, "http/1.1 200 ok");
        assert_eq!(body["email"], "ada@example.com");
    }

    const PAGE: &str = "origin: https://test\r\n";

    // Signs ada in from the server's own page: the answer's head and body.
    async fn browser_sign_in(fixture: &Fixture, address: SocketAddr) -> (Vec<String>, Value) {
        let (_, challenge) = start_sign_in(address, "ada@example.com").await;
        let (_, code) = fixture.letter();
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
        let server = fixture.run().await;
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
        let server = fixture.run().await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter();
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
        let server = fixture.run().await;
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
        let server = fixture.run().await;
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
        let bo = email("bo@example.com");
        fixture
            .shared
            .db
            .add_account(&fresh_account(), &bo)
            .await
            .unwrap();
        let server = fixture.run().await;

        let (finished, _) = sign_in(&fixture, server.address, "bo@example.com").await;

        assert_eq!(finished, "http/1.1 200 ok");
    }

    #[tokio::test]
    async fn a_stranger_or_a_bad_address_is_told_so_and_gets_no_code() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let start = |email: &str| json!({ "email": email }).to_string();
        let (stranger, bad) = (start("bo@example.com"), start("not an email"));

        let stranger = ask(server.address, "POST /api/sign-in", "", &stranger).await;
        let bad = ask(server.address, "POST /api/sign-in", "", &bad).await;
        let (unreadable, refusal) = request(server.address, "POST /api/sign-in", "{}").await;

        assert_eq!(
            stranger,
            (
                "http/1.1 404 not found".to_owned(),
                json!({ "code": "sign_in.no_account", "retryable": false })
            )
        );
        assert_eq!(
            bad,
            (
                "http/1.1 422 unprocessable entity".to_owned(),
                json!({ "code": "email.invalid", "retryable": false })
            )
        );
        assert!(fixture.letters().is_empty());
        assert_eq!(status(&unreadable), "http/1.1 400 bad request");
        assert_eq!(
            serde_json::from_str::<Value>(&refusal).unwrap(),
            json!({ "code": "request.invalid", "retryable": false })
        );
    }

    #[tokio::test]
    async fn a_wrong_or_late_code_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, challenge) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter();
        let finish = |email: &str, code: &str| finish_body(email, &challenge, code);
        let mut refusals = Vec::new();
        for body in [
            finish("ada@example.com", "00000000"),
            finish("bo@example.com", &code),
            finish("not an email", &code),
        ] {
            refusals.push(finish_with(server.address, &body).await);
        }
        fixture.advance(CODE_LIFETIME);
        refusals.push(finish_with(server.address, &finish("ada@example.com", &code)).await);
        let unchallenged = json!({ "email": "ada@example.com", "code": code });
        let (unreadable, _) = finish_with(server.address, &unchallenged.to_string()).await;

        assert_eq!(unreadable, "http/1.1 400 bad request");
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
        let server = fixture.run().await;
        let (_, theirs) = start_sign_in(server.address, "ada@example.com").await;
        fixture.letter();
        let (_, mine) = start_sign_in(server.address, "ada@example.com").await;
        let (_, code) = fixture.letter();
        let unknown = unknown_challenge();
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
    async fn a_failing_database_is_answered_as_worth_trying_again() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        fixture.shared.db.close().await;
        let finish = finish_body("ada@example.com", &unknown_challenge(), "00000000");
        let (head, body) = request(server.address, "POST /api/sign-in/finish", &finish).await;

        let records = fixture.records.lock().unwrap().clone();
        let ids = |event: &str| -> Vec<_> {
            records
                .iter()
                .filter(|record| record.msg == event)
                .map(|record| record.fields["request_id"].clone())
                .collect()
        };

        assert_eq!(status(&head), "http/1.1 500 internal server error");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({ "code": "db.failed", "retryable": true })
        );
        // Logged once, with the id that joins it to the request's own record.
        assert_eq!(ids("request failed"), ids("request handled"));
        assert_eq!(ids("request failed").len(), 1);
    }
}
