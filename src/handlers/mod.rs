//! HTTP and WebSocket request handlers.
//!
//! Every route registered in main.rs resolves to one of these. The handlers are
//! grouped by subject into sibling modules, mirroring the split in [`crate::state`]:
//!
//! - [`audio`] — serving audio bytes to Echo devices and the browser
//! - [`search`] — YouTube search
//! - [`tracks`] — the audio library: listing, ordering, deletion, metadata
//! - [`playlists`] — named playlists and their membership
//! - [`devices`] — device state, playback commands, Up Next queues
//! - [`alexa`] — the Alexa skill webhook
//! - [`ws`] — the WebSocket push channel
//!
//! The modules are private: main.rs reaches every handler through the
//! re-exports below, so `crate::handlers::X` stays the single import path.
//! What the modules share — the error type, the "resolve or 404" lookups and
//! the response locale — lives here.

mod alexa;
mod audio;
mod devices;
mod playlists;
mod search;
mod tracks;
mod ws;

pub use alexa::alexa_webhook;
pub use audio::{audio_url, live_audio, stream_audio};
pub use devices::{
    clear_queue, delete_device, get_devices, play_on_all, play_on_devices, queue_next,
    remove_queue_item, seek_device, stop_device,
};
pub use playlists::{
    add_playlist_track, bulk_add_playlist_tracks, bulk_remove_playlist_tracks, create_playlist,
    delete_playlist, list_playlists, remove_playlist_track, rename_playlist,
};
pub use search::search_youtube;
pub use tracks::{
    bulk_delete_tracks, delete_track, list_tracks, refresh_tracks_metadata, reorder_track,
};
pub use ws::ws_upgrade;

use crate::state::{AppState, AudioTrack, DeviceState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

pub(crate) type AppResult<T> = Result<T, AppError>;

// ════════════════════════════════════════
// Error type
// ════════════════════════════════════════

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    pub(crate) fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub(crate) fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "detail": self.message });
        (self.status, Json(body)).into_response()
    }
}

// ════════════════════════════════════════
// Shared request vocabulary
// ════════════════════════════════════════

/// Body shared by every endpoint that acts on a set of tracks.
#[derive(Deserialize)]
pub struct TrackIdsRequest {
    track_ids: Vec<String>,
}

pub(crate) async fn track_or_404(state: &AppState, track_id: &str) -> AppResult<AudioTrack> {
    state
        .get_track(track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))
}

pub(crate) async fn device_or_404(state: &AppState, device_id: &str) -> AppResult<DeviceState> {
    state
        .get_device(device_id)
        .await
        .ok_or_else(|| AppError::not_found("Device not found"))
}

pub(crate) async fn playlist_or_404(state: &AppState, playlist_id: &str) -> AppResult<()> {
    if state.playlist_exists(playlist_id).await {
        Ok(())
    } else {
        Err(AppError::not_found("Playlist not found"))
    }
}

/// Resolve the response locale for this request. The client advertises its
/// locale via the X-App-Lang header (derived from navigator.language); the
/// fallback policy itself lives in crate::locale.
pub(crate) fn client_locale(headers: &HeaderMap) -> String {
    crate::locale::or_default(headers.get("x-app-lang").and_then(|v| v.to_str().ok()))
}
