use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, COOKIE, IF_NONE_MATCH, SET_COOKIE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tower::ServiceExt;
use weacon_server::{AppState, db, map, router};

/// Keeps the session cookie, as a browser does.
struct Client {
    state: AppState,
    cookie: Option<String>,
    /// Set by signing in: the account and its own team.
    user_id: String,
    team_id: String,
    project_id: String,
}

async fn server() -> AppState {
    AppState {
        pool: db::memory().await,
        url: "http://weacon.test".into(),
        github_api: "http://127.0.0.1:1".into(),
        map_wake: Arc::default(),
        anthropic_api: "http://127.0.0.1:1".into(),
        claude_program: "false".into(),
        codex_program: "false".into(),
        files: std::env::temp_dir().join(format!("weacon-test-{:x}", rand::random::<u64>())),
    }
}

impl Client {
    fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
            cookie: None,
            user_id: String::new(),
            team_id: String::new(),
            project_id: String::new(),
        }
    }

    /// A path under this client's own team.
    fn team(&self, rest: &str) -> String {
        format!("/api/teams/{}{rest}", self.team_id)
    }

    /// A path under the project a helper made for this client.
    fn project(&self, rest: &str) -> String {
        self.team(&format!("/projects/{}{rest}", self.project_id))
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
        let email = email.trim().to_lowercase();
        sqlx::query_scalar!("SELECT code FROM sign_in_codes WHERE email = ?", email)
            .fetch_optional(&self.state.pool)
            .await
            .unwrap()
    }

    async fn token(&self, email: &str) -> Option<String> {
        let email = email.trim().to_lowercase();
        sqlx::query_scalar!("SELECT token FROM invites WHERE email = ?", email)
            .fetch_optional(&self.state.pool)
            .await
            .unwrap()
    }

    /// Invites the address to this client's first team and returns the link's token.
    async fn invite(&mut self, email: &str) -> String {
        let (status, _) = self
            .post(&self.team("/invites"), json!({"emails": [email]}))
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
        let attempt = json!({"email": email, "code": code});
        let (status, user) = self.post("/api/sign-in/verify", attempt).await;
        if let Some(id) = user["id"].as_str() {
            self.user_id = id.to_owned();
            self.team_id = user["teams"][0]["id"].as_str().unwrap().to_owned();
        }
        (status, user)
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
async fn a_missing_asset_and_the_reserved_ws_path_are_404() {
    for path in ["/assets/gone.js", "/ws"] {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = router(server().await).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
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
async fn asking_for_a_new_code_buys_no_guesses() {
    let mut client = Client::new(&server().await);
    let request = json!({"email": "ada@example.com", "name": "Ada"});
    client.post("/api/sign-in/code", request.clone()).await;
    let code = client.code("ada@example.com").await.unwrap();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        let attempt = json!({"email": "ada@example.com", "code": wrong});
        client.post("/api/sign-in/verify", attempt).await;
    }

    // Within a minute the first code stands.
    let (status, _) = client.post("/api/sign-in/code", request.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client.code("ada@example.com").await.unwrap(), code);

    // A minute later a new code is made, and the five wrong tries still count against it.
    sqlx::query!("UPDATE sign_in_codes SET expires_at = expires_at - 61")
        .execute(&client.state.pool)
        .await
        .unwrap();
    client.post("/api/sign-in/code", request).await;
    let code = client.code("ada@example.com").await.unwrap();
    let attempt = json!({"email": "ada@example.com", "code": code});
    let (status, _) = client.post("/api/sign-in/verify", attempt).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_session_ends_a_year_after_its_last_use() {
    let mut client = Client::new(&server().await);
    client.sign_in("ada@example.com", Some("Ada")).await;
    let pool = client.state.pool.clone();
    let days_pass = async |days: i64| {
        sqlx::query!(
            "UPDATE sessions SET renewed_at = renewed_at - ? * 24 * 60 * 60",
            days
        )
        .execute(&pool)
        .await
        .unwrap();
    };
    for _ in 0..2 {
        days_pass(300).await;
        let (_, session) = client.get("/api/session").await;
        assert_eq!(session["user"]["name"], "Ada");
    }
    days_pass(366).await;
    let (_, session) = client.get("/api/session").await;
    assert_eq!(session["user"], Value::Null);
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let mut client = Client::new(&server().await);
    let request = json!({"email": "ada@example.com", "name": "Ada"});
    client.post("/api/sign-in/code", request).await;
    let code = client.code("ada@example.com").await.unwrap();
    sqlx::query!("UPDATE sign_in_codes SET expires_at = 0")
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
    // A code whose mail failed doesn't count as asked for, so the next request tries again.
    for _ in 0..2 {
        let request = json!({"email": "ada@example.com"});
        let (status, _) = owner.post("/api/sign-in/code", request).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

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
    let joins = json!({"team_id": ada.team_id});
    assert_eq!((status, joined), (StatusCode::OK, joins));
    (ada, marco)
}

#[tokio::test]
async fn an_invited_person_registers_and_joins() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite(" Marco@Example.com ").await;

    let (_, team) = ada.get(&ada.team("")).await;
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
            {"id": ada.team_id, "name": "Ada's team", "role": "member"},
            {"id": marco.team_id, "name": "Marco's team", "role": "owner"},
        ])
    );
    let (_, team) = marco.get(&ada.team("")).await;
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
    assert_eq!(lena.get(&ada.team("")).await.0, StatusCode::NOT_FOUND);
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

    let (_, team) = ada.get(&ada.team("")).await;
    assert_eq!(team["invites"].as_array().unwrap().len(), 1);
    let invite = team["invites"][0]["id"].as_str().unwrap();
    let invite = ada.team(&format!("/invites/{invite}"));
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
        marco.put(&ada.team(""), name.clone()).await,
        StatusCode::FORBIDDEN
    );
    let invite = json!({"emails": ["lena@example.com"]});
    assert_eq!(
        marco.post(&ada.team("/invites"), invite).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        marco
            .delete(&ada.team(&format!("/members/{}", ada.user_id)))
            .await,
        StatusCode::FORBIDDEN
    );

    assert_eq!(ada.put(&ada.team(""), name).await, StatusCode::NO_CONTENT);
    assert_eq!(ada.get(&ada.team("")).await.1["name"], "Northwind");
    assert_eq!(
        ada.put(&ada.team(""), json!({"name": " "})).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Marco is already in.
    let invite = json!({"emails": ["marco@example.com"]});
    assert_eq!(
        ada.post(&ada.team("/invites"), invite).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Marco's own team isn't Ada's to see.
    assert_eq!(ada.get(&marco.team("")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        ada.put(&marco.team(""), json!({"name": "Mine"})).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_member_is_removed_or_leaves_and_the_owner_stays() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;

    assert_eq!(
        ada.delete(&ada.team(&format!("/members/{}", ada.user_id)))
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        ada.delete(&ada.team(&format!("/members/{}", marco.user_id)))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(marco.get(&ada.team("")).await.0, StatusCode::NOT_FOUND);

    let token = ada.invite("marco@example.com").await;
    marco
        .post(&format!("/api/invites/{token}/accept"), Value::Null)
        .await;
    assert_eq!(
        marco
            .delete(&ada.team(&format!("/members/{}", marco.user_id)))
            .await,
        StatusCode::NO_CONTENT
    );
    let (_, team) = ada.get(&ada.team("")).await;
    assert_eq!(team["members"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn several_addresses_are_invited_at_once_or_none_is() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;

    let invites = json!({"emails": ["marco@example.com", "lena@"]});
    assert_eq!(
        ada.post(&ada.team("/invites"), invites).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(ada.token("marco@example.com").await, None);

    // An address given twice is invited once.
    let invites = json!({"emails": ["marco@example.com", "lena@example.com", " Lena@Example.com"]});
    assert_eq!(
        ada.post(&ada.team("/invites"), invites).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, team) = ada.get(&ada.team("")).await;
    assert_eq!(team["invites"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_expired_invite_is_refused_until_it_is_sent_again() {
    let state = server().await;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let token = ada.invite("marco@example.com").await;
    sqlx::query!("UPDATE invites SET expires_at = 0")
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

/// The repository `northwind/storefront` on the fake GitHub: its branches and commits.
#[derive(Clone, Default)]
struct Repo(Arc<Mutex<RepoState>>);

#[derive(Default)]
struct RepoState {
    /// Branch to its latest commit.
    heads: BTreeMap<String, String>,
    /// Commit to its files, as path and content.
    commits: HashMap<String, Vec<(String, String)>>,
    /// How many archives were downloaded.
    archives: usize,
    /// GitHub says its list of files is cut short.
    truncated: bool,
    /// The archive arrives damaged.
    broken: bool,
}

impl Repo {
    /// Makes `commit`, with these files, the latest on the branch.
    fn push(&self, branch: &str, commit: &str, files: &[(&str, &str)]) {
        let mut repo = self.0.lock().unwrap();
        repo.heads.insert(branch.into(), commit.into());
        let files = files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()));
        repo.commits.insert(commit.into(), files.collect());
    }

    fn archives(&self) -> usize {
        self.0.lock().unwrap().archives
    }
}

/// Starts a GitHub that knows one app, installed on `northwind` with two repositories.
async fn fake_github() -> (String, Repo) {
    // The app's calls must carry a token signed with the key GitHub handed out.
    fn signed(headers: &HeaderMap) -> bool {
        let token = headers["authorization"].to_str().unwrap();
        token.strip_prefix("Bearer ").unwrap().split('.').count() == 3
    }
    let first_page = |request: &Request<Body>| request.uri().query().unwrap().ends_with("page=1");
    let repo = Repo::default();
    let github = Router::new()
        .route(
            "/app-manifests/good-code/conversions",
            post(async || {
                let pem = include_str!("github_app.pem");
                Json(json!({"id": 7, "slug": "weacon-northwind", "pem": pem}))
            }),
        )
        .route(
            "/app/installations",
            get(async |headers: HeaderMap| {
                assert!(signed(&headers));
                Json(json!([{"id": 3, "account": {"login": "northwind"}}]))
            }),
        )
        .route(
            "/app/installations/3/access_tokens",
            post(async |headers: HeaderMap| {
                assert!(signed(&headers));
                Json(json!({"token": "installation-token"}))
            }),
        )
        .route(
            "/installation/repositories",
            get(async move |headers: HeaderMap, request: Request<Body>| {
                assert_eq!(headers["authorization"], "Bearer installation-token");
                let names: &[&str] = if first_page(&request) {
                    &["northwind/storefront", "northwind/docs"]
                } else {
                    &[]
                };
                let repositories: Vec<Value> = names
                    .iter()
                    .map(|name| json!({"full_name": name}))
                    .collect();
                Json(json!({"repositories": repositories}))
            }),
        )
        .route(
            "/repos/northwind/storefront",
            get(async || Json(json!({"default_branch": "main"}))),
        )
        .route(
            "/repos/northwind/storefront/branches",
            get(
                async move |State(repo): State<Repo>, request: Request<Body>| {
                    let repo = repo.0.lock().unwrap();
                    let heads = repo.heads.iter().filter(|_| first_page(&request));
                    let branches: Vec<Value> = heads
                        .map(|(name, commit)| json!({"name": name, "commit": {"sha": commit}}))
                        .collect();
                    Json(json!(branches))
                },
            ),
        )
        .route(
            "/repos/northwind/storefront/git/trees/{commit}",
            get(
                async |State(repo): State<Repo>, Path(commit): Path<String>| {
                    let repo = repo.0.lock().unwrap();
                    let entries: Vec<Value> = repo.commits[&commit]
                        .iter()
                        .map(|(path, text)| {
                            let mut hasher = DefaultHasher::new();
                            text.hash(&mut hasher);
                            let blob = format!("{:x}", hasher.finish());
                            json!({"path": path, "type": "blob", "sha": blob})
                        })
                        .collect();
                    Json(json!({"tree": entries, "truncated": repo.truncated}))
                },
            ),
        )
        .route(
            "/repos/northwind/storefront/tarball/{commit}",
            get(
                async |State(repo): State<Repo>, Path(commit): Path<String>| {
                    let mut repo = repo.0.lock().unwrap();
                    repo.archives += 1;
                    if repo.broken {
                        return b"not an archive".to_vec();
                    }
                    let gzip = flate2::write::GzEncoder::new(Vec::new(), Default::default());
                    let mut archive = tar::Builder::new(gzip);
                    for (path, text) in &repo.commits[&commit] {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(text.len() as u64);
                        header.set_mode(0o644);
                        let path = format!("northwind-storefront-{commit}/{path}");
                        archive
                            .append_data(&mut header, path, text.as_bytes())
                            .unwrap();
                    }
                    archive.into_inner().unwrap().finish().unwrap()
                },
            ),
        )
        .with_state(repo.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async { axum::serve(listener, github).await.unwrap() });
    (address, repo)
}

#[tokio::test]
async fn members_make_projects_and_only_the_owner_deletes_them() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;

    let (status, project) = marco
        .post(&ada.team("/projects"), json!({"name": " Storefront "}))
        .await;
    assert_eq!(status, StatusCode::OK);
    ada.project_id = project["id"].as_str().unwrap().to_owned();
    assert_eq!(
        marco
            .post(&ada.team("/projects"), json!({"name": " "}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let renamed = json!({"name": "Shop"});
    assert_eq!(
        marco.put(&ada.project(""), renamed.clone()).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        marco.put(&ada.team("/projects/none"), renamed).await,
        StatusCode::NOT_FOUND
    );
    let (_, projects) = ada.get(&ada.team("/projects")).await;
    assert_eq!(
        projects,
        json!([{"id": ada.project_id, "name": "Shop", "repo": null, "branches": []}])
    );

    assert_eq!(marco.delete(&ada.project("")).await, StatusCode::FORBIDDEN);
    assert_eq!(ada.delete(&ada.project("")).await, StatusCode::NO_CONTENT);
    assert_eq!(ada.get(&ada.team("/projects")).await.1, json!([]));

    // Marco's own team isn't Ada's to see.
    assert_eq!(
        ada.get(&marco.team("/projects")).await.0,
        StatusCode::NOT_FOUND
    );
}

// On a database file, where requests really run side by side.
#[tokio::test(flavor = "multi_thread")]
async fn projects_edited_at_the_same_time_are_all_saved() {
    let file = std::env::temp_dir().join(format!("weacon-test-{}.db", std::process::id()));
    let file = file.to_str().unwrap().to_owned();
    let pool = db::open(&file).await.unwrap();
    let state = AppState {
        pool,
        ..server().await
    };
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    let mut renames = Vec::new();
    for name in ["One", "Two", "Three", "Four"] {
        let (_, project) = ada
            .post(&ada.team("/projects"), json!({"name": name}))
            .await;
        let path = ada.team(&format!("/projects/{}", project["id"].as_str().unwrap()));
        let mut client = Client::new(&state);
        client.cookie = ada.cookie.clone();
        let renamed = json!({"name": format!("{name} again"), "branches": []});
        renames.push(tokio::spawn(
            async move { client.put(&path, renamed).await },
        ));
    }
    for rename in renames {
        assert_eq!(rename.await.unwrap(), StatusCode::NO_CONTENT);
    }
    state.pool.close().await;
    for side in ["", "-wal", "-shm"] {
        std::fs::remove_file(format!("{file}{side}")).ok();
    }
}

#[tokio::test]
async fn the_team_owner_registers_a_github_app_and_projects_pick_its_repositories() {
    let mut state = server().await;
    state.github_api = fake_github().await.0;
    let (mut ada, mut marco) = ada_and_marco(&state).await;

    let nothing = json!({"app": null, "account": null, "repos": []});
    assert_eq!(marco.get(&ada.team("/github")).await.1, nothing);
    let with_repo = json!({"name": "Storefront", "repo": "northwind/storefront"});
    assert_eq!(
        marco
            .post(&ada.team("/projects"), with_repo.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let code = json!({"code": "good-code"});
    assert_eq!(
        marco.post(&ada.team("/github"), code.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.post(&ada.team("/github"), json!({"code": "bad-code"}))
            .await
            .0,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        ada.post(&ada.team("/github"), code).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        marco.get(&ada.team("/github")).await.1,
        json!({
            "app": "weacon-northwind", "account": "northwind",
            "repos": ["northwind/storefront", "northwind/docs"],
        })
    );

    let (status, project) = marco.post(&ada.team("/projects"), with_repo).await;
    assert_eq!(status, StatusCode::OK);
    ada.project_id = project["id"].as_str().unwrap().to_owned();
    assert_eq!(
        ada.get(&ada.team("/projects")).await.1[0]["repo"],
        "northwind/storefront"
    );
    let elsewhere = json!({"name": "Storefront", "repo": "someone/else"});
    assert_eq!(
        marco.put(&ada.project(""), elsewhere).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let without = json!({"name": "Storefront"});
    assert_eq!(
        marco.put(&ada.project(""), without).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        ada.get(&ada.team("/projects")).await.1[0]["repo"],
        Value::Null
    );
}

#[tokio::test]
async fn forgetting_the_github_app_keeps_the_projects() {
    let mut state = server().await;
    state.github_api = fake_github().await.0;
    let (mut ada, mut marco) = ada_and_marco(&state).await;
    ada.post(&ada.team("/github"), json!({"code": "good-code"}))
        .await;
    let with_repo = json!({"name": "Storefront", "repo": "northwind/docs"});
    ada.post(&ada.team("/projects"), with_repo).await;

    assert_eq!(
        marco.delete(&ada.team("/github")).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.delete(&ada.team("/github")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get(&ada.team("/github")).await.1["app"], Value::Null);
    assert_eq!(
        ada.get(&ada.team("/projects")).await.1[0]["repo"],
        "northwind/docs"
    );
}

const PRICE_TS: &str = "export function formatPrice(cents: number): string {
  return String(cents)
}
export function init() {}
";
const APP_TS: &str = "import { formatPrice } from \"./price\"
export class Cart {
  total() {
    return formatPrice(1)
  }
}
init()
";
const UTIL_PY: &str = "def init():
    pass

class Basket:
    pass
";
const LIB_RS: &str = "pub struct Order;

pub fn ship(order: Order) {}
";

/// Ada's team with its GitHub App, and project 1 tracking `main` of `northwind/storefront`.
async fn mapped_project(files: &[(&str, &str)]) -> (AppState, Client, Repo) {
    let mut state = server().await;
    let (address, repo) = fake_github().await;
    state.github_api = address;
    let mut ada = Client::new(&state);
    ada.sign_in("ada@example.com", Some("Ada")).await;
    ada.post(&ada.team("/github"), json!({"code": "good-code"}))
        .await;
    repo.push("main", "c1", files);
    let project =
        json!({"name": "Storefront", "repo": "northwind/storefront", "branches": ["main"]});
    let (status, project) = ada.post(&ada.team("/projects"), project).await;
    assert_eq!(status, StatusCode::OK);
    ada.project_id = project["id"].as_str().unwrap().to_owned();
    map::round(&state).await;
    (state, ada, repo)
}

fn file<'a>(map: &'a Value, path: &str) -> &'a Value {
    let mut files = map["files"].as_array().unwrap().iter();
    files.find(|file| file["path"] == path).unwrap()
}

/// The names of the symbols the map has for a file.
fn symbols(map: &Value, path: &str) -> Vec<String> {
    let symbols = file(map, path)["symbols"].as_array().unwrap().iter();
    symbols
        .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_tracked_branch_is_mapped_with_symbols_and_uses() {
    // A bundle on one long line defines the name the app calls, and is listed without being read.
    let bundle = format!("function formatPrice() {{}}{}", ";".repeat(5_000));
    let files = [
        ("src/app.ts", APP_TS),
        ("src/price.ts", PRICE_TS),
        ("public/app.js", bundle.as_str()),
        ("tools/util.py", UTIL_PY),
        ("server/lib.rs", LIB_RS),
        ("README.md", "# Storefront\n\nA shop.\n"),
    ];
    let (_, mut ada, _) = mapped_project(&files).await;

    let (_, branches) = ada
        .get(&ada.team("/github/branches?repo=northwind/storefront"))
        .await;
    assert_eq!(branches, json!({"default": "main", "branches": ["main"]}));
    let (_, projects) = ada.get(&ada.team("/projects")).await;
    assert_eq!(
        projects[0]["branches"],
        json!([{"name": "main", "head_sha": "c1", "commit_sha": "c1", "error": null}])
    );

    let (status, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(map["commit"], "c1");
    assert_eq!(map["files"].as_array().unwrap().len(), 6);
    assert_eq!(symbols(&map, "public/app.js"), [""; 0]);
    assert_eq!(symbols(&map, "src/price.ts"), ["formatPrice", "init"]);
    assert_eq!(symbols(&map, "src/app.ts"), ["Cart", "total"]);
    assert_eq!(symbols(&map, "tools/util.py"), ["init", "Basket"]);
    assert_eq!(symbols(&map, "server/lib.rs"), ["Order", "ship"]);
    assert_eq!(
        map["files"][0],
        json!({"path": "README.md", "language": null, "lines": 3, "symbols": []})
    );
    assert_eq!(
        map["files"][3]["symbols"][1],
        json!({"name": "total", "kind": "method", "line": 3, "end_line": 5})
    );
    // The app imports the price file. `init` is defined in two languages and imported from neither.
    assert_eq!(map["uses"], json!([["src/app.ts", "src/price.ts"]]));

    assert_eq!(
        ada.get(&ada.project("/map?branch=other")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_new_commit_reads_only_the_content_that_is_new() {
    let files = [("src/app.ts", APP_TS), ("src/price.ts", PRICE_TS)];
    let (state, mut ada, repo) = mapped_project(&files).await;
    assert_eq!(repo.archives(), 1);

    // Nothing moved, so nothing is mapped.
    map::round(&state).await;
    assert_eq!(repo.archives(), 1);

    // A commit that only moves a file brings no new content.
    repo.push(
        "main",
        "c2",
        &[("app.ts", APP_TS), ("src/price.ts", PRICE_TS)],
    );
    map::round(&state).await;
    assert_eq!(repo.archives(), 1);
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(map["commit"], "c2");
    assert_eq!(symbols(&map, "app.ts"), ["Cart", "total"]);

    repo.push("main", "c3", &[("app.ts", "export function start() {}\n")]);
    map::round(&state).await;
    assert_eq!(repo.archives(), 2);
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(map["files"].as_array().unwrap().len(), 1);
    assert_eq!(symbols(&map, "app.ts"), ["start"]);
    // The contents no commit lists any more are forgotten.
    let kept = sqlx::query_scalar!("SELECT COUNT(*) FROM blobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
}

#[tokio::test]
async fn a_file_s_language_follows_its_name() {
    let (state, mut ada, repo) = mapped_project(&[("notes.txt", "fn start() {}\n")]).await;
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(map["files"][0]["language"], Value::Null);

    repo.push("main", "c2", &[("notes.rs", "fn start() {}\n")]);
    map::round(&state).await;
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(map["files"][0]["language"], "Rust");
    // The content is the same, and is read again as what it now is.
    assert_eq!(symbols(&map, "notes.rs"), ["start"]);
}

#[tokio::test]
async fn each_language_is_read_by_its_own_grammar() {
    // A path, its language, its source and a name the source defines.
    let files = [
        (
            "a.go",
            "Go",
            "package shop\n\nfunc Total() int { return 1 }\n",
            "Total",
        ),
        (
            "a.java",
            "Java",
            "class Cart { int total() { return 1; } }\n",
            "Cart",
        ),
        ("a.c", "C", "int total(void) { return 1; }\n", "total"),
        ("a.h", "C", "struct cart { int total; };\n", "cart"),
        (
            "a.cpp",
            "C++",
            "class Cart { public: int total(); };\n",
            "Cart",
        ),
        (
            "a.cs",
            "C#",
            "class Cart { int Total() { return 1; } }\n",
            "Cart",
        ),
        (
            "a.rb",
            "Ruby",
            "class Cart\n  def total\n    1\n  end\nend\n",
            "Cart",
        ),
        (
            "a.swift",
            "Swift",
            "class Cart {\n    func total() -> Int { return 1 }\n}\n",
            "Cart",
        ),
        (
            "a.dart",
            "Dart",
            "class Cart {\n  int total() => 1;\n}\n",
            "Cart",
        ),
        (
            "a.ex",
            "Elixir",
            "defmodule Cart do\n  def total, do: 1\nend\n",
            "Cart",
        ),
        (
            "a.lua",
            "Lua",
            "function total()\n  return 1\nend\n",
            "total",
        ),
        (
            "a.kt",
            "Kotlin",
            "class Cart {\n    fun total(): Int = 1\n}\n",
            "total",
        ),
        (
            "a.scala",
            "Scala",
            "object Cart {\n  def total: Int = 1\n}\n",
            "Cart",
        ),
        ("a.sh", "Bash", "total() {\n  echo 1\n}\n", "total"),
        (
            "a.zig",
            "Zig",
            "const Cart = struct {};\n\npub fn total() u32 {\n    return 1;\n}\n",
            "Cart",
        ),
    ];
    let sources = files.map(|(path, _, source, _)| (path, source));
    let (_, mut ada, _) = mapped_project(&sources).await;
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    for (path, language, _, name) in files {
        assert_eq!(file(&map, path)["language"], language, "{path}");
        assert!(symbols(&map, path).contains(&name.to_owned()), "{path}");
    }
}

#[tokio::test]
async fn a_component_is_used_where_its_tag_is_written() {
    let files = [
        (
            "Card.tsx",
            "export function Card() {\n  return <div />\n}\n",
        ),
        ("App.tsx", "export function App() {\n  return <Card />\n}\n"),
        (
            "Price.vue",
            "<template>\n  <b>{{ shown }}</b>\n</template>\n\n<script setup lang=\"ts\">\nfunction shown() {}\n</script>\n",
        ),
        // What is commented out isn't there.
        (
            "Cart.vue",
            "<template>\n  <Price />\n  <!-- <Chip /> -->\n</template>\n<!-- <script>function old() {}</script> -->\n",
        ),
        // The same content under another name is another component.
        ("Row.svelte", "<ul><li><Chip label=\"a\" /></li></ul>\n"),
        (
            "Chip.svelte",
            "<script>\n  export let label\n</script>\n\n<i>{label}</i>\n",
        ),
        ("List.svelte", "<ul><li><Chip label=\"a\" /></li></ul>\n"),
    ];
    let (_, mut ada, _) = mapped_project(&files).await;
    let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
    assert_eq!(
        map["uses"],
        json!([
            ["App.tsx", "Card.tsx"],
            ["Cart.vue", "Price.vue"],
            ["List.svelte", "Chip.svelte"],
            ["Row.svelte", "Chip.svelte"]
        ])
    );
    assert_eq!(symbols(&map, "Cart.vue"), ["Cart"]);
    assert_eq!(symbols(&map, "Row.svelte"), ["Row"]);
    // A script's line is its line in the whole file.
    assert_eq!(
        file(&map, "Price.vue")["symbols"],
        json!([
            {"name": "Price", "kind": "component", "line": 1, "end_line": 7},
            {"name": "shown", "kind": "function", "line": 6, "end_line": 6},
        ])
    );
}

#[tokio::test]
async fn a_commit_that_can_t_be_read_whole_keeps_the_old_map() {
    let (state, mut ada, repo) = mapped_project(&[("src/app.ts", APP_TS)]).await;
    repo.push("main", "c2", &[("src/price.ts", PRICE_TS)]);
    let mut mapped = async |truncated, broken| {
        {
            let mut repo = repo.0.lock().unwrap();
            (repo.truncated, repo.broken) = (truncated, broken);
        }
        map::round(&state).await;
        let (_, map) = ada.get(&ada.project("/map?branch=main")).await;
        let (_, projects) = ada.get(&ada.team("/projects")).await;
        let error = projects[0]["branches"][0]["error"].as_str();
        (map["commit"].clone(), error.map(str::to_owned))
    };

    let (commit, error) = mapped(true, false).await;
    assert_eq!(commit, "c1");
    assert_eq!(
        error.unwrap(),
        "This repository has too many files to map yet"
    );
    let (commit, error) = mapped(false, true).await;
    assert_eq!(commit, "c1");
    assert!(
        error
            .unwrap()
            .starts_with("The commit's archive couldn't be read")
    );
    assert_eq!(mapped(false, false).await, (json!("c2"), None));
}

#[tokio::test]
async fn each_tracked_branch_has_its_own_map() {
    let files = [("src/price.ts", PRICE_TS), ("old/price.ts", PRICE_TS)];
    let (state, mut ada, repo) = mapped_project(&files).await;
    repo.push(
        "dev",
        "d1",
        &[("src/price.ts", PRICE_TS), ("src/app.ts", APP_TS)],
    );
    let mut project = json!({
        "name": "Storefront", "repo": "northwind/storefront", "branches": ["main", "dev", "gone"],
    });
    ada.put(&ada.project(""), project.clone()).await;
    map::round(&state).await;

    let (_, main) = ada.get(&ada.project("/map?branch=main")).await;
    let (_, dev) = ada.get(&ada.project("/map?branch=dev")).await;
    assert_eq!(main["files"].as_array().unwrap().len(), 2);
    assert_eq!(dev["files"].as_array().unwrap().len(), 2);
    // The same content at two paths is read once.
    assert_eq!(symbols(&main, "old/price.ts"), ["formatPrice", "init"]);
    assert_eq!(dev["commit"], "d1");
    let (_, projects) = ada.get(&ada.team("/projects")).await;
    assert_eq!(projects[0]["branches"][1]["name"], "gone");
    assert_eq!(
        projects[0]["branches"][1]["error"],
        "This branch isn't on GitHub"
    );

    project["branches"] = json!(["main"]);
    ada.put(&ada.project(""), project.clone()).await;
    assert_eq!(
        ada.get(&ada.project("/map?branch=dev")).await.0,
        StatusCode::NOT_FOUND
    );
    let kept = sqlx::query_scalar!("SELECT COUNT(*) FROM blobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);

    // Without a repository there is nothing to map.
    project["repo"] = Value::Null;
    ada.put(&ada.project(""), project).await;
    assert_eq!(
        ada.get(&ada.team("/projects")).await.1[0]["branches"],
        json!([])
    );
}

/// Starts an Anthropic API that plans one "works" and one "built" page, and keeps the prompts it got.
async fn fake_anthropic() -> (String, Arc<Mutex<Vec<String>>>) {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let api = Router::new()
        .route(
            "/v1/messages",
            post(
                async |State(prompts): State<Arc<Mutex<Vec<String>>>>,
                       headers: HeaderMap,
                       Json(body): Json<Value>| {
                    let key = headers["x-api-key"].to_str().unwrap();
                    if !["good-key", "slow-key", "odd-key"].contains(&key) {
                        let error = json!({"error": {"message": "invalid x-api-key"}});
                        return (StatusCode::UNAUTHORIZED, Json(error));
                    }
                    // The slow key answers only once a test has put "continue" among the prompts.
                    while key == "slow-key" && !prompts.lock().unwrap().contains(&"continue".into()) {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    assert_eq!(body["model"], "claude-haiku-5-5");
                    let system = body["system"].as_str().unwrap();
                    let prompt = body["messages"][0]["content"].as_str().unwrap();
                    prompts.lock().unwrap().push(prompt.to_owned());
                    let note = json!([{
                        "quote": "By hand.", "says": "It is done by the code.",
                        "place": "src/app.ts:1", "instead": "By the code.",
                    }]);
                    let text = if key == "odd-key" {
                        "} {".to_owned()
                    } else if system.contains("Someone asks for a change") {
                        // About a page that exists, a writer gives no set and no title.
                        let page = prompt.split("The page they had open is ").nth(1);
                        match page.and_then(|page| page.split(':').next()) {
                            Some(page) => json!({
                                "page": page, "section": null, "title": null,
                                "summary": "Buying.", "files": ["src/app.ts"],
                            }),
                            None => json!({
                                "page": null, "section": "works", "title": "Refunds",
                                "summary": "Getting money back.", "files": ["src/app.ts"],
                            }),
                        }
                        .to_string()
                    } else if system.contains("Say which files") {
                        json!({"files": ["src/app.ts", "no/such.ts"]}).to_string()
                    } else if system.contains("You check one page") {
                        note.to_string()
                    } else if system.contains("Answer with JSON") {
                        // Prices is planned only while its file is in the repository.
                        let prices = json!({
                            "title": "Prices", "summary": "Formatting prices.",
                            "files": ["src/price.ts", "vendor/lib.ts", "no/such.ts"],
                        });
                        let built = prompt.contains("src/price.ts").then_some(prices);
                        json!({
                            "left_out": ["vendor"],
                            "works": [{"title": "Buy", "summary": "Buying.", "files": ["src/app.ts"]}],
                            "built": built.into_iter().collect::<Vec<_>>(),
                        })
                        .to_string()
                    } else if system.contains("---REASON---") {
                        // A page that exists is written again; a person's words in it get a note.
                        let page = if prompt.contains("The page as it is now") {
                            "A page, written again."
                        } else {
                            "A new page."
                        };
                        let notes = if prompt.contains("By hand.") { note } else { json!([]) };
                        format!("{page}\n---REASON---\nThe code changed.\n---NOTES---\n{notes}")
                    } else if system.contains("overview page") {
                        "The overview.".to_owned()
                    } else {
                        "A page, citing `src/price.ts:1`.".to_owned()
                    };
                    let answer = json!({"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"});
                    (StatusCode::OK, Json(answer))
                },
            ),
        )
        .with_state(prompts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async { axum::serve(listener, api).await.unwrap() });
    (address, prompts)
}

/// Starts writing project 1's docs from `main` and returns them once the run has ended.
async fn written_docs(client: &mut Client) -> Value {
    let (status, _) = client
        .post(&client.project("/docs"), json!({"branch": "main"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    settled_docs(client).await
}

/// Project 1's docs, once no run is writing them and no page is being checked.
async fn settled_docs(client: &mut Client) -> Value {
    let ended = async {
        loop {
            let (_, docs) = client.get(&client.project("/docs")).await;
            let mut pages = docs["pages"].as_array().unwrap().iter();
            if docs["state"] != "running" && pages.all(|page| page["checking"] == false) {
                return docs;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    let patience = std::time::Duration::from_secs(10);
    tokio::time::timeout(patience, ended).await.unwrap()
}

/// Ada's mapped project with docs, and a new run held up at the slow writer's first answer.
async fn docs_with_a_run_held_up() -> (Client, Arc<Mutex<Vec<String>>>) {
    let (_, mut ada, _) =
        mapped_project(&[("src/app.ts", APP_TS), ("src/price.ts", PRICE_TS)]).await;
    let (address, prompts) = fake_anthropic().await;
    ada.state.anthropic_api = address;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    assert_eq!(
        written_docs(&mut ada).await["pages"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    prompts.lock().unwrap().clear();

    let writer = json!({"kind": "claude_api", "key": "slow-key"});
    ada.put(&ada.team("/writer"), writer).await;
    let start = json!({"branch": "main"});
    assert_eq!(
        ada.post(&ada.project("/docs"), start).await.0,
        StatusCode::ACCEPTED
    );
    // The run is held up once it has asked for its plan.
    while ada.get(&ada.project("/docs")).await.1["message"] != "Planning the pages" {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (ada, prompts)
}

/// Lets the held-up call return. The run has lost its project, so it asks the writer for nothing more.
async fn let_the_run_go_on(prompts: &Mutex<Vec<String>>) {
    prompts.lock().unwrap().push("continue".into());
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The outline it was asked for, and the word above.
    assert_eq!(prompts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn changing_the_repository_keeps_the_pages_and_stops_a_run_under_way() {
    let (mut ada, prompts) = docs_with_a_run_held_up().await;
    let project = json!({"name": "Storefront", "repo": "northwind/docs"});
    assert_eq!(
        ada.put(&ada.project(""), project).await,
        StatusCode::NO_CONTENT
    );
    // People may have written in the pages, so they stay, without what tied them to the old repository.
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(
        (&docs["state"], &docs["branch"], &docs["commit"]),
        (&json!("idle"), &Value::Null, &Value::Null)
    );
    let buy = &docs["pages"][1];
    assert_eq!(buy["markdown"], "A page, citing `src/price.ts:1`.");
    assert_eq!((&buy["files"], &buy["commit"]), (&json!([]), &Value::Null));
    let history = format!("/docs/pages/{}/versions", buy["id"].as_str().unwrap());
    assert_eq!(ada.get(&ada.project(&history)).await.0, StatusCode::OK);

    let_the_run_go_on(&prompts).await;
    let (_, after) = ada.get(&ada.project("/docs")).await;
    assert_eq!(after, docs);
}

#[tokio::test]
async fn deleting_a_project_stops_its_run() {
    let (mut ada, prompts) = docs_with_a_run_held_up().await;
    ada.delete(&ada.project("")).await;

    let_the_run_go_on(&prompts).await;
    let pages = sqlx::query_scalar!("SELECT COUNT(*) FROM docs");
    assert_eq!(pages.fetch_one(&ada.state.pool).await.unwrap(), 0);
}

#[tokio::test]
async fn a_shared_link_shows_a_copy_of_the_user_docs_to_anyone() {
    let (state, mut ada, _) =
        mapped_project(&[("src/app.ts", APP_TS), ("src/price.ts", PRICE_TS)]).await;
    let shares = ada.project("/shares");
    let (status, _) = ada.post(&shares, Value::Null).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    written_docs(&mut ada).await;
    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    for (status, _) in [
        eve.get(&shares).await,
        eve.post(&shares, Value::Null).await,
        eve.post(
            &eve.team(&format!("/projects/{}/shares", ada.project_id)),
            Value::Null,
        )
        .await,
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    let (status, share) = ada.post(&shares, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let token = share["token"].as_str().unwrap();
    let (_, listed) = ada.get(&shares).await;
    assert_eq!(listed[0]["token"], token);
    assert_eq!(listed[0]["created_by"], "Ada");

    // Without a session, and still the same after the docs are deleted with the repository.
    let public = format!("/api/shares/{token}");
    let mut anyone = Client::new(&state);
    let (status, shared) = anyone.get(&public).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shared["title"], "Storefront");
    let page = json!([{"title": "Buy", "markdown": "A page, citing `src/price.ts:1`."}]);
    assert_eq!(shared["pages"], page);
    ada.put(&ada.project(""), json!({"name": "Shop", "branches": []}))
        .await;
    assert_eq!(anyone.get(&public).await.1, shared);

    let link = format!("{shares}/{token}");
    assert_eq!(eve.delete(&link).await, StatusCode::NOT_FOUND);
    assert_eq!(ada.delete(&link).await, StatusCode::NO_CONTENT);
    assert_eq!(anyone.get(&public).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_project_ends_its_shared_links() {
    let (state, mut ada, _) = mapped_project(&[("src/app.ts", APP_TS)]).await;
    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    written_docs(&mut ada).await;
    let (_, share) = ada.post(&ada.project("/shares"), Value::Null).await;
    let public = format!("/api/shares/{}", share["token"].as_str().unwrap());
    ada.delete(&ada.project("")).await;
    assert_eq!(
        Client::new(&state).get(&public).await.0,
        StatusCode::NOT_FOUND
    );
}

/// Ada's mapped project with the docs a run wrote: an overview, "Buy" and "Prices".
async fn project_with_docs() -> (AppState, Client) {
    let (state, mut ada, _) =
        mapped_project(&[("src/app.ts", APP_TS), ("src/price.ts", PRICE_TS)]).await;
    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    written_docs(&mut ada).await;
    (state, ada)
}

#[tokio::test]
async fn a_member_edits_a_page_and_every_text_stays_in_its_history() {
    let (state, mut ada) = project_with_docs().await;
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    let buy = docs["pages"][1].clone();
    assert_eq!(buy["title"], "Buy");
    let page = ada.project(&format!("/docs/pages/{}", buy["id"].as_str().unwrap()));

    let edit = json!({"title": " Buying ", "markdown": "By hand.", "version": buy["version"]});
    assert_eq!(ada.put(&page, edit.clone()).await, StatusCode::NO_CONTENT);
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(docs["pages"][1]["title"], "Buying");
    assert_eq!(docs["pages"][1]["markdown"], "By hand.");

    // An edit that started from the text before is refused, and changes nothing.
    assert_eq!(ada.put(&page, edit).await, StatusCode::CONFLICT);
    let untitled = json!({"title": " ", "markdown": "", "version": docs["pages"][1]["version"]});
    assert_eq!(
        ada.put(&page, untitled).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let (_, versions) = ada.get(&format!("{page}/versions")).await;
    let versions = versions.as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["id"], docs["pages"][1]["version"]);
    assert_eq!(versions[0]["author"], "Ada");
    assert_eq!(versions[0]["markdown"], "By hand.");
    assert_eq!(versions[1]["author"], Value::Null);
    assert_eq!(versions[1]["title"], "Buy");

    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    let hers = json!({"title": "Hers", "markdown": "", "version": versions[0]["id"]});
    assert_eq!(eve.put(&page, hers).await, StatusCode::NOT_FOUND);
    assert_eq!(
        eve.get(&format!("{page}/versions")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(eve.delete(&page).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_member_adds_orders_and_deletes_pages() {
    let (_, mut ada) = project_with_docs().await;
    let pages = ada.project("/docs/pages");
    let odd = json!({"section": "overview", "title": "Another overview"});
    assert_eq!(
        ada.post(&pages, odd).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, added) = ada
        .post(&pages, json!({"section": "works", "title": "Refunds"}))
        .await;
    assert_eq!(status, StatusCode::OK);

    let titles = async |ada: &mut Client| {
        let (_, docs) = ada.get(&ada.project("/docs")).await;
        let pages = docs["pages"].as_array().unwrap().clone();
        let titles = pages
            .iter()
            .map(|page| page["title"].as_str().unwrap().to_owned());
        (pages.clone(), titles.collect::<Vec<_>>())
    };
    let (listed, names) = titles(&mut ada).await;
    assert_eq!(names, ["Overview", "Buy", "Prices", "Refunds"]);
    assert_eq!(listed[3]["id"], added["id"]);
    assert_eq!(listed[3]["section"], "works");

    let order = json!([
        listed[0]["id"],
        listed[3]["id"],
        listed[1]["id"],
        listed[2]["id"]
    ]);
    assert_eq!(ada.put(&pages, order).await, StatusCode::NO_CONTENT);
    assert_eq!(
        titles(&mut ada).await.1,
        ["Overview", "Refunds", "Buy", "Prices"]
    );

    let refunds = format!("{pages}/{}", added["id"].as_str().unwrap());
    assert_eq!(ada.delete(&refunds).await, StatusCode::NO_CONTENT);
    assert_eq!(ada.delete(&refunds).await, StatusCode::NOT_FOUND);
    assert_eq!(titles(&mut ada).await.1, ["Overview", "Buy", "Prices"]);
}

/// Sends `bytes` as a file of Ada's project, and returns the answer.
async fn upload(client: &Client, path: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let request = Request::post(path)
        .header(COOKIE, client.cookie.as_deref().unwrap())
        .body(Body::from(bytes.to_vec()))
        .unwrap();
    let response = router(client.state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// The status, media type, disposition and bytes that `path` answers with, asked without a session.
async fn fetched(state: &AppState, path: &str) -> (StatusCode, String, String, Vec<u8>) {
    let request = Request::get(path).body(Body::empty()).unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let header = |name| match response.headers().get(name) {
        Some(value) => value.to_str().unwrap().to_owned(),
        None => String::new(),
    };
    let (kind, disposition) = (header(CONTENT_TYPE), header(CONTENT_DISPOSITION));
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, kind, disposition, body.to_vec())
}

#[tokio::test]
async fn a_file_is_kept_for_its_team_and_served_by_a_link_whose_page_names_it() {
    let (state, mut ada) = project_with_docs().await;
    std::fs::create_dir_all(&state.files).unwrap();
    let files = ada.project("/files");
    let png = b"\x89PNG\r\n\x1a\n and the rest";
    let svg = b"<svg onload='alert(1)'/>";

    let large = [png.as_slice(), &vec![0; 5_000_000]].concat();
    assert_eq!(
        upload(&ada, &files, &large).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        upload(&ada, &files, &vec![0; 20_000_001]).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    assert_eq!(upload(&eve, &files, png).await.0, StatusCode::NOT_FOUND);

    let (status, shown) = upload(&ada, &files, png).await;
    assert_eq!((status, &shown["image"]), (StatusCode::OK, &json!(true)));
    let shown = shown["id"].as_str().unwrap().to_owned();
    let (_, unshown) = upload(&ada, &files, png).await;
    let unshown = unshown["id"].as_str().unwrap();
    // Anything that isn't an image is kept as an attachment, under a name made plain.
    let named = format!("{files}?name=a%22b%2Fdrawing.svg");
    let (_, attached) = upload(&ada, &named, svg).await;
    assert_eq!(attached["image"], false);
    let attached = attached["id"].as_str().unwrap();
    assert_eq!(
        eve.get(&format!("{files}/{shown}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fetched(&state, &format!("{files}/{shown}")).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Only what a shared page names can be had through the link.
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    let buy = &docs["pages"][1];
    let page = ada.project(&format!("/docs/pages/{}", buy["id"].as_str().unwrap()));
    let markdown = format!("![The cart](image:{shown}) and [a drawing](file:{attached})");
    let edit = json!({"title": "Buy", "markdown": markdown, "version": buy["version"]});
    ada.put(&page, edit).await;
    let (_, share) = ada.post(&ada.project("/shares"), Value::Null).await;
    let public = format!("/api/shares/{}/files", share["token"].as_str().unwrap());
    let (status, kind, disposition, bytes) = fetched(&state, &format!("{public}/{shown}")).await;
    assert_eq!((status, kind.as_str()), (StatusCode::OK, "image/png"));
    assert_eq!(
        (disposition.as_str(), bytes.as_slice()),
        ("inline", &png[..])
    );
    // A browser that has the file asks before using its copy, and is told the copy is good.
    let again = Request::get(format!("{public}/{shown}"))
        .header(IF_NONE_MATCH, format!("\"{shown}\""))
        .body(Body::empty())
        .unwrap();
    let answer = router(state.clone()).oneshot(again).await.unwrap();
    assert_eq!(answer.status(), StatusCode::NOT_MODIFIED);
    // An attachment is downloaded and never shown, whatever it holds.
    let (status, kind, disposition, bytes) = fetched(&state, &format!("{public}/{attached}")).await;
    assert_eq!(
        (status, kind.as_str()),
        (StatusCode::OK, "application/octet-stream")
    );
    assert_eq!(disposition, "attachment; filename=\"abdrawing.svg\"");
    assert_eq!(bytes, svg);
    assert_eq!(
        fetched(&state, &format!("{public}/{unshown}")).await.0,
        StatusCode::NOT_FOUND
    );

    ada.delete(&ada.project("")).await;
    assert_eq!(std::fs::read_dir(&state.files).unwrap().count(), 0);
    std::fs::remove_dir(&state.files).unwrap();
}

#[tokio::test]
async fn a_run_over_existing_docs_proposes_changes_and_replaces_nothing() {
    let (state, mut ada, repo) =
        mapped_project(&[("src/app.ts", APP_TS), ("src/price.ts", PRICE_TS)]).await;
    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["proposals"], json!([]));
    let buy = &docs["pages"][1];
    let page = ada.project(&format!("/docs/pages/{}", buy["id"].as_str().unwrap()));
    let edit = json!({"title": "Buy", "markdown": "By hand.", "version": buy["version"]});
    ada.put(&page, edit).await;
    let before = settled_docs(&mut ada).await;

    // The pages stay as they are. The overview would come out the same, so it has no proposal.
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["pages"], before["pages"]);
    let proposals = docs["proposals"].as_array().unwrap();
    let titles: Vec<&Value> = proposals
        .iter()
        .map(|proposal| &proposal["title"])
        .collect();
    assert_eq!(titles, ["Buy", "Prices"]);
    let bought = &proposals[0];
    assert_eq!(bought["page"], buy["id"]);
    assert_eq!(bought["base"], before["pages"][1]["version"]);
    assert_eq!(bought["markdown"], "A page, written again.");
    assert_eq!(bought["reason"], "The code changed.");
    // Only the page a person edited gets a note on their words.
    assert_eq!(bought["notes"][0]["quote"], "By hand.");
    assert_eq!(proposals[1]["notes"], json!([]));

    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    let proposals_path = ada.project("/docs/proposals");
    let proposal =
        |proposal: &Value| format!("{proposals_path}/{}", proposal["id"].as_str().unwrap());
    let (status, _) = eve.post(&proposal(bought), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(eve.delete(&proposal(bought)).await, StatusCode::NOT_FOUND);

    // Accepted from a view of the page as it was before the edit, nothing happens and the proposal stays.
    let stale = json!({"version": buy["version"]});
    let (status, _) = ada.post(&proposal(bought), stale).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(docs["pages"], before["pages"]);
    assert_eq!(docs["proposals"].as_array().unwrap().len(), 2);

    // Accepted whole, the text is the writer's; with some changes left out, the person's.
    let seen = json!({"version": before["pages"][2]["version"]});
    let (status, _) = ada.post(&proposal(&proposals[1]), seen).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let mixed =
        json!({"version": before["pages"][1]["version"], "markdown": "By hand, written again."});
    ada.post(&proposal(bought), mixed).await;
    let (status, _) = ada.post(&proposal(bought), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let docs = settled_docs(&mut ada).await;
    assert_eq!(docs["proposals"], json!([]));
    assert_eq!(docs["pages"][1]["markdown"], "By hand, written again.");
    assert_eq!(docs["pages"][2]["markdown"], "A page, written again.");
    let (_, versions) = ada.get(&format!("{page}/versions")).await;
    assert_eq!(versions[0]["author"], "Ada");
    let prices = docs["pages"][2]["id"].as_str().unwrap();
    let (_, versions) = ada
        .get(&ada.project(&format!("/docs/pages/{prices}/versions")))
        .await;
    assert_eq!(versions[0]["author"], Value::Null);

    // A page whose files left the repository is proposed for removal, and a rejected proposal changes nothing.
    repo.push("main", "c2", &[("src/app.ts", APP_TS)]);
    map::round(&state).await;
    let docs = written_docs(&mut ada).await;
    let proposals = docs["proposals"].as_array().unwrap();
    let removal = proposals
        .iter()
        .find(|proposal| proposal["title"] == "Prices");
    let removal = removal.unwrap();
    assert_eq!(removal["markdown"], Value::Null);
    let other = proposals.iter().find(|proposal| proposal["title"] == "Buy");
    assert_eq!(
        ada.delete(&proposal(other.unwrap())).await,
        StatusCode::NO_CONTENT
    );
    // A page keeps the commit its text was written from, whatever a later run looked at.
    assert_eq!(docs["commit"], "c2");
    assert_eq!(docs["pages"][1]["commit"], "c1");
    let seen = json!({"version": docs["pages"][2]["version"]});
    let (status, _) = ada.post(&proposal(removal), seen).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let docs = settled_docs(&mut ada).await;
    let titles: Vec<&Value> = docs["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| &page["title"])
        .collect();
    assert_eq!(titles, ["Overview", "Buy"]);
    assert_eq!(docs["pages"][1]["markdown"], "By hand, written again.");
}

#[tokio::test]
async fn a_request_is_answered_with_one_proposal() {
    let (state, mut ada) = project_with_docs().await;
    let requests = ada.project("/docs/requests");
    let request = json!({"text": "Add a page on refunds", "branch": "main"});
    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    assert_eq!(
        eve.post(&requests, request.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    let empty = json!({"text": " ", "branch": "main"});
    assert_eq!(
        ada.post(&requests, empty).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    assert_eq!(ada.post(&requests, request).await.0, StatusCode::ACCEPTED);
    let docs = settled_docs(&mut ada).await;
    assert_eq!(docs["pages"].as_array().unwrap().len(), 3);
    let proposal = &docs["proposals"][0];
    assert_eq!(proposal["request"], "Add a page on refunds");
    assert_eq!(
        (&proposal["page"], &proposal["section"], &proposal["title"]),
        (&Value::Null, &json!("works"), &json!("Refunds"))
    );

    let accept = ada.project(&format!(
        "/docs/proposals/{}",
        proposal["id"].as_str().unwrap()
    ));
    ada.post(&accept, json!({})).await;
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(docs["pages"][3]["title"], "Refunds");
    assert_eq!(docs["pages"][3]["markdown"], "A new page.");
    assert_eq!(docs["pages"][3]["files"], json!(["src/app.ts"]));
    assert_eq!(docs["pages"][3]["commit"], "c1");

    // A request made with a page open is about that page.
    let buy = docs["pages"][1].clone();
    let request = json!({"text": "Say more", "branch": "main", "page": buy["id"]});
    assert_eq!(ada.post(&requests, request).await.0, StatusCode::ACCEPTED);
    let docs = settled_docs(&mut ada).await;
    assert_eq!(docs["state"], "idle");
    let proposal = &docs["proposals"][0];
    assert_eq!(
        (&proposal["page"], &proposal["section"], &proposal["title"]),
        (&buy["id"], &json!("works"), &json!("Buy"))
    );
}

#[tokio::test]
async fn a_person_s_edit_is_checked_against_the_code() {
    let (state, mut ada) = project_with_docs().await;
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(docs["pages"][1]["notes"], Value::Null);
    let buy = &docs["pages"][1];
    let page = ada.project(&format!("/docs/pages/{}", buy["id"].as_str().unwrap()));
    let edit = json!({"title": "Buy", "markdown": "By hand.", "version": buy["version"]});
    ada.put(&page, edit).await;
    let docs = settled_docs(&mut ada).await;
    let note = json!({
        "quote": "By hand.", "says": "It is done by the code.",
        "place": "src/app.ts:1", "instead": "By the code.",
    });
    assert_eq!(docs["pages"][1]["notes"], json!([note]));

    let mut eve = Client::new(&state);
    eve.sign_in("eve@example.com", Some("Eve")).await;
    assert_eq!(
        eve.delete(&format!("{page}/notes/0")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        ada.delete(&format!("{page}/notes/0")).await,
        StatusCode::NO_CONTENT
    );
    let (_, docs) = ada.get(&ada.project("/docs")).await;
    assert_eq!(docs["pages"][1]["notes"], json!([]));

    // Words that look like the line a run once put under a page are part of the text, and shared whole.
    let text = "By hand.\n\nWritten from: memory.\n\nAnd more.";
    let edit = json!({"title": "Buy", "markdown": text, "version": docs["pages"][1]["version"]});
    ada.put(&page, edit).await;
    let (_, share) = ada.post(&ada.project("/shares"), Value::Null).await;
    let public = format!("/api/shares/{}", share["token"].as_str().unwrap());
    assert_eq!(ada.get(&public).await.1["pages"][0]["markdown"], text);

    // A page a person made is checked too, against the files the writer says it is about.
    let new = json!({"section": "works", "title": "Returns"});
    let (_, added) = ada.post(&ada.project("/docs/pages"), new).await;
    let page = ada.project(&format!("/docs/pages/{}", added["id"].as_str().unwrap()));
    let docs = settled_docs(&mut ada).await;
    let edit =
        json!({"title": "Returns", "markdown": "By hand.", "version": docs["pages"][3]["version"]});
    ada.put(&page, edit).await;
    let docs = settled_docs(&mut ada).await;
    assert_eq!(docs["pages"][3]["notes"], json!([note]));
}

#[tokio::test]
async fn a_page_written_before_any_run_is_checked_against_a_mapped_branch() {
    let (_, mut ada, _) = mapped_project(&[("src/app.ts", APP_TS)]).await;
    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "good-key"});
    ada.put(&ada.team("/writer"), writer).await;
    let new = json!({"section": "works", "title": "Returns"});
    let (_, added) = ada.post(&ada.project("/docs/pages"), new).await;
    let page = ada.project(&format!("/docs/pages/{}", added["id"].as_str().unwrap()));
    let docs = settled_docs(&mut ada).await;
    let edit =
        json!({"title": "Returns", "markdown": "By hand.", "version": docs["pages"][0]["version"]});
    ada.put(&page, edit).await;
    let docs = settled_docs(&mut ada).await;
    assert_eq!(docs["pages"][0]["notes"][0]["quote"], "By hand.");
    assert_eq!(docs["pages"][0]["files"], json!(["src/app.ts"]));
}

#[tokio::test]
async fn a_plan_that_can_t_be_read_fails_the_run() {
    let (_, mut ada, _) = mapped_project(&[("src/price.ts", PRICE_TS)]).await;
    ada.state.anthropic_api = fake_anthropic().await.0;
    let writer = json!({"kind": "claude_api", "key": "odd-key"});
    ada.put(&ada.team("/writer"), writer).await;
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["state"], "failed");
    let message = docs["message"].as_str().unwrap();
    assert!(message.starts_with("The writer's plan couldn't be read"));
}

#[tokio::test]
async fn the_team_owner_chooses_the_docs_writer() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;
    let with_key = json!({"kind": "claude_api", "key": "good-key"});

    assert_eq!(
        marco.put(&ada.team("/writer"), with_key.clone()).await,
        StatusCode::FORBIDDEN
    );
    // Marco owns team 2, but a program runs on the account of whoever runs the server.
    assert_eq!(
        marco
            .put(&marco.team("/writer"), json!({"kind": "codex"}))
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.put(&ada.team("/writer"), json!({"kind": "claude_api"}))
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        ada.put(&ada.team("/writer"), with_key).await,
        StatusCode::NO_CONTENT
    );
    // Saving again without a key keeps the saved one, and the key is never read back.
    let model = json!({"kind": "claude_api", "model": "claude-sonnet-5-5"});
    assert_eq!(
        ada.put(&ada.team("/writer"), model).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        ada.get(&ada.team("")).await.1["writer"],
        json!({"kind": "claude_api", "model": "claude-sonnet-5-5", "has_key": true})
    );

    assert_eq!(
        ada.put(&ada.team("/writer"), json!({"kind": "codex"}))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get(&ada.team("")).await.1["writer"]["has_key"], false);
    assert_eq!(
        ada.delete(&ada.team("/writer")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get(&ada.team("")).await.1["writer"], Value::Null);
}

#[tokio::test]
async fn docs_are_written_in_three_stages_from_the_project_s_own_files() {
    let files = [
        ("README.md", "# Storefront"),
        ("src/app.ts", APP_TS),
        ("src/price.ts", PRICE_TS),
        ("vendor/lib.ts", "export function vendored() {}\n"),
    ];
    let (_, mut ada, _) = mapped_project(&files).await;
    let (address, prompts) = fake_anthropic().await;
    ada.state.anthropic_api = address;
    let start = json!({"branch": "main"});

    // Nothing to write with, and nothing to write from.
    assert_eq!(
        ada.post(&ada.project("/docs"), start).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    ada.put(
        &ada.team("/writer"),
        json!({"kind": "claude_api", "key": "good-key"}),
    )
    .await;
    let other = json!({"branch": "other"});
    assert_eq!(
        ada.post(&ada.project("/docs"), other).await.0,
        StatusCode::NOT_FOUND
    );

    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["state"], "idle");
    assert_eq!(docs["branch"], "main");
    assert_eq!(docs["commit"], "c1");
    assert_eq!(docs["left_out"], json!(["vendor"]));
    let pages: Vec<(&str, &str)> = docs["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| {
            (
                page["section"].as_str().unwrap(),
                page["title"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pages,
        [
            ("overview", "Overview"),
            ("works", "Buy"),
            ("built", "Prices")
        ]
    );
    assert_eq!(docs["pages"][0]["markdown"], "The overview.");
    // What a page was written from is kept beside its text, not in it.
    let buy = &docs["pages"][1];
    assert_eq!(buy["markdown"], "A page, citing `src/price.ts:1`.");
    assert_eq!(
        (&buy["files"], &buy["unread"], &buy["commit"]),
        (&json!(["src/app.ts"]), &json!([]), &json!("c1"))
    );

    // The outline, one prompt per page with only that page's own files, then the overview.
    let asked = prompts.lock().unwrap().clone();
    assert_eq!(asked.len(), 4);
    assert!(asked[0].contains("src/price.ts (TypeScript, 4 lines)\n  function formatPrice :1"));
    assert!(asked[2].contains("=== src/price.ts ===\n1| export function formatPrice"));
    assert!(!asked[2].contains("=== src/app.ts ==="));
    assert!(!asked[2].contains("=== vendor/lib.ts ==="));
    assert!(asked[3].contains("=== Prices ==="));

    // Writing again replaces the pages; a failed run keeps them.
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["pages"].as_array().unwrap().len(), 3);
    ada.put(
        &ada.team("/writer"),
        json!({"kind": "claude_api", "key": "bad-key"}),
    )
    .await;
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["state"], "failed");
    assert_eq!(
        docs["message"],
        "Anthropic refused (401 Unauthorized): invalid x-api-key"
    );
    assert_eq!(docs["pages"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_program_on_the_server_can_write_the_docs() {
    let (_, mut ada, _) = mapped_project(&[("src/price.ts", PRICE_TS)]).await;
    let program = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_writer.sh");
    ada.state.claude_program = program.into();
    ada.state.codex_program = program.into();

    for kind in ["claude_code", "codex"] {
        ada.put(&ada.team("/writer"), json!({"kind": kind})).await;
        let docs = written_docs(&mut ada).await;
        assert_eq!(docs["state"], "idle", "{}", docs["message"]);
        assert_eq!(docs["pages"][1]["title"], "Prices");
        assert_eq!(docs["pages"][1]["markdown"], "Written by the program.");
    }

    // A program that fails says why.
    ada.state.codex_program = "false".into();
    let docs = written_docs(&mut ada).await;
    assert_eq!(docs["state"], "failed");
    assert_eq!(docs["message"], "false failed: ");
}
