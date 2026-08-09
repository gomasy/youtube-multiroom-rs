//! Named playlists: their lifecycle, and which tracks belong to them.

use super::{
    AppError, AppResult, TrackIdsRequest, client_locale, playlist_or_404, track_or_404,
    written_or_err,
};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;
use rust_i18n::t;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// GET /api/playlists
pub async fn list_playlists(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "playlists": state.playlists_json().await }))
}

/// Body of both endpoints that name a playlist: create and rename.
#[derive(Deserialize)]
pub struct PlaylistNameRequest {
    name: String,
}

/// POST /api/playlists
pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlaylistNameRequest>,
) -> AppResult<Json<Value>> {
    let playlist = state
        .create_playlist(&req.name)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid playlist name"))?;
    state.broadcast_playlists().await;
    // Return in the same shape as the client's Playlist type (count required)
    let mut playlist = serde_json::to_value(&playlist)
        .map_err(|e| AppError::internal(format!("Failed to serialize playlist: {e}")))?;
    playlist["count"] = json!(0);
    Ok(Json(json!({ "status": "ok", "playlist": playlist })))
}

/// PATCH /api/playlists/:id
pub async fn rename_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Json(req): Json<PlaylistNameRequest>,
) -> AppResult<Json<Value>> {
    if !state.rename_playlist(&playlist_id, &req.name).await {
        return Err(AppError::bad_request("Invalid name or playlist not found"));
    }
    state.broadcast_playlists().await;
    Ok(Json(json!({ "status": "ok" })))
}

/// DELETE /api/playlists/:id
pub async fn delete_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
) -> AppResult<Json<Value>> {
    if !state.delete_playlist(&playlist_id).await {
        return Err(AppError::not_found("Playlist not found"));
    }
    state.broadcast_playlists().await;
    // If this playlist was the active playback scope, notify that it reverted to full library
    state.broadcast_active_playlist().await;
    Ok(Json(json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub struct PlaylistTrackRequest {
    track_id: String,
}

/// POST /api/playlists/:id/tracks
pub async fn add_playlist_track(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Json(req): Json<PlaylistTrackRequest>,
) -> AppResult<Json<Value>> {
    playlist_or_404(&state, &playlist_id).await?;
    let track = track_or_404(&state, &req.track_id).await?;
    written_or_err(
        state.add_playlist_track(&playlist_id, &track.id).await,
        "Playlist not found",
        "Failed to add track to playlist",
    )?;
    state.broadcast_playlists().await;
    // Notify clients viewing this playlist to refresh their track list
    state.broadcast_tracks();
    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "message": t!("api_added_to_playlist", locale = &locale, title = &track.title),
    })))
}

/// DELETE /api/playlists/:id/tracks/:track_id
pub async fn remove_playlist_track(
    State(state): State<Arc<AppState>>,
    Path((playlist_id, track_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    playlist_or_404(&state, &playlist_id).await?;
    if !state.remove_playlist_track(&playlist_id, &track_id).await {
        return Err(AppError::not_found("Track not in playlist"));
    }
    state.broadcast_playlists().await;
    state.broadcast_tracks();
    Ok(Json(json!({ "status": "ok" })))
}

/// POST /api/playlists/:id/tracks/bulk
pub async fn bulk_add_playlist_tracks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    playlist_or_404(&state, &playlist_id).await?;
    // Resolving the whole list up front costs no accuracy: a track deleted
    // between here and its append is stripped from every playlist by the
    // deletion itself. Dropping repeats matters beyond the saved round-trip —
    // the script moves an already-listed track to the end, so a repeat would
    // have reordered the playlist against the request.
    let track_ids = req.known_ids(&state).await;
    let ids: Vec<&str> = track_ids.iter().map(String::as_str).collect();
    // A playlist deleted mid-request stops the append rather than it being
    // written where nobody can see it.
    let (added, outcome) = state.add_playlist_tracks(&playlist_id, &ids).await;

    // Whatever landed before a failure is real, so clients are told about it
    // even when the request as a whole is reported as failed.
    if added > 0 {
        state.broadcast_playlists().await;
        state.broadcast_tracks();
    }
    written_or_err(
        outcome,
        "Playlist not found",
        "Failed to add tracks to playlist",
    )?;
    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "added": added,
        "message": t!("api_bulk_added_to_playlist", locale = &locale, count = added),
    })))
}

/// POST /api/playlists/:id/tracks/bulk-remove
pub async fn bulk_remove_playlist_tracks(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Json(req): Json<TrackIdsRequest>,
) -> AppResult<Json<Value>> {
    playlist_or_404(&state, &playlist_id).await?;
    let removed = state
        .remove_playlist_tracks(&playlist_id, &req.track_ids)
        .await
        .map_err(|e| {
            tracing::warn!("Redis error removing tracks from playlist {playlist_id}: {e}");
            AppError::internal("Failed to remove tracks from playlist")
        })?;
    if removed > 0 {
        state.broadcast_playlists().await;
        state.broadcast_tracks();
    }
    Ok(Json(json!({ "status": "ok", "removed": removed })))
}
