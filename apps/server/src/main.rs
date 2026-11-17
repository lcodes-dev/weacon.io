mod auth;
mod config;
mod db;
mod mail;
mod web;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use mail::Mailer;

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    mailer: Arc<Mailer>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = env_or("WEACON_ADDR", "127.0.0.1:8080");
    let pool = db::open(&env_or("WEACON_DB", "weacon.db")).await?;
    let smtp = db::smtp(&pool).await?;
    match &smtp {
        Some(smtp) => println!("mail goes through {}", smtp.host),
        None => println!("mail is printed to this log until the owner saves SMTP settings"),
    }
    let mailer = Arc::new(Mailer::default());
    mailer.set(smtp.as_ref())?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, router(AppState { pool, mailer })).await?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// Seconds since 1970.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/session", get(auth::session))
        .route("/sign-in/code", post(auth::request_code))
        .route("/sign-in/verify", post(auth::verify))
        .route("/sign-out", post(auth::sign_out))
        .route(
            "/config/mail",
            get(config::mail)
                .put(config::save_mail)
                .delete(config::clear_mail),
        )
        .route("/config/mail/test", post(config::test_mail))
        .fallback(async || ApiError(StatusCode::NOT_FOUND, "not found".into()))
        .with_state(state);
    Router::new().nest("/api", api).fallback(web::serve)
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok(Json(json!({"status": "ok"})))
}

/// An error the API answers with, as `{"error": "..."}`.
#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        eprintln!("database error: {error}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    /// One browser: it keeps the session cookie the server sets.
    struct Client {
        state: AppState,
        cookie: Option<String>,
    }

    async fn server() -> AppState {
        AppState {
            pool: db::memory().await,
            mailer: Arc::default(),
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

        /// Asks for a code, as a registration when there is a name, and confirms it.
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
        assert_eq!(user["team_name"], "Ada's team");

        let (_, session) = ada.get("/api/session").await;
        assert_eq!(session["setup_needed"], false);
        assert_eq!(session["user"]["email"], "ada@example.com");

        let mut marco = Client::new(&state);
        let (_, user) = marco.sign_in(" Marco@Example.com ", Some("Marco")).await;
        assert_eq!(user["email"], "marco@example.com");
        assert_eq!(user["is_owner"], false);
        assert_eq!(user["team_name"], "Marco's team");
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
        let pool = db::memory().await;
        db::save_code(&pool, "ada@example.com", "123456", Some("Ada"), 1000)
            .await
            .unwrap();
        assert!(
            !db::sign_in(&pool, "ada@example.com", "123456", "token", 1000)
                .await
                .unwrap()
        );
        db::save_code(&pool, "ada@example.com", "123456", Some("Ada"), 1000)
            .await
            .unwrap();
        assert!(
            db::sign_in(&pool, "ada@example.com", "123456", "token", 999)
                .await
                .unwrap()
        );
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
            "host": "smtp.example.com", "port": 587, "security": "starttls",
            "username": "ada", "password": "secret", "from": "Weacon <weacon@example.com>",
        });

        assert_eq!(
            owner.put("/api/config/mail", smtp.clone()).await,
            StatusCode::NO_CONTENT
        );
        let (_, saved) = owner.get("/api/config/mail").await;
        assert_eq!(saved["smtp"]["host"], "smtp.example.com");
        assert_eq!(saved["smtp"].get("password"), None);
        assert!(state.mailer.uses_smtp());

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
        assert!(!state.mailer.uses_smtp());
    }
}
