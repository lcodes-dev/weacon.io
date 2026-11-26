use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use serde_json::{Value, json};

use crate::error::{ApiError, Result};
use crate::{AppState, db};

#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Dist;

pub async fn serve_web(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (path, file) = match Dist::get(path) {
        Some(file) => (path, file),
        None if path.starts_with("assets/") => {
            return ApiError::not_found().into_response();
        }
        None => match Dist::get("index.html") {
            Some(index) => ("index.html", index),
            None => return (StatusCode::NOT_FOUND, "the web app is not built").into_response(),
        },
    };
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    ([(header::CONTENT_TYPE, file.metadata.mimetype()), (header::CACHE_CONTROL, cache)], file.data)
        .into_response()
}

pub async fn health(State(state): State<AppState>) -> Result<Json<Value>> {
    db::ping(&state.pool).await?;
    Ok(Json(json!({"status": "ok"})))
}
