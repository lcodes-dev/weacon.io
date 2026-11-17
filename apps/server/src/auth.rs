use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, User};
use crate::{ApiError, AppState, now};

const COOKIE_NAME: &str = "weacon_session";
const CODE_SECONDS: i64 = 10 * 60;

/// The signed-in person; a route that takes one answers 401 to anyone else.
impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        signed_in(state, &parts.headers)
            .await?
            .ok_or(ApiError(StatusCode::UNAUTHORIZED, "Sign in first".into()))
    }
}

async fn signed_in(state: &AppState, headers: &HeaderMap) -> Result<Option<User>, ApiError> {
    match session_token(headers) {
        Some(token) => Ok(db::user_by_session(&state.pool, token).await?),
        None => Ok(None),
    }
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split("; ")
        .find_map(|cookie| cookie.strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

/// The header that sets the session cookie; a `max_age` of 0 removes it.
fn session_cookie(token: &str, max_age: u32) -> [(HeaderName, String); 1] {
    let cookie =
        format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    [(SET_COOKIE, cookie)]
}

pub async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let setup_needed = !db::has_users(&state.pool).await?;
    let user = signed_in(&state, &headers).await?;
    Ok(Json(json!({"setup_needed": setup_needed, "user": user})))
}

#[derive(Deserialize)]
pub struct CodeRequest {
    email: String,
    /// Present when registering.
    name: Option<String>,
}

pub async fn request_code(
    State(state): State<AppState>,
    Json(request): Json<CodeRequest>,
) -> Result<StatusCode, ApiError> {
    let email = normalized(&request.email);
    if !email.contains('@') {
        let message = "That isn't an email address";
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into()));
    }
    let name = request.name.as_deref().map(str::trim);
    let name = name.filter(|name| !name.is_empty());
    // Signing in with an unknown email looks the same from outside, and mails nothing.
    if name.is_none() && !db::user_exists(&state.pool, &email).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let code = format!("{:06}", rand::random_range(0..1_000_000));
    db::save_code(&state.pool, &email, &code, name, now() + CODE_SECONDS).await?;
    let text = format!("Your Weacon sign-in code is {code}\n\nIt works for 10 minutes.");
    state
        .mailer
        .send(&email, "Your Weacon sign-in code", &text)
        .await
        .map_err(|error| {
            eprintln!("mail failed: {error}");
            ApiError(StatusCode::BAD_GATEWAY, "The mail could not be sent".into())
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Verify {
    email: String,
    code: String,
}

pub async fn verify(
    State(state): State<AppState>,
    Json(verify): Json<Verify>,
) -> Result<impl IntoResponse, ApiError> {
    let email = normalized(&verify.email);
    let code = verify.code.trim();
    let token = format!("{:032x}", rand::random::<u128>());
    if !db::sign_in(&state.pool, &email, code, &token, now()).await? {
        let message = "That code is wrong or has expired";
        return Err(ApiError(StatusCode::UNAUTHORIZED, message.into()));
    }
    let user = db::user_by_session(&state.pool, &token).await?;
    Ok((session_cookie(&token, 365 * 24 * 60 * 60), Json(user)))
}

pub async fn sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(token) = session_token(&headers) {
        db::sign_out(&state.pool, token).await?;
    }
    Ok((session_cookie("", 0), StatusCode::NO_CONTENT))
}

fn normalized(email: &str) -> String {
    email.trim().to_lowercase()
}
