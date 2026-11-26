use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::auth::Owner;
use crate::db;
use crate::mail::{self, Mail, Smtp};
use crate::{ApiError, AppState};

fn bad_settings(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
}

pub async fn mail(State(state): State<AppState>, _: Owner) -> Result<Json<Value>, ApiError> {
    let smtp = db::smtp(&state.pool, &state.server_key).await?;
    Ok(Json(json!({"smtp": smtp})))
}

/// A missing password keeps the saved one.
pub async fn save_mail(
    State(state): State<AppState>,
    _: Owner,
    Json(mut smtp): Json<Smtp>,
) -> Result<StatusCode, ApiError> {
    if smtp.password.is_none() {
        smtp.password = db::smtp(&state.pool, &state.server_key)
            .await?
            .and_then(|saved| saved.password);
    }
    mail::check(&smtp).map_err(bad_settings)?;
    db::save_smtp(&state.pool, &state.server_key, Some(&smtp)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn clear_mail(State(state): State<AppState>, _: Owner) -> Result<StatusCode, ApiError> {
    db::save_smtp(&state.pool, &state.server_key, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_mail(
    State(state): State<AppState>,
    Owner(user): Owner,
) -> Result<StatusCode, ApiError> {
    let mail = Mail {
        subject: "Weacon test mail",
        text: "This is a test mail from Weacon.\n\nYour mail settings work.",
        code: None,
        button: None,
    };
    mail::send(&state.pool, &state.server_key, &user.email, &mail)
        .await
        .map_err(|error| ApiError(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
