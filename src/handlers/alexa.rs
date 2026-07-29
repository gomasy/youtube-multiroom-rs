//! The Alexa skill webhook. Verification lives in [`crate::alexa_verify`] and
//! the skill logic in [`crate::alexa`]; this is only the HTTP edge.

use super::{AppError, AppResult};
use crate::alexa::handle_alexa;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::Json;
use serde_json::Value;
use std::sync::Arc;

/// POST /alexa
///
/// Not behind Bearer auth — instead, Amazon's signature verification confirms
/// the request genuinely originates from Alexa. Returns 400 on verification
/// failure per Amazon's specification.
pub async fn alexa_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    if let Err(e) = crate::alexa_verify::verify_request(&headers, &body).await {
        tracing::warn!("Rejected Alexa request: {e}");
        return Err(AppError::bad_request("Request verification failed"));
    }

    let body: Value =
        serde_json::from_slice(&body).map_err(|_| AppError::bad_request("Invalid JSON body"))?;

    if let Err(e) = crate::alexa_verify::verify_timestamp(&body) {
        tracing::warn!("Rejected Alexa request: {e}");
        return Err(AppError::bad_request("Request verification failed"));
    }

    let req_type = body["request"]["type"].as_str().unwrap_or("unknown");
    tracing::info!("Alexa request: {}", req_type);

    let base_url = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|host| format!("https://{host}"))
        .ok_or_else(|| AppError::bad_request("Host header is required"))?;

    Ok(Json(handle_alexa(&state, body, &base_url).await))
}
