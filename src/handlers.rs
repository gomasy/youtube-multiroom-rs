use crate::alexa::handle_alexa;
use crate::state::{
    AUDIO_MIME, AppState, AudioTrack, DeviceUpdate, PlayRequest, ReorderOutcome, ReorderRequest,
    SeekRequest, UrlKind, classify_url, run_yt_dlp,
};
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use rust_i18n::t;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

type AppResult<T> = Result<T, AppError>;

// ════════════════════════════════════════
// Error type
// ════════════════════════════════════════

pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
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
// Audio API
// ════════════════════════════════════════

/// GET /api/audio/:id/stream
///
/// Streams the file with Range support (no full in-memory read). Echo devices
/// issue repeated Range requests during playback.
pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let track = state
        .get_track(&audio_id)
        .await
        .ok_or_else(|| AppError::not_found("Audio not found"))?;

    let mut file = fs::File::open(&track.file_path)
        .await
        .map_err(|e| AppError::not_found(format!("Failed to open file: {e}")))?;
    let total = file
        .metadata()
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat file: {e}")))?
        .len() as usize;

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| parse_byte_range(r, total));

    let mut resp = Response::builder()
        .header(header::CONTENT_TYPE, AUDIO_MIME)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, max-age=3600");

    let body = if let Some((start, end)) = range {
        file.seek(SeekFrom::Start(start as u64))
            .await
            .map_err(|e| AppError::internal(format!("Failed to seek: {e}")))?;
        let len = end - start + 1;
        resp = resp
            .status(StatusCode::PARTIAL_CONTENT)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )
            .header(header::CONTENT_LENGTH, len);
        Body::from_stream(ReaderStream::new(file.take(len as u64)))
    } else {
        resp = resp.header(header::CONTENT_LENGTH, total);
        Body::from_stream(ReaderStream::new(file))
    };

    resp.body(body)
        .map_err(|e| AppError::internal(format!("Failed to build response: {e}")))
}

/// GET /api/audio/:id/live
///
/// Live streams cannot be saved as files, so we resolve the CDN HLS URL via
/// yt-dlp on each request, then relay audio-only (AAC) through ffmpeg as an
/// ADTS stream. Echo devices cannot play muxed HLS with video, so server-side
/// audio extraction is required. Audio is codec-copied (no re-encoding) for
/// minimal CPU overhead.
pub async fn live_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Response> {
    let track = track_or_404(&state, &audio_id).await?;

    if !track.is_live {
        return Err(AppError::bad_request("Track is not a live stream"));
    }

    let url = format!("https://www.youtube.com/watch?v={audio_id}");
    // Prefer HLS which ffmpeg handles well. Live streams often lack audio-only
    // formats, so fall back to the lowest-bitrate muxed HLS (video+audio).
    // Also fetch acodec to decide whether re-encoding is needed.
    // Use a short timeout since Echo devices can't wait long.
    let stdout = run_yt_dlp(
        &[
            "--print",
            "urls",
            "--print",
            "acodec",
            "-f",
            "bestaudio[protocol^=m3u8]/worst[protocol^=m3u8]/bestaudio/worst",
            "--no-playlist",
            &url,
        ],
        std::time::Duration::from_secs(15),
    )
    .await
    .map_err(|e| AppError::internal(format!("Failed to get live stream URL: {e}")))?;

    // Output is URL (may be multiple lines for DASH) then acodec. Use only the first URL.
    let stdout_str = String::from_utf8_lossy(&stdout);
    let lines: Vec<&str> = stdout_str
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let (cdn_url, acodec) = match lines.as_slice() {
        [urls @ .., acodec] if !urls.is_empty() => (urls[0], *acodec),
        _ => return Err(AppError::internal("yt-dlp returned empty stream URL")),
    };

    // AAC can be remuxed as-is; other codecs (Opus, etc.) need transcoding
    // since ADTS only supports AAC. "unknown" acodec (common in muxed HLS)
    // also triggers transcoding to be safe.
    let codec_args: &[&str] = if acodec.starts_with("mp4a") || acodec.starts_with("aac") {
        &["-c:a", "copy"]
    } else {
        tracing::info!("Live audio codec '{acodec}' is not AAC, transcoding");
        &["-c:a", "aac", "-b:a", "128k"]
    };

    let mut child = tokio::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i", cdn_url, "-vn"])
        .args(codec_args)
        .args(["-f", "adts", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::internal(format!("Failed to run ffmpeg: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::internal("Failed to capture ffmpeg stdout"))?;
    let stderr = child.stderr.take();

    // When Echo disconnects, the response body and stdout pipe close, causing
    // ffmpeg to exit naturally via EPIPE. Reap the child to prevent zombies
    // and log any stderr for debugging.
    tokio::spawn(async move {
        let mut err_buf = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut err_buf).await;
        }
        let err = err_buf.trim();
        match child.wait().await {
            Ok(status) if err.is_empty() => {
                tracing::info!("ffmpeg exited: {status}")
            }
            Ok(status) => tracing::warn!("ffmpeg exited: {status}: {err}"),
            Err(e) => tracing::warn!("ffmpeg wait error: {e}"),
        }
    });

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "audio/aac".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(ReaderStream::new(stdout)),
    )
        .into_response())
}

