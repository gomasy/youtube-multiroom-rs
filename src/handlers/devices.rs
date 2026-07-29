//! Device state and playback control. Nothing here reaches an Echo directly —
//! the skill cannot push directives, so every command is parked in Redis and
//! applied the next time the device connects (see crate::alexa).

use super::{AppError, AppResult, client_locale, device_or_404, track_or_404};
use crate::state::{AppState, AudioTrack, DeviceUpdate, PlayRequest, SeekRequest};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;
use rust_i18n::t;
use serde_json::{Value, json};
use std::sync::Arc;

/// GET /api/devices
pub async fn get_devices(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.devices_json().await)
}

/// POST /api/play
pub async fn play_on_devices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &req.track_id).await?;
    let locale = client_locale(&headers);
    queue_on_devices(&state, track, req.device_ids, &locale).await
}

/// POST /api/play-all
pub async fn play_on_all(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &req.track_id).await?;
    let locale = client_locale(&headers);
    let device_ids = state
        .device_ids()
        .await
        .map_err(|e| AppError::internal(format!("Failed to list devices: {e}")))?;
    queue_on_devices(&state, track, device_ids, &locale).await
}

/// Queue a track for playback on each device's pending slot and broadcast state.
async fn queue_on_devices(
    state: &AppState,
    track: AudioTrack,
    device_ids: Vec<String>,
    locale: &str,
) -> AppResult<Json<Value>> {
    for did in &device_ids {
        state.queue_play(did, track.clone(), 0).await;
    }

    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": device_ids,
        "message": t!("api_play_queued", locale = locale),
    })))
}

/// POST /api/queue
///
/// Append a track to the "next up" queue of the selected devices. The front
/// of the queue is consumed on PlaybackNearlyFinished.
pub async fn queue_next(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let locale = client_locale(&headers);
    let track = track_or_404(&state, &req.track_id).await?;
    let mut queued = Vec::new();
    let mut write_error = None;
    for did in &req.device_ids {
        // Only queue on registered devices to avoid orphaned Redis keys
        if !state.device_exists(did).await {
            continue;
        }
        match state.push_queue(did, &track.id).await {
            Ok(()) => queued.push(did.clone()),
            Err(e) => write_error = Some(e),
        }
    }
    if queued.is_empty() {
        // A device that exists but could not be written to is our fault, not a
        // malformed request, so don't report it as one
        return Err(match write_error {
            Some(e) => AppError::internal(format!("Failed to queue track: {e}")),
            None => AppError::bad_request("No valid devices"),
        });
    }
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "ok",
        "devices": queued,
        "message": t!("api_queued_next", locale = &locale, title = &track.title),
    })))
}

/// DELETE /api/devices/:id/queue/:entry
///
/// Remove a single queue item by entry value match. Entries are unique, so
/// even if a device consumes it concurrently, a different track won't be
/// accidentally removed.
pub async fn remove_queue_item(
    State(state): State<Arc<AppState>>,
    Path((device_id, entry)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let removed = state.remove_queue_entry(&device_id, &entry).await;
    // Always broadcast latest state even on miss (client's view may be stale)
    state.broadcast_devices().await;
    if !removed {
        return Err(AppError::not_found("Queue item not found"));
    }
    Ok(Json(json!({ "status": "ok" })))
}

/// DELETE /api/devices/:id/queue
pub async fn clear_queue(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
) -> AppResult<Json<Value>> {
    // DEL cannot tell "no device" from "empty queue", so existence is checked
    // separately — but only for existence, not for the device's contents
    if !state.device_exists(&device_id).await {
        return Err(AppError::not_found("Device not found"));
    }
    state.clear_queue(&device_id).await;
    state.broadcast_devices().await;
    Ok(Json(json!({ "status": "ok" })))
}

/// POST /api/devices/:id/seek
///
/// Queue a seek command for a device's current track. Since the Alexa skill
/// cannot push directives to devices, the seek is applied when the Echo next
/// connects to the skill (via voice command or track transition).
pub async fn seek_device(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    Json(req): Json<SeekRequest>,
) -> AppResult<Json<Value>> {
    let dev = device_or_404(&state, &device_id).await?;
    let track = dev
        .current_track
        .ok_or_else(|| AppError::bad_request("Device has no track to seek"))?;
    if track.is_live {
        return Err(AppError::bad_request("Cannot seek a live stream"));
    }
    // Unknown duration means we can't clamp properly; reject rather than
    // silently seeking to the start
    if track.duration == 0 {
        return Err(AppError::bad_request("Track duration is unknown"));
    }

    // Clamp to 1 second before the end to avoid immediate playback termination
    let max_ms = track.duration.saturating_mul(1000).saturating_sub(1000);
    let position_ms = req.position_ms.min(max_ms);
    state.queue_play(&device_id, track, position_ms).await;
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "position_ms": position_ms,
    })))
}

/// POST /api/devices/:id/stop
pub async fn stop_device(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
) -> AppResult<Json<Value>> {
    // update_device already reads the device, so let it report the 404 rather
    // than paying for a second lookup
    if !state
        .update_device(&device_id, DeviceUpdate::new().status("stopped"))
        .await
    {
        return Err(AppError::not_found("Device not found"));
    }
    state.broadcast_devices().await;
    Ok(Json(json!({ "status": "ok" })))
}

/// DELETE /api/devices/:id
pub async fn delete_device(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .remove_device(&device_id)
        .await
        .ok_or_else(|| AppError::not_found("Device not found"))?;
    state.broadcast_devices().await;
    Ok(Json(json!({ "status": "ok" })))
}
