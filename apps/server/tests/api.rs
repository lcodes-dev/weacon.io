use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use weacon_server::{AppState, db, router};

/// Keeps the session cookie, as a browser does.
struct Client {
    state: AppState,
    cookie: Option<String>,
}

async fn server() -> AppState {
    AppState {
        pool: db::memory().await,
        mailer: Arc::default(),
        url: "http://weacon.test".into(),
    }
}

impl Client {
    fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
            cookie: None,
        }
    }

    async fn send(&mut self, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(CONTENT_TYPE, "application/json");
        if let Some(cookie) = &self.cookie {
            request = request.header(COOKIE, cookie);
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        let response = router(self.state.clone()).oneshot(request).await.unwrap();
        if let Some(cookie) = response.headers().get(SET_COOKIE) {
            self.cookie = cookie
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .map(str::to_owned);
        }
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    async fn get(&mut self, path: &str) -> (StatusCode, Value) {
        self.send(Method::GET, path, Value::Null).await
    }

    async fn post(&mut self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, path, body).await
    }

    async fn put(&mut self, path: &str, body: Value) -> StatusCode {
        self.send(Method::PUT, path, body).await.0
    }

    async fn delete(&mut self, path: &str) -> StatusCode {
        self.send(Method::DELETE, path, Value::Null).await.0
    }

    async fn code(&self, email: &str) -> Option<String> {
        sqlx::query_scalar("SELECT code FROM sign_in_codes WHERE email = ?")
            .bind(email.trim().to_lowercase())
            .fetch_optional(&self.state.pool)
            .await
            .unwrap()
    }

    async fn token(&self, email: &str) -> Option<String> {
        sqlx::query_scalar("SELECT token FROM invites WHERE email = ?")
            .bind(email.trim().to_lowercase())
            .fetch_optional(&self.state.pool)
            .await
            .unwrap()
    }

    /// Invites the address to this client's first team and returns the link's token.
    async fn invite(&mut self, email: &str) -> String {
        let (status, _) = self
            .post("/api/teams/1/invites", json!({"emails": [email]}))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        self.token(email).await.unwrap()
    }

    /// With a name, registers.
    async fn sign_in(&mut self, email: &str, name: Option<&str>) -> (StatusCode, Value) {
        let (status, _) = self
            .post("/api/sign-in/code", json!({"email": email, "name": name}))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let code = self.code(email).await.unwrap();
        self.post("/api/sign-in/verify", json!({"email": email, "code": code}))
            .await
    }
}

#[tokio::test]
async fn health_answers_ok() {
    let (status, body) = Client::new(&server().await).get("/api/health").await;
    assert_eq!((status, body), (StatusCode::OK, json!({"status": "ok"})));
}

#[tokio::test]
async fn unknown_api_path_is_a_json_404() {
    let (status, body) = Client::new(&server().await).get("/api/nope").await;
    assert_eq!(
        (status, body),
        (StatusCode::NOT_FOUND, json!({"error": "not found"}))
    );
}