/// GET /api/audio/:id/url
///
/// Returns a stream URL (signed relative path when auth is enabled) for
/// browser preview playback via the audio element. This endpoint itself
/// requires Bearer auth, so third parties cannot mint signed URLs.
pub async fn audio_url(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &audio_id).await?;
    let url = crate::auth::stream_path(state.api_token.as_deref(), &track.id, track.is_live);
    Ok(Json(json!({ "url": url })))
}

fn parse_byte_range(header: &str, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;
    let (start, end) = if start_str.is_empty() {
        // Suffix range (bytes=-N): last N bytes
        let suffix_len: usize = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        (total.saturating_sub(suffix_len), total - 1)
    } else {
        let start = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            total - 1
        } else {
            end_str.parse::<usize>().ok()?.min(total - 1)
        };
        (start, end)
    };
    if start <= end && start < total {
        Some((start, end))
    } else {
        None
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    limit: Option<usize>,
}

/// GET /api/search?q=...&limit=8
///
/// Searches YouTube via yt-dlp ytsearch and returns lightweight metadata in
/// the same shape as /api/tracks. Uses --flat-playlist to skip resolving
/// individual video pages, keeping response time to a few seconds.
pub async fn search_youtube(Query(query): Query<SearchQuery>) -> AppResult<Json<Value>> {
    let q = query.q.trim();
    if q.is_empty() {
        return Err(AppError::bad_request("Search query is empty"));
    }
    let limit = query.limit.unwrap_or(8).clamp(1, 20);

    let target = format!("ytsearch{limit}:{q}");
    let stdout = run_yt_dlp(
        &["--dump-json", "--flat-playlist", &target],
        std::time::Duration::from_secs(30),
    )
    .await
    .map_err(|e| AppError::internal(format!("Search failed: {e}")))?;

    // Output is one JSON object per video per line
    let results: Vec<AudioTrack> = String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| search_entry(&v))
        .collect();

    Ok(Json(json!({ "results": results })))
}

