use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::auth::{
    SESSION_SECONDS, normalized, random_token, session_cookie, session_token, session_user,
    signed_in,
};
use crate::error::{ApiError, Result};
use crate::mail::{self, Mail};
use crate::{AppState, db, now};

const CODE_SECONDS: i64 = 10 * 60;
const RENEW_SECONDS: i64 = 24 * 60 * 60;

/// The web app asks this on every load, so a session in use is renewed here, once a day.
pub async fn show(State(state): State<AppState>, headers: HeaderMap) -> Result<impl IntoResponse> {
    let setup_needed = !db::users::any_exist(&state.pool).await?;
    let user = signed_in(&state, &headers).await?;
    let mut cookie = None;
    if let (Some(_), Some(token)) = (&user, session_token(&headers))
        && db::users::renew_session(&state.pool, token, now(), now() - RENEW_SECONDS).await?
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

pub async fn send_code(
    State(state): State<AppState>,
    Json(request): Json<CodeRequest>,
) -> Result<StatusCode> {
    let email = normalized(&request.email);
    if !mail::is_address(&email) {
        return Err(ApiError::invalid("That isn't an email address"));
    }
    let name = request.name.as_deref().map(str::trim);
    let name = name.filter(|name| !name.is_empty());
    // An unknown email gets the same answer, and no mail.
    if name.is_none() && !db::users::exists(&state.pool, &email).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let code = format!("{:06}", rand::random_range(0..1_000_000));
    if !db::users::save_code(&state.pool, &email, &code, name, now(), CODE_SECONDS).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let mail = Mail {
        subject: "Your Weacon sign-in code",
        text: "Enter this code to sign in. It works for 10 minutes.",
        code: Some(&code),
        button: None,
    };
    if let Err(error) = mail::send(&state, &email, &mail).await {
        tracing::error!("mail to {email} failed: {error}");
        db::users::end_code_wait(&state.pool, &email).await?;
        return Err(ApiError::bad_gateway("The mail could not be sent"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SignIn {
    email: String,
    code: String,
}

pub async fn create(
    State(state): State<AppState>,
    Json(sign_in): Json<SignIn>,
) -> Result<impl IntoResponse> {
    let email = normalized(&sign_in.email);
    let token = random_token();
    if !db::users::sign_in(&state.pool, &email, sign_in.code.trim(), &token, now()).await? {
        let message = "That code is wrong, has expired or was tried too often";
        return Err(ApiError::unauthorized(message));
    }
    let user = session_user(&state, &token).await?;
    Ok((session_cookie(&state, &token, SESSION_SECONDS), Json(user)))
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    if let Some(token) = session_token(&headers) {
        db::users::sign_out(&state.pool, token).await?;
    }
    Ok((session_cookie(&state, "", 0), StatusCode::NO_CONTENT))
}
