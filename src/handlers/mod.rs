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
//! - [`cache`] — the state of the audio cache, and repairing it
//! - [`library`] — exporting and importing the library structure
//! - [`alexa`] — the Alexa skill webhook
//! - [`ws`] — the WebSocket push channel
//!
//! What the modules share — the error type, the "resolve or 404" lookups and
//! the response locale — lives here.

mod alexa;
mod audio;
mod cache;
mod devices;
mod library;
mod playlists;
mod search;
mod tracks;
mod ws;

pub use alexa::alexa_webhook;
pub use audio::{audio_url, live_audio, relay_audio, stream_audio};
pub use cache::{cache_status, cleanup_cache, repair_cache};
pub use devices::{
    clear_queue, delete_device, get_devices, play_on_all, play_on_devices, queue_next,
    remove_queue_item, seek_device, stop_device, sync_devices,
};
pub use library::{export_library, import_library};
pub use playlists::{
    add_playlist_track, bulk_add_playlist_tracks, bulk_remove_playlist_tracks, create_playlist,
    delete_playlist, list_playlists, remove_playlist_track, rename_playlist, sync_playlist,
};
pub use search::search_youtube;
pub use tracks::{
    bulk_delete_tracks, delete_track, download_tracks, list_tracks, refresh_tracks_metadata,
    release_tracks, reorder_track,
};
pub use ws::ws_upgrade;

use crate::state::{AppState, AudioTrack, DeviceState, WriteOutcome};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) type AppResult<T> = Result<T, AppError>;

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
    pub(crate) fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Unauthorized".to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "detail": self.message });
        (self.status, Json(body)).into_response()
    }
}

/// Body shared by every endpoint that acts on a set of tracks.
#[derive(Deserialize)]
pub struct TrackIdsRequest {
    track_ids: Vec<String>,
}

impl TrackIdsRequest {
    /// The requested tracks that exist, in the order asked for and without
    /// repeats, resolved in one HMGET.
    pub(crate) async fn known_ids(&self, state: &AppState) -> Vec<String> {
        self.known_ids_where(state, |_| true).await
    }

    /// [`Self::known_ids`], narrowed to the tracks `keep` accepts.
    pub(crate) async fn known_ids_where(
        &self,
        state: &AppState,
        keep: impl Fn(&AudioTrack) -> bool,
    ) -> Vec<String> {
        let mut unclaimed = state.fetch_tracks_for(self.track_ids.iter()).await;
        self.track_ids
            .iter()
            .filter(|id| unclaimed.remove(id.as_str()).is_some_and(|t| keep(&t)))
            .cloned()
            .collect()
    }
}

/// Turn a guarded write's outcome into a handler result: a vanished target is
/// a 404, a failed write a 500.
pub(crate) fn written_or_err(
    outcome: WriteOutcome,
    gone: &'static str,
    failed: &'static str,
) -> AppResult<()> {
    match outcome {
        WriteOutcome::Written => Ok(()),
        WriteOutcome::Gone => Err(AppError::not_found(gone)),
        WriteOutcome::Failed => Err(AppError::internal(failed)),
    }
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

/// The reply of an endpoint that started a background job over `total` items.
pub(crate) fn job_started(total: usize, message: &str) -> Json<Value> {
    Json(json!({ "status": "ok", "total": total, "message": message }))
}

/// The response locale, from the client's X-App-Lang header.
pub(crate) fn client_locale(headers: &HeaderMap) -> String {
    crate::locale::or_default(headers.get("x-app-lang").and_then(|v| v.to_str().ok()))
}
