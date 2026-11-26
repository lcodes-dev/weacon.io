use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::error::Result;
use crate::{AppState, db};

pub async fn show(State(state): State<AppState>) -> Result<Json<Value>> {
    db::ping(&state.pool).await?;
    Ok(Json(json!({"status": "ok"})))
}
