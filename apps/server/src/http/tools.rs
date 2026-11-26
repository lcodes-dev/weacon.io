use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::bearer;
use crate::{AppState, db, tools};

pub async fn call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let caller = match bearer(&headers) {
        Some(token) => db::chats::caller(&state.pool, token).await.ok().flatten(),
        None => None,
    };
    let Some(caller) = caller else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (id, params) = (&request["id"], &request["params"]);
    if id.is_null() {
        return StatusCode::ACCEPTED.into_response();
    }
    let mut answer = match request["method"].as_str() {
        Some("initialize") => json!({"result": {
            "protocolVersion": params["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "weacon", "version": env!("CARGO_PKG_VERSION")},
        }}),
        Some("tools/list") => json!({"result": {"tools": tools::list()}}),
        Some("tools/call") => {
            let name = params["name"].as_str().unwrap_or_default();
            let done = tools::call(&state, &caller, name, &params["arguments"]).await;
            let failed = done.is_err();
            let text = done.unwrap_or_else(|error| error.message);
            json!({"result": {"content": [{"type": "text", "text": text}], "isError": failed}})
        }
        _ => json!({"error": {"code": -32601, "message": "Method not found"}}),
    };
    answer["jsonrpc"] = "2.0".into();
    answer["id"] = id.clone();
    Json(answer).into_response()
}