/// Convert a yt-dlp flat-playlist entry into an AudioTrack for search results.
/// Using AudioTrack ensures wire-format compatibility with /api/tracks
/// (file_path is serde(skip) so it's never exposed).
fn search_entry(v: &Value) -> Option<AudioTrack> {
    let id = v["id"].as_str()?;
    Some(AudioTrack {
        id: id.to_string(),
        title: v["title"].as_str().unwrap_or(id).to_string(),
        // Flat entries have inconsistent thumbnail formats; use a known URL pattern
        thumbnail: format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"),
        duration: v["duration"].as_f64().unwrap_or(0.0) as u64,
        channel: v["channel"]
            .as_str()
            .or(v["uploader"].as_str())
            .unwrap_or("")
            .to_string(),
        is_live: v["live_status"].as_str() == Some("is_live"),
        created_at: 0.0,
        file_path: String::new(),
    })
}

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
    let per_page = query.per_page.unwrap_or(10).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let filter = query.q.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
    let (tracks, total) = state
        .list_tracks_page(query.playlist.as_deref(), page, per_page, filter)
        .await;
    Ok(Json(json!({
        "tracks": tracks,
        "total": total,
        "page": page,
        "per_page": per_page,
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
    state.broadcast_tracks().await;
    Ok(Json(json!({ "status": "ok" })))
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
    state.broadcast_tracks().await;
    state.broadcast_devices().await;
    // Update playlist counts for any playlists that contained this track
    state.broadcast_playlists().await;
    Ok(Json(json!({ "status": "ok" })))
}

// ════════════════════════════════════════
// Playlist API
// ════════════════════════════════════════

async fn playlist_or_404(state: &AppState, playlist_id: &str) -> AppResult<()> {
    state
        .get_playlist(playlist_id)
        .await
        .map(|_| ())
        .ok_or_else(|| AppError::not_found("Playlist not found"))
}

/// GET /api/playlists
pub async fn list_playlists(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "playlists": state.playlists_json().await }))
}

#[derive(Deserialize)]
pub struct CreatePlaylistRequest {
    name: String,
}

/// POST /api/playlists
pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePlaylistRequest>,
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

#[derive(Deserialize)]
pub struct RenamePlaylistRequest {
    name: String,
}

/// PATCH /api/playlists/:id
pub async fn rename_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Json(req): Json<RenamePlaylistRequest>,
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
    if !state.add_playlist_track(&playlist_id, &track.id).await {
        return Err(AppError::internal("Failed to add track to playlist"));
    }
    state.broadcast_playlists().await;
    // Notify clients viewing this playlist to refresh their track list
    state.broadcast_tracks().await;
    let locale = client_locale(&headers, &state);
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
    state.broadcast_tracks().await;
    Ok(Json(json!({ "status": "ok" })))
}

// ════════════════════════════════════════
// Device & Playback Control API
// ════════════════════════════════════════

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
    let locale = client_locale(&headers, &state);
    queue_on_devices(&state, track, req.device_ids, &locale).await
}

/// POST /api/play-all
pub async fn play_on_all(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &req.track_id).await?;
    let locale = client_locale(&headers, &state);
    let device_ids = state
        .device_ids()
        .await
        .map_err(|e| AppError::internal(format!("Failed to list devices: {e}")))?;
    queue_on_devices(&state, track, device_ids, &locale).await
}

async fn track_or_404(state: &AppState, track_id: &str) -> AppResult<AudioTrack> {
    state
        .get_track(track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))
}

