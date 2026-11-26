use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::mail::{Mail, Smtp};
use super::writer::{Kind, Writer};
use super::{db, mail};
use crate::AppState;
use crate::accounts::db::User;
use crate::auth::ServerOwner;
use crate::error::{ApiError, Result};

pub async fn show_mail(State(state): State<AppState>, _: ServerOwner) -> Result<Json<Value>> {
    let smtp = db::smtp(&state.pool, &state.server_key).await?;
    Ok(Json(json!({"smtp": smtp})))
}

pub async fn update_mail(
    State(state): State<AppState>,
    _: ServerOwner,
    Json(mut smtp): Json<Smtp>,
) -> Result<StatusCode> {
    if smtp.password.is_none() {
        let saved = db::smtp(&state.pool, &state.server_key).await?;
        smtp.password = saved.and_then(|saved| saved.password);
    }
    mail::check(&smtp).map_err(|error| ApiError::invalid(error.to_string()))?;
    db::save_smtp(&state.pool, &state.server_key, Some(&smtp)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_mail(State(state): State<AppState>, _: ServerOwner) -> Result<StatusCode> {
    db::save_smtp(&state.pool, &state.server_key, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_mail(State(state): State<AppState>, ServerOwner(user): ServerOwner) -> Result<StatusCode> {
    let mail = Mail {
        subject: "Weacon test mail",
        text: "This is a test mail from Weacon.\n\nYour mail settings work.",
        code: None,
        button: None,
    };
    mail::send(&state, &user.email, &mail).await.map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Key {
    key: String,
}

pub async fn update_api_key(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(Key { key }): Json<Key>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let key = key.trim();
    if key.is_empty() {
        return Err(ApiError::invalid("An API key is needed"));
    }
    db::save_api_key(&state.pool, &state.server_key, &team_id, Some(key)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let writer = db::writer(&state.pool, &state.server_key, &team_id).await?;
    if writer.is_some_and(|writer| writer.kind == Kind::ClaudeApi) {
        let message = "The team's docs are written with this key. Choose another writer first";
        return Err(ApiError::invalid(message));
    }
    db::save_api_key(&state.pool, &state.server_key, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_writer(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(writer): Json<Writer>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let key = db::api_key(&state.pool, &state.server_key, &team_id).await?;
    if writer.kind == Kind::ClaudeApi && key.is_none() {
        return Err(ApiError::invalid("Save the team's API key first"));
    }
    db::save_writer(&state.pool, &team_id, Some(&writer)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_writer(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    db::save_writer(&state.pool, &team_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
