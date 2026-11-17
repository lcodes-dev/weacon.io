use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::db::{self, User};
use crate::mail::Smtp;
use crate::{ApiError, AppState};

fn owner(user: &User) -> Result<(), ApiError> {
    if user.is_owner {
        return Ok(());
    }
    let message = "Only the server's owner can do that";
    Err(ApiError(StatusCode::FORBIDDEN, message.into()))
}

fn bad_settings(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
}

/// The settings without the password, which `Smtp` never serializes.
pub async fn mail(State(state): State<AppState>, user: User) -> Result<Json<Value>, ApiError> {
    owner(&user)?;
    let smtp = db::smtp(&state.pool).await?;
    Ok(Json(json!({"smtp": smtp})))
}

/// A missing password keeps the saved one, since the settings are read back without it.
pub async fn save_mail(
    State(state): State<AppState>,
    user: User,
    Json(mut smtp): Json<Smtp>,
) -> Result<StatusCode, ApiError> {
    owner(&user)?;
    if smtp.password.is_none() {
        smtp.password = db::smtp(&state.pool)
            .await?
            .and_then(|saved| saved.password);
    }
    state.mailer.set(Some(&smtp)).map_err(bad_settings)?;
    db::save_smtp(&state.pool, Some(&smtp)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn clear_mail(State(state): State<AppState>, user: User) -> Result<StatusCode, ApiError> {
    owner(&user)?;
    db::save_smtp(&state.pool, None).await?;
    state.mailer.set(None).map_err(bad_settings)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_mail(State(state): State<AppState>, user: User) -> Result<StatusCode, ApiError> {
    owner(&user)?;
    let text = "This is a test mail from Weacon. Your mail settings work.";
    state
        .mailer
        .send(&user.email, "Weacon test mail", text)
        .await
        .map_err(|error| ApiError(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