/// Resolve the response locale for this request. The client advertises its
/// locale via the X-App-Lang header (derived from navigator.language); when
/// absent or unrecognized we fall back to the server-wide APP_LANG default.
fn client_locale(headers: &HeaderMap, state: &AppState) -> String {
    headers
        .get("x-app-lang")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::resolve_locale)
        .unwrap_or_else(|| state.locale.clone())
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
    let locale = client_locale(&headers, &state);
    let track = track_or_404(&state, &req.track_id).await?;
    let mut queued = Vec::new();
    for did in &req.device_ids {
        // Only queue on registered devices to avoid orphaned Redis keys
        if state.get_device(did).await.is_none() {
            continue;
        }
        if state.push_queue(did, &track.id).await {
            queued.push(did.clone());
        }
    }
    if queued.is_empty() {
        return Err(AppError::bad_request("No valid devices"));
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
) -> Json<Value> {
    state.clear_queue(&device_id).await;
    state.broadcast_devices().await;
    Json(json!({ "status": "ok" }))
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
    let dev = state
        .get_device(&device_id)
        .await
        .ok_or_else(|| AppError::not_found("Device not found"))?;
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
) -> Json<Value> {
    state
        .update_device(&device_id, DeviceUpdate::new().status("stopped"))
        .await;
    state.broadcast_devices().await;
    Json(json!({ "status": "ok" }))
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

// ════════════════════════════════════════
// Alexa Webhook
// ════════════════════════════════════════

/// POST /alexa
///
/// Not behind Bearer auth — instead, Amazon's signature verification confirms
/// the request genuinely originates from Alexa. Returns 400 on verification
/// failure per Amazon's specification.
pub async fn alexa_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    if let Err(e) = crate::alexa_verify::verify_request(&headers, &body).await {
        tracing::warn!("Rejected Alexa request: {e}");
        return Err(AppError::bad_request("Request verification failed"));
    }

    let body: Value =
        serde_json::from_slice(&body).map_err(|_| AppError::bad_request("Invalid JSON body"))?;

    if let Err(e) = crate::alexa_verify::verify_timestamp(&body) {
        tracing::warn!("Rejected Alexa request: {e}");
        return Err(AppError::bad_request("Request verification failed"));
    }

    let req_type = body["request"]["type"].as_str().unwrap_or("unknown");
    tracing::info!("Alexa request: {}", req_type);

    let base_url = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|host| format!("https://{host}"))
        .ok_or_else(|| AppError::bad_request("Host header is required"))?;

    Ok(Json(handle_alexa(&state, body, &base_url).await))
}

// ════════════════════════════════════════
// WebSocket
// ════════════════════════════════════════

/// WS /ws
pub async fn ws_upgrade(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_handler(socket, state))
}