// Needs the web app built: `npm run build` in apps/web.
#[tokio::test]
async fn unknown_path_serves_the_web_app() {
    let request = Request::get("/chats/42").body(Body::empty()).unwrap();
    let response = router(server().await).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains(r#"<div id="root">"#));
}

#[tokio::test]
async fn a_fresh_server_needs_setup() {
    let (_, session) = Client::new(&server().await).get("/api/session").await;
    assert_eq!(session, json!({"setup_needed": true, "user": null}));
}

#[tokio::test]
async fn the_first_to_register_owns_the_server_and_later_ones_do_not() {
    let state = server().await;
    let mut ada = Client::new(&state);
    let (status, user) = ada.sign_in("ada@example.com", Some("Ada")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["is_owner"], true);
    assert_eq!(user["teams"][0]["name"], "Ada's team");
    assert_eq!(user["teams"][0]["role"], "owner");

    let (_, session) = ada.get("/api/session").await;
    assert_eq!(session["setup_needed"], false);
    assert_eq!(session["user"]["email"], "ada@example.com");

    let mut marco = Client::new(&state);
    let (_, user) = marco.sign_in(" Marco@Example.com ", Some("Marco")).await;
    assert_eq!(user["email"], "marco@example.com");
    assert_eq!(user["is_owner"], false);
    assert_eq!(user["teams"][0]["name"], "Marco's team");
}

#[tokio::test]
async fn an_account_signs_out_and_in_again() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;

    assert_eq!(
        ada.post("/api/sign-out", Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get("/api/session").await.1["user"], Value::Null);

    let (status, user) = ada.sign_in("ada@example.com", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["name"], "Ada");
}

#[tokio::test]
async fn an_unknown_email_gets_no_code_to_sign_in() {
    let mut client = Client::new(&server().await);
    let (status, _) = client
        .post("/api/sign-in/code", json!({"email": "nobody@example.com"}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client.code("nobody@example.com").await, None);
}

#[tokio::test]
async fn the_fifth_wrong_code_kills_the_right_one() {
    let mut client = Client::new(&server().await);
    let request = json!({"email": "ada@example.com", "name": "Ada"});
    client.post("/api/sign-in/code", request).await;
    let code = client.code("ada@example.com").await.unwrap();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for code in [wrong, wrong, wrong, wrong, wrong, &code] {
        let attempt = json!({"email": "ada@example.com", "code": code});
        let (status, _) = client.post("/api/sign-in/verify", attempt).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let mut client = Client::new(&server().await);
    let request = json!({"email": "ada@example.com", "name": "Ada"});
    client.post("/api/sign-in/code", request).await;
    let code = client.code("ada@example.com").await.unwrap();
    sqlx::query("UPDATE sign_in_codes SET expires_at = 0")
        .execute(&client.state.pool)
        .await
        .unwrap();
    let attempt = json!({"email": "ada@example.com", "code": code});
    let (status, _) = client.post("/api/sign-in/verify", attempt).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_the_owner_reaches_the_mail_settings() {
    let state = server().await;
    let mut owner = Client::new(&state);
    owner.sign_in("ada@example.com", Some("Ada")).await;
    let mut member = Client::new(&state);
    member.sign_in("marco@example.com", Some("Marco")).await;

    assert_eq!(
        Client::new(&state).get("/api/config/mail").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        member.get("/api/config/mail").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        member.delete("/api/config/mail").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        owner.get("/api/config/mail").await,
        (StatusCode::OK, json!({"smtp": null}))
    );
}

#[tokio::test]
async fn the_owner_saves_and_clears_smtp() {
    let state = server().await;
    let mut owner = Client::new(&state);
    owner.sign_in("ada@example.com", Some("Ada")).await;
    let smtp = json!({
        "host": "127.0.0.1", "port": 1, "security": "none",
        "username": "ada", "password": "secret", "from": "Weacon <weacon@example.com>",
    });

    assert_eq!(
        owner.put("/api/config/mail", smtp.clone()).await,
        StatusCode::NO_CONTENT
    );
    let (_, saved) = owner.get("/api/config/mail").await;
    assert_eq!(saved["smtp"]["host"], "127.0.0.1");
    assert_eq!(saved["smtp"].get("password"), None);
    // Nothing listens on that port, so a mail that goes over SMTP fails.
    assert_eq!(
        owner.post("/api/config/mail/test", Value::Null).await.0,
        StatusCode::BAD_GATEWAY
    );

    // Saving again without a password keeps the saved one.
    let mut without = smtp.clone();
    without.as_object_mut().unwrap().remove("password");
    owner.put("/api/config/mail", without).await;
    assert_eq!(
        db::smtp(&state.pool)
            .await
            .unwrap()
            .unwrap()
            .password
            .as_deref(),
        Some("secret")
    );

    let mut bad = smtp;
    bad["security"] = json!("carrier pigeon");
    assert_eq!(
        owner.put("/api/config/mail", bad).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    assert_eq!(
        owner.delete("/api/config/mail").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(owner.get("/api/config/mail").await.1, json!({"smtp": null}));
    assert_eq!(
        owner.post("/api/config/mail/test", Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
}

/// Ada, who owns team 1, and Marco, who has joined it.
async fn ada_and_marco(state: &AppState) -> (Client, Client) {
    let mut ada = Client::new(state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite("marco@example.com").await;
    let mut marco = Client::new(state);
    marco.sign_in("marco@example.com", Some("Marco")).await;
    let (status, joined) = marco
        .post(&format!("/api/invites/{token}/accept"), Value::Null)
        .await;
    assert_eq!((status, joined), (StatusCode::OK, json!({"team_id": 1})));
    (ada, marco)
}

#[tokio::test]
async fn an_invited_person_registers_and_joins() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite(" Marco@Example.com ").await;

    let (_, team) = ada.get("/api/teams/1").await;
    assert_eq!(team["invites"][0]["email"], "marco@example.com");

    let path = format!("/api/invites/{token}");
    let (status, invitation) = Client::new(&state).get(&path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        invitation,
        json!({
            "team_name": "Ada's team", "invited_by": "Ada",
            "email": "marco@example.com", "has_account": false,
        })
    );
    let accept = format!("{path}/accept");
    assert_eq!(
        Client::new(&state).post(&accept, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );

    let mut marco = Client::new(&state);
    marco.sign_in("marco@example.com", Some("Marco")).await;
    assert_eq!(marco.post(&accept, Value::Null).await.0, StatusCode::OK);

    let (_, session) = marco.get("/api/session").await;
    assert_eq!(
        session["user"]["teams"],
        json!([
            {"id": 1, "name": "Ada's team", "role": "member"},
            {"id": 2, "name": "Marco's team", "role": "owner"},
        ])
    );
    let (_, team) = marco.get("/api/teams/1").await;
    assert_eq!(team["members"][0]["role"], "owner");
    assert_eq!(team["members"][1]["email"], "marco@example.com");
    assert_eq!(team["invites"], json!([]));

    // The link worked once.
    assert_eq!(marco.get(&path).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        marco.post(&accept, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_invite_only_works_for_its_address() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite("marco@example.com").await;

    let mut lena = Client::new(&state);
    lena.sign_in("lena@example.com", Some("Lena")).await;
    let accept = format!("/api/invites/{token}/accept");
    assert_eq!(
        lena.post(&accept, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(lena.get("/api/teams/1").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn inviting_again_replaces_the_link_and_revoking_kills_it() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let first = ada.invite("marco@example.com").await;
    let second = ada.invite("marco@example.com").await;
    assert_ne!(first, second);
    assert_eq!(
        ada.get(&format!("/api/invites/{first}")).await.0,
        StatusCode::NOT_FOUND
    );

    let (_, team) = ada.get("/api/teams/1").await;
    assert_eq!(team["invites"].as_array().unwrap().len(), 1);
    let invite = format!("/api/teams/1/invites/{}", team["invites"][0]["id"]);
    assert_eq!(ada.delete(&invite).await, StatusCode::NO_CONTENT);
    assert_eq!(
        ada.get(&format!("/api/invites/{second}")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn only_the_owner_renames_invites_and_removes() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;
    let name = json!({"name": " Northwind "});

    assert_eq!(
        marco.put("/api/teams/1", name.clone()).await,
        StatusCode::FORBIDDEN
    );
    let invite = json!({"emails": ["lena@example.com"]});
    assert_eq!(
        marco.post("/api/teams/1/invites", invite).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        marco.delete("/api/teams/1/members/1").await,
        StatusCode::FORBIDDEN
    );

    assert_eq!(ada.put("/api/teams/1", name).await, StatusCode::NO_CONTENT);
    assert_eq!(ada.get("/api/teams/1").await.1["name"], "Northwind");
    assert_eq!(
        ada.put("/api/teams/1", json!({"name": " "})).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Marco is already in.
    let invite = json!({"emails": ["marco@example.com"]});
    assert_eq!(
        ada.post("/api/teams/1/invites", invite).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Marco's own team isn't Ada's to see.
    assert_eq!(ada.get("/api/teams/2").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        ada.put("/api/teams/2", json!({"name": "Mine"})).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_member_is_removed_or_leaves_and_the_owner_stays() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;

    assert_eq!(
        ada.delete("/api/teams/1/members/1").await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        ada.delete("/api/teams/1/members/2").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(marco.get("/api/teams/1").await.0, StatusCode::NOT_FOUND);

    let token = ada.invite("marco@example.com").await;
    marco
        .post(&format!("/api/invites/{token}/accept"), Value::Null)
        .await;
    assert_eq!(
        marco.delete("/api/teams/1/members/2").await,
        StatusCode::NO_CONTENT
    );
    let (_, team) = ada.get("/api/teams/1").await;
    assert_eq!(team["members"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn several_addresses_are_invited_at_once_or_none_is() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;

    let invites = json!({"emails": ["marco@example.com", "not an address"]});
    assert_eq!(
        ada.post("/api/teams/1/invites", invites).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(ada.token("marco@example.com").await, None);

    let invites = json!({"emails": ["marco@example.com", "lena@example.com"]});
    assert_eq!(
        ada.post("/api/teams/1/invites", invites).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, team) = ada.get("/api/teams/1").await;
    assert_eq!(team["invites"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_expired_invite_is_refused_until_it_is_sent_again() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite("marco@example.com").await;
    sqlx::query("UPDATE invites SET expires_at = 0")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut marco = Client::new(&state);
    marco.sign_in("marco@example.com", Some("Marco")).await;
    let accept = format!("/api/invites/{token}/accept");
    assert_eq!(
        marco.post(&accept, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );

    let token = ada.invite("marco@example.com").await;
    let accept = format!("/api/invites/{token}/accept");
    assert_eq!(marco.post(&accept, Value::Null).await.0, StatusCode::OK);
}
