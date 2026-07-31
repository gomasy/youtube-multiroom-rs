//! The audio library: paging through it, reordering it, and deleting from it.

use super::{AppError, AppResult, TrackIdsRequest, playlist_or_404, track_or_404};
use crate::state::{AppState, ReorderOutcome, ReorderRequest};
use axum::extract::{Path, Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TracksQuery {
    page: Option<usize>,
    per_page: Option<usize>,
    /// When specified, return tracks in this playlist's order (omit for full library).
    playlist: Option<String>,
    /// Case-insensitive substring filter on title and channel name.
    q: Option<String>,
}

/// GET /api/tracks?page=1&per_page=10&playlist={id}
pub async fn list_tracks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TracksQuery>,
) -> AppResult<Json<Value>> {
    // Restore track metadata from audio_cache if Redis was cleared
    state.restore_tracks_if_missing().await;

    if let Some(pid) = &query.playlist {
        playlist_or_404(&state, pid).await?;
    }
    // Sampled before the read so a change landing while this page is assembled
    // is reported as a newer revision than the one served with it.
    let rev = state.tracks_rev();
    let per_page = query.per_page.unwrap_or(10).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let filter = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (tracks, total) = state
        .list_tracks_page(query.playlist.as_deref(), page, per_page, filter)
        .await;
    Ok(Json(json!({
        "tracks": tracks,
        "total": total,
        "page": page,
        "per_page": per_page,
        "rev": rev,
    })))
}

/// POST /api/tracks/reorder
///
/// Reorders a track within a playlist (if specified) or the full library.
pub async fn reorder_track(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReorderRequest>,
) -> AppResult<Json<Value>> {
    track_or_404(&state, &req.track_id).await?;
    if let Some(pid) = &req.playlist {
        playlist_or_404(&state, pid).await?;
    }
    match state
        .reorder_track(req.playlist.as_deref(), &req.track_id, req.new_index)
        .await
    {
        ReorderOutcome::Moved => {}
        // Track not in the list (not added to playlist, or removed concurrently)
        ReorderOutcome::NotInList => return Err(AppError::not_found("Track not in the list")),
        ReorderOutcome::Failed => return Err(AppError::internal("Failed to save track order")),
    }
    state.broadcast_tracks();
    Ok(Json(json!({ "status": "ok" })))
}

/// Announce a track deletion. Deleting a track also strips it from playlists
/// and from device queues, so all three views have to be refreshed together.
async fn broadcast_track_removal(state: &AppState) {
    state.broadcast_tracks();
    state.broadcast_devices().await;
    state.broadcast_playlists().await;
}

/// POST /api/tracks/bulk-delete
pub async fn bulk_delete_tracks(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    let mut deleted = 0u32;
    for id in &req.track_ids {
        if state.remove_track(id).await.is_some() {
            deleted += 1;
        }
    }
    if deleted > 0 {
        broadcast_track_removal(&state).await;
    }
    Ok(Json(json!({ "status": "ok", "deleted": deleted })))
}

/// DELETE /api/tracks/:id
pub async fn delete_track(
    State(state): State<Arc<AppState>>,
    Path(track_id): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .remove_track(&track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))?;
    broadcast_track_removal(&state).await;
    Ok(Json(json!({ "status": "ok" })))
}