async fn ws_handler(mut socket: WebSocket, state: Arc<AppState>) {
    tracing::info!("WebSocket client connected");

    // Subscribe before building the snapshot to avoid missing updates that
    // arrive during snapshot assembly (e.g., download completion removing an
    // entry), which would leave init stale with no subsequent correction.
    let mut rx = state.tx.subscribe();

    // Send initial state (track list is fetched via REST pagination).
    // Include in-progress downloads so the progress display is restored after reload.
    let init_msg = json!({
        "type": "init",
        "version": crate::VERSION,
        "devices": state.devices_json().await,
        "playback_mode": state.playback_mode().await,
        "downloads": state.downloads_json().await,
        "playlists": state.playlists_json().await,
        "active_playlist": state.active_playlist().await,
    });
    if socket
        .send(Message::Text(init_msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Per-client channel for individual responses (e.g., extract results)
    let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    loop {
        tokio::select! {
            // Server → client (broadcast)
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }

            // Server → client (individual response)
            Some(msg) = client_rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }

            // Client → server
            recv = socket.recv() => {
                // Handle both clean disconnects (None) and errors to ensure we
                // always break out of the loop and don't leak the task
                match recv {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(data) = serde_json::from_str::<Value>(&text) {
                            handle_ws_message(&state, &client_tx, &data).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }

            else => break,
        }
    }

    tracing::info!("WebSocket client disconnected");
}

/// Process a single client WebSocket message.
/// Responses are sent via client_tx, delivered by ws_handler's select loop.
async fn handle_ws_message(
    state: &Arc<AppState>,
    client_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    data: &Value,
) {
    match data["type"].as_str().unwrap_or("") {
        "ping" => {
            let _ = client_tx.send(json!({ "type": "pong" }).to_string());
        }
        "extract_audio" => {
            let Some(url) = data["url"].as_str() else {
                let msg = json!({
                    "type": "extract_audio_error",
                    "error": "Missing 'url' field",
                });
                let _ = client_tx.send(msg.to_string());
                return;
            };
            // Download can take a long time; run in a separate task and return result.
            // Playlist URLs are expanded and trigger a batch import.
            let state = state.clone();
            let tx = client_tx.clone();
            let url = url.to_string();
            tokio::spawn(async move {
                let result = match classify_url(&url) {
                    UrlKind::Video => match state.extract_audio(&url).await {
                        Ok(track) => {
                            state.broadcast_tracks().await;
                            json!({ "type": "extract_audio_result", "track": track })
                        }
                        Err(e) => json!({ "type": "extract_audio_error", "error": e }),
                    },
                    UrlKind::Playlist(list_id) => match state.import_playlist(&list_id).await {
                        Ok(info) => json!({
                            "type": "playlist_import_result",
                            "name": info.name,
                            "total": info.total,
                        }),
                        Err(e) => json!({ "type": "extract_audio_error", "error": e }),
                    },
                    UrlKind::Unknown => json!({
                        "type": "extract_audio_error",
                        "error": "Could not recognize YouTube URL",
                    }),
                };
                let _ = tx.send(result.to_string());
            });
        }
        "set_playback_mode" => {
            if let Some(mode) = data["mode"].as_str()
                && state.set_playback_mode(mode).await
            {
                state.broadcast_playback_mode(mode).await;
            }
        }
        "set_active_playlist" => {
            // null is a valid value (meaning "full library")
            let playlist = data["playlist"].as_str();
            if state.set_active_playlist(playlist).await {
                state.broadcast_active_playlist().await;
            }
        }
        "cancel_downloads" => {
            state.cancel_downloads().await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_byte_range, search_entry};
    use serde_json::json;

    #[test]
    fn search_entry_maps_flat_playlist_fields() {
        let v = json!({
            "id": "dQw4w9WgXcQ",
            "title": "Song",
            "duration": 212.0,
            "channel": "Ch",
            "live_status": "not_live",
        });
        let entry = serde_json::to_value(search_entry(&v).unwrap()).unwrap();
        assert_eq!(entry["id"], "dQw4w9WgXcQ");
        assert_eq!(entry["title"], "Song");
        assert_eq!(entry["duration"], 212);
        assert_eq!(entry["channel"], "Ch");
        assert_eq!(entry["is_live"], false);
        assert_eq!(
            entry["thumbnail"],
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg"
        );
        // Internal file_path must not appear in wire format
        assert!(entry.get("file_path").is_none());
    }

    #[test]
    fn search_entry_fills_missing_fields() {
        // No duration (e.g., live); uploader instead of channel
        let v = json!({
            "id": "dQw4w9WgXcQ",
            "uploader": "Up",
            "live_status": "is_live",
        });
        let entry = search_entry(&v).unwrap();
        assert_eq!(entry.title, "dQw4w9WgXcQ");
        assert_eq!(entry.duration, 0);
        assert_eq!(entry.channel, "Up");
        assert!(entry.is_live);

        // Entries without id are discarded
        assert!(search_entry(&json!({ "title": "x" })).is_none());
    }

    #[test]
    fn parses_byte_ranges() {
        // Normal range, clamped end, start-only
        assert_eq!(parse_byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_byte_range("bytes=900-1999", 1000), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=500-", 1000), Some((500, 999)));
        // Suffix range: last N bytes
        assert_eq!(parse_byte_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=-2000", 1000), Some((0, 999)));
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert_eq!(parse_byte_range("bytes=-0", 1000), None);
        assert_eq!(parse_byte_range("bytes=1000-", 1000), None);
        assert_eq!(parse_byte_range("bytes=5-2", 1000), None);
        assert_eq!(parse_byte_range("bytes=0-99", 0), None);
        assert_eq!(parse_byte_range("items=0-99", 1000), None);
        // Multi-range not supported (caller falls back to 200 full)
        assert_eq!(parse_byte_range("bytes=0-1,5-6", 1000), None);
    }
}
