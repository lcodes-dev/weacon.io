mod db;
mod web;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::SqlitePool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = env_or("WEACON_ADDR", "127.0.0.1:8080");
    let pool = db::open(&env_or("WEACON_DB", "weacon.db")).await?;
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, router(pool)).await?;
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn router(pool: SqlitePool) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .fallback(api_not_found)
        .with_state(pool);
    Router::new().nest("/api", api).fallback(web::serve)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health(State(pool): State<SqlitePool>) -> Result<Json<Health>, StatusCode> {
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(Health { status: "ok" }))
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
}

async fn api_not_found() -> (StatusCode, Json<ApiError>) {
    (StatusCode::NOT_FOUND, Json(ApiError { error: "not found" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get(path: &str) -> (StatusCode, String) {
        let pool = db::open(":memory:").await.unwrap();
        let request = Request::get(path).body(Body::empty()).unwrap();
        let response = router(pool).oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn health_answers_ok() {
        assert_eq!(get("/api/health").await, (StatusCode::OK, r#"{"status":"ok"}"#.to_owned()));
    }

    #[tokio::test]
    async fn unknown_api_path_is_a_json_404() {
        let expected = (StatusCode::NOT_FOUND, r#"{"error":"not found"}"#.to_owned());
        assert_eq!(get("/api/nope").await, expected);
    }

    // Needs the web app built: `npm run build` in web/.
    #[tokio::test]
    async fn unknown_path_serves_the_web_app() {
        let (status, body) = get("/chats/42").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#"<div id="root">"#));
    }
}
