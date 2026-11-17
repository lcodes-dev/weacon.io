use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Dist;

/// Serves the built web app: the file at the path, or `index.html` so client-side routes load.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (path, file) = match Dist::get(path) {
        Some(file) => (path, file),
        None => match Dist::get("index.html") {
            Some(index) => ("index.html", index),
            None => return (StatusCode::NOT_FOUND, "the web app is not built").into_response(),
        },
    };
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype()),
            (header::CACHE_CONTROL, cache),
        ],
        file.data,
    )
        .into_response()
}
