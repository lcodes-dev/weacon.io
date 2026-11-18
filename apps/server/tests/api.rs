use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
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
}

async fn server() -> AppState {
    AppState {
        pool: db::memory().await,
        mailer: Arc::default(),
        url: "http://weacon.test".into(),
        github_api: "http://127.0.0.1:1".into(),
        map_wake: Arc::default(),
        anthropic_api: "http://127.0.0.1:1".into(),
        claude_program: "false".into(),
        codex_program: "false".into(),
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
                    Json(json!({"tree": entries}))
                },
            ),
        )
        .route(
            "/repos/northwind/storefront/tarball/{commit}",
            get(
                async |State(repo): State<Repo>, Path(commit): Path<String>| {
                    let mut repo = repo.0.lock().unwrap();
                    repo.archives += 1;
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
        .post("/api/teams/1/projects", json!({"name": " Storefront "}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project, json!({"id": 1}));
    assert_eq!(
        marco
            .post("/api/teams/1/projects", json!({"name": " "}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let renamed = json!({"name": "Shop"});
    assert_eq!(
        marco.put("/api/teams/1/projects/1", renamed.clone()).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        marco.put("/api/teams/1/projects/9", renamed).await,
        StatusCode::NOT_FOUND
    );
    let (_, projects) = ada.get("/api/teams/1/projects").await;
    assert_eq!(
        projects,
        json!([{"id": 1, "name": "Shop", "repo": null, "branches": []}])
    );

    assert_eq!(
        marco.delete("/api/teams/1/projects/1").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.delete("/api/teams/1/projects/1").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get("/api/teams/1/projects").await.1, json!([]));

    // Marco's own team isn't Ada's to see.
    assert_eq!(
        ada.get("/api/teams/2/projects").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_team_owner_registers_a_github_app_and_projects_pick_its_repositories() {
    let mut state = server().await;
    state.github_api = fake_github().await.0;
    let (mut ada, mut marco) = ada_and_marco(&state).await;

    let nothing = json!({"app": null, "account": null, "repos": []});
    assert_eq!(marco.get("/api/teams/1/github").await.1, nothing);
    let with_repo = json!({"name": "Storefront", "repo": "northwind/storefront"});
    assert_eq!(
        marco
            .post("/api/teams/1/projects", with_repo.clone())
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let code = json!({"code": "good-code"});
    assert_eq!(
        marco.post("/api/teams/1/github", code.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.post("/api/teams/1/github", json!({"code": "bad-code"}))
            .await
            .0,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        ada.post("/api/teams/1/github", code).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        marco.get("/api/teams/1/github").await.1,
        json!({
            "app": "weacon-northwind", "account": "northwind",
            "repos": ["northwind/storefront", "northwind/docs"],
        })
    );

    let (status, _) = marco.post("/api/teams/1/projects", with_repo).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ada.get("/api/teams/1/projects").await.1[0]["repo"],
        "northwind/storefront"
    );
    let elsewhere = json!({"name": "Storefront", "repo": "someone/else"});
    assert_eq!(
        marco.put("/api/teams/1/projects/1", elsewhere).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let without = json!({"name": "Storefront"});
    assert_eq!(
        marco.put("/api/teams/1/projects/1", without).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        ada.get("/api/teams/1/projects").await.1[0]["repo"],
        Value::Null
    );
}

#[tokio::test]
async fn forgetting_the_github_app_keeps_the_projects() {
    let mut state = server().await;
    state.github_api = fake_github().await.0;
    let (mut ada, mut marco) = ada_and_marco(&state).await;
    ada.post("/api/teams/1/github", json!({"code": "good-code"}))
        .await;
    let with_repo = json!({"name": "Storefront", "repo": "northwind/docs"});
    ada.post("/api/teams/1/projects", with_repo).await;

    assert_eq!(
        marco.delete("/api/teams/1/github").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.delete("/api/teams/1/github").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get("/api/teams/1/github").await.1["app"], Value::Null);
    assert_eq!(
        ada.get("/api/teams/1/projects").await.1[0]["repo"],
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
    ada.post("/api/teams/1/github", json!({"code": "good-code"}))
        .await;
    repo.push("main", "c1", files);
    let project =
        json!({"name": "Storefront", "repo": "northwind/storefront", "branches": ["main"]});
    let (status, _) = ada.post("/api/teams/1/projects", project).await;
    assert_eq!(status, StatusCode::OK);
    map::round(&state).await;
    (state, ada, repo)
}

/// The names of the symbols the map has for a file.
fn symbols(map: &Value, path: &str) -> Vec<String> {
    let files = map["files"].as_array().unwrap();
    let file = files.iter().find(|file| file["path"] == path).unwrap();
    let symbols = file["symbols"].as_array().unwrap().iter();
    symbols
        .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_tracked_branch_is_mapped_with_symbols_and_uses() {
    let files = [
        ("src/app.ts", APP_TS),
        ("src/price.ts", PRICE_TS),
        ("tools/util.py", UTIL_PY),
        ("server/lib.rs", LIB_RS),
        ("README.md", "# Storefront\n\nA shop.\n"),
    ];
    let (_, mut ada, _) = mapped_project(&files).await;

    let (_, branches) = ada
        .get("/api/teams/1/github/branches?repo=northwind/storefront")
        .await;
    assert_eq!(branches, json!({"default": "main", "branches": ["main"]}));
    let (_, projects) = ada.get("/api/teams/1/projects").await;
    assert_eq!(
        projects[0]["branches"],
        json!([{"name": "main", "head_sha": "c1", "commit_sha": "c1", "error": null}])
    );

    let (status, map) = ada.get("/api/teams/1/projects/1/map?branch=main").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(map["commit"], "c1");
    assert_eq!(map["files"].as_array().unwrap().len(), 5);
    assert_eq!(symbols(&map, "src/price.ts"), ["formatPrice", "init"]);
    assert_eq!(symbols(&map, "src/app.ts"), ["Cart", "total"]);
    assert_eq!(symbols(&map, "tools/util.py"), ["init", "Basket"]);
    assert_eq!(symbols(&map, "server/lib.rs"), ["Order", "ship"]);
    assert_eq!(
        map["files"][0],
        json!({"path": "README.md", "language": null, "lines": 3, "symbols": []})
    );
    assert_eq!(
        map["files"][2]["symbols"][1],
        json!({"name": "total", "kind": "method", "line": 3, "end_line": 5})
    );
    // `init` has two definitions, so mentioning it links nothing.
    assert_eq!(map["uses"], json!([["src/app.ts", "src/price.ts"]]));

    assert_eq!(
        ada.get("/api/teams/1/projects/1/map?branch=other").await.0,
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
    let (_, map) = ada.get("/api/teams/1/projects/1/map?branch=main").await;
    assert_eq!(map["commit"], "c2");
    assert_eq!(symbols(&map, "app.ts"), ["Cart", "total"]);

    repo.push("main", "c3", &[("app.ts", "export function start() {}\n")]);
    map::round(&state).await;
    assert_eq!(repo.archives(), 2);
    let (_, map) = ada.get("/api/teams/1/projects/1/map?branch=main").await;
    assert_eq!(map["files"].as_array().unwrap().len(), 1);
    assert_eq!(symbols(&map, "app.ts"), ["start"]);
    // The contents no commit lists any more are forgotten.
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
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
    ada.put("/api/teams/1/projects/1", project.clone()).await;
    map::round(&state).await;

    let (_, main) = ada.get("/api/teams/1/projects/1/map?branch=main").await;
    let (_, dev) = ada.get("/api/teams/1/projects/1/map?branch=dev").await;
    assert_eq!(main["files"].as_array().unwrap().len(), 2);
    assert_eq!(dev["files"].as_array().unwrap().len(), 2);
    // The same content at two paths is read once.
    assert_eq!(symbols(&main, "old/price.ts"), ["formatPrice", "init"]);
    assert_eq!(dev["commit"], "d1");
    let (_, projects) = ada.get("/api/teams/1/projects").await;
    assert_eq!(projects[0]["branches"][1]["name"], "gone");
    assert_eq!(
        projects[0]["branches"][1]["error"],
        "This branch isn't on GitHub"
    );

    project["branches"] = json!(["main"]);
    ada.put("/api/teams/1/projects/1", project.clone()).await;
    assert_eq!(
        ada.get("/api/teams/1/projects/1/map?branch=dev").await.0,
        StatusCode::NOT_FOUND
    );
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);

    // Without a repository there is nothing to map.
    project["repo"] = Value::Null;
    ada.put("/api/teams/1/projects/1", project).await;
    assert_eq!(
        ada.get("/api/teams/1/projects").await.1[0]["branches"],
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
                    if headers["x-api-key"] != "good-key" {
                        let error = json!({"error": {"message": "invalid x-api-key"}});
                        return (StatusCode::UNAUTHORIZED, Json(error));
                    }
                    assert_eq!(body["model"], "claude-haiku-5-5");
                    let system = body["system"].as_str().unwrap();
                    let prompt = body["messages"][0]["content"].as_str().unwrap();
                    prompts.lock().unwrap().push(prompt.to_owned());
                    let text = if system.contains("Answer with JSON") {
                        json!({
                            "left_out": ["vendor"],
                            "works": [{"title": "Buy", "summary": "Buying.", "files": ["src/app.ts"]}],
                            "built": [{
                                "title": "Prices", "summary": "Formatting prices.",
                                "files": ["src/price.ts", "vendor/lib.ts", "no/such.ts"],
                            }],
                        })
                        .to_string()
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
        .post("/api/teams/1/projects/1/docs", json!({"branch": "main"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    loop {
        let (_, docs) = client.get("/api/teams/1/projects/1/docs").await;
        if docs["state"] != "running" {
            return docs;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_team_owner_chooses_the_docs_writer() {
    let state = server().await;
    let (mut ada, mut marco) = ada_and_marco(&state).await;
    let with_key = json!({"kind": "claude_api", "key": "good-key"});

    assert_eq!(
        marco.put("/api/teams/1/writer", with_key.clone()).await,
        StatusCode::FORBIDDEN
    );
    // Marco owns team 2, but a program runs on the account of whoever runs the server.
    assert_eq!(
        marco
            .put("/api/teams/2/writer", json!({"kind": "codex"}))
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ada.put("/api/teams/1/writer", json!({"kind": "claude_api"}))
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        ada.put("/api/teams/1/writer", with_key).await,
        StatusCode::NO_CONTENT
    );
    // Saving again without a key keeps the saved one, and the key is never read back.
    let model = json!({"kind": "claude_api", "model": "claude-sonnet-5-5"});
    assert_eq!(
        ada.put("/api/teams/1/writer", model).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        ada.get("/api/teams/1").await.1["writer"],
        json!({"kind": "claude_api", "model": "claude-sonnet-5-5", "has_key": true})
    );

    assert_eq!(
        ada.put("/api/teams/1/writer", json!({"kind": "codex"}))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get("/api/teams/1").await.1["writer"]["has_key"], false);
    assert_eq!(
        ada.delete("/api/teams/1/writer").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(ada.get("/api/teams/1").await.1["writer"], Value::Null);
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
        ada.post("/api/teams/1/projects/1/docs", start).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    ada.put(
        "/api/teams/1/writer",
        json!({"kind": "claude_api", "key": "good-key"}),
    )
    .await;
    let other = json!({"branch": "other"});
    assert_eq!(
        ada.post("/api/teams/1/projects/1/docs", other).await.0,
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
    assert_eq!(
        docs["pages"][1]["markdown"],
        "A page, citing `src/price.ts:1`.\n\nWritten from: src/app.ts"
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
        "/api/teams/1/writer",
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
        ada.put("/api/teams/1/writer", json!({"kind": kind})).await;
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
