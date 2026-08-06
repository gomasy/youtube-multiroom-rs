//! The audio library: paging through it, reordering it, refreshing what it
//! knows about a track, and deleting from it.

use super::{AppError, AppResult, TrackIdsRequest, client_locale, playlist_or_404, track_or_404};
use crate::state::{AppState, ReorderOutcome, ReorderRequest};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;
use rust_i18n::t;
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
    let tracks = state.with_file_status(tracks).await;
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
    let deleted = state.remove_tracks(&req.track_ids).await;
    if deleted > 0 {
        broadcast_track_removal(&state).await;
    }
    Ok(Json(json!({ "status": "ok", "deleted": deleted })))
}

/// POST /api/tracks/refresh-metadata
///
/// Re-fetches title, thumbnail, channel and duration from YouTube for the given
/// tracks. Every track costs a yt-dlp run, so the work is handed to a background
/// job and reported over the download progress channel; the response says only
/// how many tracks that job will visit.
pub async fn refresh_tracks_metadata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    // An ID that names nothing would cost the job a 30-second yt-dlp run that
    // could only end in "track not found", and a repeated one would fetch the
    // same video twice; neither survives the resolution.
    let track_ids = req.known_ids(&state).await;
    let total = track_ids.len();

    if total > 0 {
        // Capture the token before spawning, so a Stop all received immediately
        // afterwards still cancels this job.
        let cancel = state.download_token().await;
        state.start_metadata_refresh(track_ids, cancel);
    }
    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "total": total,
        "message": t!("api_metadata_refresh_started", locale = &locale, count = total),
    })))
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
