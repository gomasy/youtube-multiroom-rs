//! Device state and playback control. Nothing here reaches an Echo directly —
//! the skill cannot push directives, so every command is parked in Redis and
//! applied the next time the device connects (see crate::alexa).

use super::{AppError, AppResult, client_locale, device_or_404, track_or_404, written_or_err};
use crate::state::{
    AppState, AudioTrack, DeviceState, DeviceUpdate, PlayRequest, SeekRequest, SyncRequest,
    WriteOutcome,
};
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
    let device_ids = all_device_ids(&state).await?;
    queue_on_devices(&state, track, device_ids, &locale).await
}

/// Every registered device. Failing to read them is ours to report, not a
/// malformed request — the caller asked for "all of them" and we cannot say
/// which those are.
async fn all_device_ids(state: &AppState) -> AppResult<Vec<String>> {
    state
        .device_ids()
        .await
        .map_err(|e| AppError::internal(format!("Failed to list devices: {e}")))
}

/// Apply a per-device write to every target, returning the devices it reached.
///
/// A named device that is no longer registered is skipped rather than failing
/// the request: the client's list is a snapshot, and one stale entry must not
/// cost the user the devices still there. Reaching none at all is the error,
/// and which error depends on why — a device that exists but could not be
/// written to is our fault, not a malformed request. `failed` carries no Redis
/// detail; the write already logged it.
///
/// The ID is handed over owned for the reason `VideoJob::run` documents: a
/// borrowing `write` makes the returned future higher-ranked, which stops axum
/// from proving the handler `Send`.
async fn reach_devices<F, Fut>(
    device_ids: Vec<String>,
    failed: &'static str,
    write: F,
) -> AppResult<Vec<String>>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = WriteOutcome>,
{
    let mut reached = Vec::new();
    let mut write_failed = false;
    for did in device_ids {
        match write(did.clone()).await {
            WriteOutcome::Written => reached.push(did),
            WriteOutcome::Gone => {}
            WriteOutcome::Failed => write_failed = true,
        }
    }
    if reached.is_empty() {
        return Err(if write_failed {
            AppError::internal(failed)
        } else {
            AppError::bad_request("No valid devices")
        });
    }
    Ok(reached)
}

/// Queue a track for playback on each device's pending slot and broadcast state.
async fn queue_on_devices(
    state: &AppState,
    track: AudioTrack,
    device_ids: Vec<String>,
    locale: &str,
) -> AppResult<Json<Value>> {
    let queued = reach_devices(device_ids, "Failed to queue playback", |did| {
        let track = track.clone();
        async move { state.queue_play(&did, track, 0).await }
    })
    .await?;

    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": queued,
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
    // Lent rather than moved: the closure is called once per device, and both
    // are needed again below.
    let (state, track) = (&state, &track);
    let queued = reach_devices(req.device_ids, "Failed to queue track", |did| async move {
        state.push_queue(&did, &track.id).await
    })
    .await?;
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "ok",
        "devices": queued,
        "message": t!("api_queued_next", locale = &locale, title = &track.title),
    })))
}

/// DELETE /api/devices/:id/queue/:entry
///
/// Remove one queue item by value match. Entries are unique, so a device
/// consuming one concurrently cannot cause the wrong track to be removed.
pub async fn remove_queue_item(
    State(state): State<Arc<AppState>>,
    Path((device_id, entry)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let removed = state.remove_queue_entry(&device_id, &entry).await;
    // Broadcast even on a miss: the client's view may be the stale one
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
    // separately.
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
    // Nothing to clamp against; rejected rather than silently seeking to 0
    let max_ms = track
        .max_offset_ms()
        .ok_or_else(|| AppError::bad_request("Track duration is unknown"))?;
    let position_ms = req.position_ms.min(max_ms);
    written_or_err(
        state.queue_play(&device_id, track, position_ms).await,
        "Device not found",
        "Failed to queue seek",
    )?;
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "position_ms": position_ms,
    })))
}

/// POST /api/devices/:id/sync
///
/// Line the other devices up with this one: queue its current track on each,
/// starting from where it is now. Like a seek, each device applies it the next
/// time it contacts the skill, so this is what brings a room that joined late
/// into the same part of the same track rather than back to its beginning.
pub async fn sync_devices(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<SyncRequest>,
) -> AppResult<Json<Value>> {
    let leader = device_or_404(&state, &device_id).await?;
    let track = leader
        .current_track
        .clone()
        .ok_or_else(|| AppError::bad_request("Device has no track to sync"))?;
    let position_ms = sync_offset(&leader, &track);

    let followers = match req.device_ids {
        Some(ids) => ids,
        None => all_device_ids(&state).await?,
    };
    // The leader is already where it is; queueing its own track back to it
    // would restart what everything else is being lined up with.
    let followers: Vec<String> = followers
        .into_iter()
        .filter(|id| *id != device_id)
        .collect();
    if followers.is_empty() {
        return Err(AppError::bad_request("No other devices to sync"));
    }

    let (state, track) = (&state, &track);
    let synced = reach_devices(followers, "Failed to queue playback", |did| async move {
        state.queue_play(&did, track.clone(), position_ms).await
    })
    .await?;
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": synced,
        "position_ms": position_ms,
        "message": t!("api_sync_queued", locale = &client_locale(&headers)),
    })))
}

/// Where a follower should pick the leader's track up.
///
/// A live stream has no position to match — the relay hands out whatever is at
/// the live edge either way — and a finite track is clamped short of its end,
/// as a seek is, so a follower does not start by immediately finishing.
fn sync_offset(leader: &DeviceState, track: &AudioTrack) -> u64 {
    if track.is_live {
        return 0;
    }
    let position = leader.estimated_position_ms(crate::state::now_f64());
    // A track of unknown length has nothing to clamp against, and the position
    // is still the best answer — unlike a seek, it was measured, not asked for.
    track
        .max_offset_ms()
        .map_or(position, |max_ms| position.min(max_ms))
}

/// POST /api/devices/:id/stop
pub async fn stop_device(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
) -> AppResult<Json<Value>> {
    // update_device already reads the device, so it reports the 404 rather than
    // us paying for a second lookup
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
