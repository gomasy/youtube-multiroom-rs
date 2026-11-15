//! The state of the audio cache: what it holds, what it holds for nothing, and
//! what the library expects of it that is no longer there.

use super::{AppResult, client_locale, job_started};
use crate::state::{AppState, CacheReport};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use rust_i18n::t;
use serde_json::{Value, json};
use std::sync::Arc;

/// GET /api/cache
pub async fn cache_status(State(state): State<Arc<AppState>>) -> Json<CacheReport> {
    Json(state.cache_report().await)
}

/// POST /api/cache/cleanup
///
/// Delete the cache files no registered track claims.
pub async fn cleanup_cache(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let (removed, freed) = state.remove_orphans().await;
    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "removed": removed,
        "freed_bytes": freed,
        "message": t!("api_cache_cleaned", locale = &locale, count = removed),
    })))
}

/// POST /api/cache/repair
///
/// Start re-downloading every registered track whose cache file is gone, and
/// say how many.
pub async fn repair_cache(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AppResult<Json<Value>> {
    let track_ids: Vec<String> = state
        .cache_report()
        .await
        .missing
        .into_iter()
        .map(|t| t.id)
        .collect();
    let total = track_ids.len();
    if total > 0 {
        state
            .start_track_recovery(track_ids, Default::default())
            .await;
    }
    let locale = client_locale(&headers);
    Ok(job_started(
        total,
        &t!("api_cache_repair_started", locale = &locale, count = total),
    ))
}
