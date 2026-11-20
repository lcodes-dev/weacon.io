use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::db::{self, User};
use crate::mail::{self, Mail};
use crate::{ApiError, AppState, now};

const COOKIE_NAME: &str = "weacon_session";
const CODE_SECONDS: i64 = 10 * 60;
/// Counted from the session's last renewal.
const SESSION_SECONDS: i64 = 365 * 24 * 60 * 60;
const RENEW_SECONDS: i64 = 24 * 60 * 60;

impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        signed_in(state, &parts.headers)
            .await?
            .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first".into()))
    }
}

pub struct Owner(pub User);

impl FromRequestParts<AppState> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = User::from_request_parts(parts, state).await?;
        if user.is_owner {
            return Ok(Self(user));
        }
        let message = "Only the server's owner can do that";
        Err(ApiError(StatusCode::FORBIDDEN, message.into()))
    }
}

pub async fn signed_in(state: &AppState, headers: &HeaderMap) -> Result<Option<User>, ApiError> {
    match session_token(headers) {
        Some(token) => Ok(session_user(state, token).await?),
        None => Ok(None),
    }
}

async fn session_user(state: &AppState, token: &str) -> sqlx::Result<Option<User>> {
    db::user_by_session(&state.pool, token, now() - SESSION_SECONDS).await
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split("; ")
        .find_map(|cookie| cookie.strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

/// A `max_age` of 0 removes the cookie.
fn session_cookie(state: &AppState, token: &str, max_age: i64) -> [(HeaderName, String); 1] {
    let secure = if state.url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    [(SET_COOKIE, cookie)]
}

pub async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let setup_needed = !db::has_users(&state.pool).await?;
    let user = signed_in(&state, &headers).await?;
    // The web app asks this on every load, so a session in use is renewed here, once a day.
    let mut cookie = None;
    if let (Some(_), Some(token)) = (&user, session_token(&headers))
        && db::renew_session(&state.pool, token, now(), now() - RENEW_SECONDS).await?
    {
        cookie = Some(session_cookie(&state, token, SESSION_SECONDS));
    }
    let session = json!({"setup_needed": setup_needed, "user": user});
    Ok((cookie, Json(session)))
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
    if !mail::is_address(&email) {
        let message = "That isn't an email address";
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into()));
    }
    let name = request.name.as_deref().map(str::trim);
    let name = name.filter(|name| !name.is_empty());
    // An unknown email gets the same answer, and no mail.
    if name.is_none() && !db::user_exists(&state.pool, &email).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let code = format!("{:06}", rand::random_range(0..1_000_000));
    // Asked again within a minute, the first code stands and no second mail goes out.
    if !db::save_code(&state.pool, &email, &code, name, now(), CODE_SECONDS).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let mail = Mail {
        subject: "Your Weacon sign-in code",
        text: "Enter this code to sign in. It works for 10 minutes.",
        code: Some(&code),
        button: None,
    };
    if let Err(error) = mail::send(&state.pool, &email, &mail).await {
        tracing::error!("mail to {email} failed: {error}");
        db::forget_code_wait(&state.pool, &email).await?;
        let message = "The mail could not be sent";
        return Err(ApiError(StatusCode::BAD_GATEWAY, message.into()));
    }
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
        let message = "That code is wrong, has expired or was tried too often";
        return Err(ApiError(StatusCode::UNAUTHORIZED, message.into()));
    }
    let user = session_user(&state, &token).await?;
    Ok((session_cookie(&state, &token, SESSION_SECONDS), Json(user)))
}

pub async fn sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(token) = session_token(&headers) {
        db::sign_out(&state.pool, token).await?;
    }
    Ok((session_cookie(&state, "", 0), StatusCode::NO_CONTENT))
}

pub fn normalized(email: &str) -> String {
    email.trim().to_lowercase()
}
