//! Exporting the library structure and importing it back.

use super::{AppError, AppResult, client_locale};
use crate::state::{AppState, LIBRARY_EXPORT_VERSION, LibraryExport};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use rust_i18n::t;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// An exported document, plus what the importer should do beyond restoring the
/// structure it describes.
#[derive(Deserialize)]
pub struct ImportRequest {
    #[serde(flatten)]
    doc: LibraryExport,
    /// Also fetch the audio for the videos this server has no track for. Off by
    /// default: a large library is a long series of yt-dlp runs, and an import
    /// that only restores the ordering is instant.
    #[serde(default)]
    download: bool,
}

/// GET /api/library/export
pub async fn export_library(State(state): State<Arc<AppState>>) -> Json<LibraryExport> {
    Json(state.export_library().await)
}

/// POST /api/library/import
pub async fn import_library(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ImportRequest>,
) -> AppResult<Json<Value>> {
    // Refused rather than guessed at: a document this server cannot read
    // correctly would be applied as a silently partial import.
    if req.doc.version != LIBRARY_EXPORT_VERSION {
        return Err(AppError::bad_request(format!(
            "Unsupported export version {} (this server reads version {LIBRARY_EXPORT_VERSION})",
            req.doc.version
        )));
    }

    let outcome = state.import_library(&req.doc).await;
    let missing = outcome.missing_ids.len();
    let downloading = if req.download && missing > 0 {
        // Captured before spawning, so a Stop all received immediately
        // afterwards still cancels this job.
        let cancel = state.download_token().await;
        state.start_track_recovery(outcome.missing_ids, cancel);
        missing
    } else {
        0
    };

    let locale = client_locale(&headers);
    Ok(Json(json!({
        "status": "ok",
        "playlists": outcome.playlists,
        "tracks": outcome.tracks,
        "missing": missing,
        "downloading": downloading,
        "message": t!(
            "api_library_imported",
            locale = &locale,
            tracks = outcome.tracks,
            playlists = outcome.playlists,
        ),
    })))
}
