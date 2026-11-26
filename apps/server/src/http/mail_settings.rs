use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::auth::ServerOwner;
use crate::error::{ApiError, Result};
use crate::mail::{self, Mail, Smtp};
use crate::{AppState, db};

pub async fn show(State(state): State<AppState>, _: ServerOwner) -> Result<Json<Value>> {
    let smtp = db::settings::smtp(&state.pool, &state.server_key).await?;
    Ok(Json(json!({"smtp": smtp})))
}

/// A missing password keeps the saved one.
pub async fn update(
    State(state): State<AppState>,
    _: ServerOwner,
    Json(mut smtp): Json<Smtp>,
) -> Result<StatusCode> {
    if smtp.password.is_none() {
        let saved = db::settings::smtp(&state.pool, &state.server_key).await?;
        smtp.password = saved.and_then(|saved| saved.password);
    }
    mail::check(&smtp).map_err(|error| ApiError::invalid(error.to_string()))?;
    db::settings::save_smtp(&state.pool, &state.server_key, Some(&smtp)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, _: ServerOwner) -> Result<StatusCode> {
    db::settings::save_smtp(&state.pool, &state.server_key, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test(
    State(state): State<AppState>,
    ServerOwner(user): ServerOwner,
) -> Result<StatusCode> {
    let mail = Mail {
        subject: "Weacon test mail",
        text: "This is a test mail from Weacon.\n\nYour mail settings work.",
        code: None,
        button: None,
    };
    mail::send(&state, &user.email, &mail)
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
