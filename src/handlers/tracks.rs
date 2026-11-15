//! The audio library: paging through it, reordering it, refreshing what it
//! knows about a track, and deleting from it.

use super::{
    AppError, AppResult, TrackIdsRequest, client_locale, job_started, playlist_or_404, track_or_404,
};
use crate::state::{AppState, AudioTrack, ReorderOutcome, ReorderRequest};
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
    // Sampled before the read (see AppState::tracks_rev).
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

/// POST /api/tracks/bulk-delete
pub async fn bulk_delete_tracks(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    let deleted = state.remove_tracks(&req.track_ids).await;
    if deleted > 0 {
        state.broadcast_track_removal().await;
    }
    Ok(Json(json!({ "status": "ok", "deleted": deleted })))
}

/// POST /api/tracks/refresh-metadata
///
/// Re-fetches metadata from YouTube for the given tracks in a background job;
/// the response says how many tracks it will visit.
pub async fn refresh_tracks_metadata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    let track_ids = req.known_ids(&state).await;
    let total = track_ids.len();
    if total > 0 {
        state.start_metadata_refresh(track_ids).await;
    }
    let locale = client_locale(&headers);
    Ok(job_started(
        total,
        &t!(
            "api_metadata_refresh_started",
            locale = &locale,
            count = total
        ),
    ))
}

/// POST /api/tracks/download
///
/// Starts a background download of the streamed tracks among those given and
/// says how many that is.
pub async fn download_tracks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    let track_ids = req.known_ids_where(&state, |t| t.streamed).await;
    let total = track_ids.len();
    if total > 0 {
        state.start_track_download(track_ids).await;
    }
    let locale = client_locale(&headers);
    Ok(job_started(
        total,
        &t!(
            "api_track_download_started",
            locale = &locale,
            count = total
        ),
    ))
}

/// POST /api/tracks/release
///
/// Turns the cached tracks among those given into streamed ones, deleting
/// their files.
pub async fn release_tracks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    let track_ids = req.known_ids_where(&state, AudioTrack::keeps_file).await;
    let released = state.release_downloads(&track_ids).await.len();
    if released > 0 {
        state.broadcast_tracks();
        state.broadcast_devices().await;
    }
    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "released": released,
        "message": t!("api_tracks_released", locale = &locale, count = released),
    })))
}

/// DELETE /api/tracks/:id
pub async fn delete_track(
    State(state): State<Arc<AppState>>,
    Path(track_id): Path<String>,
) -> AppResult<Json<Value>> {
    if state.remove_tracks(&[track_id]).await == 0 {
        return Err(AppError::not_found("Track not found"));
    }
    state.broadcast_track_removal().await;
    Ok(Json(json!({ "status": "ok" })))
}
