use crate::alexa::handle_alexa;
use crate::state::{AppState, DeviceUpdate, ExtractRequest, PlayRequest};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::fs;

type AppResult<T> = Result<T, AppError>;

// ════════════════════════════════════════
// エラー型
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
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "detail": self.message });
        (self.status, Json(body)).into_response()
    }
}

// ════════════════════════════════════════
// 音声 API
// ════════════════════════════════════════

/// POST /api/audio/extract
pub async fn extract_audio(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExtractRequest>,
) -> AppResult<Json<Value>> {
    let track = state
        .extract_audio(&req.url)
        .await
        .map_err(AppError::bad_request)?;
    state.broadcast_tracks().await;
    Ok(Json(json!(track)))
}

/// GET /api/audio/:id/stream
pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let track = state
        .get_track(&audio_id)
        .await
        .ok_or_else(|| AppError::not_found("Audio not found"))?;

    let bytes = fs::read(&track.file_path)
        .await
        .map_err(|e| AppError::not_found(format!("Failed to read file: {e}")))?;

    let total = bytes.len();

    if let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(range) = parse_byte_range(range, total) {
            let body = bytes[range.0..=range.1].to_vec();
            return Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, "audio/mpeg".to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", range.0, range.1, total),
                    ),
                    (header::CONTENT_LENGTH, body.len().to_string()),
                    (
                        header::CACHE_CONTROL,
                        "public, max-age=3600".to_string(),
                    ),
                ],
                body,
            )
                .into_response());
        }
    }

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "audio/mpeg".to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_LENGTH, total.to_string()),
            (
                header::CACHE_CONTROL,
                "public, max-age=3600".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

fn parse_byte_range(header: &str, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;
    let start = if start_str.is_empty() {
        let suffix_len: usize = end_str.parse().ok()?;
        total.saturating_sub(suffix_len)
    } else {
        start_str.parse().ok()?
    };
    let end = if end_str.is_empty() {
        total - 1
    } else {
        end_str.parse::<usize>().ok()?.min(total - 1)
    };
    if start <= end && start < total {
        Some((start, end))
    } else {
        None
    }
}

/// GET /api/tracks
pub async fn list_tracks(State(state): State<Arc<AppState>>) -> Json<Value> {
    let tracks = state.list_tracks().await;
    Json(json!(tracks))
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
    Ok(Json(json!({ "status": "ok" })))
}

// ════════════════════════════════════════
// デバイス & 再生制御 API
// ════════════════════════════════════════

/// GET /api/devices
pub async fn get_devices(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.devices_json().await)
}

/// POST /api/play
pub async fn play_on_devices(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = state
        .get_track(&req.track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))?;

    for did in &req.device_ids {
        state.queue_play(did, track.clone()).await;
    }

    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": req.device_ids,
        "message": "Say 'Alexa, open YouTube Player' on each Echo device"
    })))
}

/// POST /api/play-all
pub async fn play_on_all(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = state
        .get_track(&req.track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))?;

    let device_ids: Vec<String> = state
        .devices
        .read()
        .await
        .keys()
        .cloned()
        .collect();

    for did in &device_ids {
        state.queue_play(did, track.clone()).await;
    }

    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": device_ids,
        "message": "Say 'Alexa, open YouTube Player' on each Echo device"
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
pub async fn alexa_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
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
pub async fn ws_upgrade(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| ws_handler(socket, state))
}

async fn ws_handler(mut socket: WebSocket, state: Arc<AppState>) {
    tracing::info!("WebSocket client connected");

    // 初期状態を送信
    let init_msg = json!({
        "type": "init",
        "devices": state.devices_json().await,
        "tracks": state.tracks_json().await,
    });
    if socket
        .send(Message::Text(init_msg.to_string()))
        .await
        .is_err()
    {
        return;
    }

    // broadcast チャンネルを購読
    let mut rx = state.tx.subscribe();

    loop {
        tokio::select! {
            // サーバー → クライアント (broadcast)
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }

            // クライアント → サーバー
            Some(Ok(msg)) = socket.recv() => {
                match msg {
                    Message::Text(text) => {
                        if let Ok(data) = serde_json::from_str::<Value>(&text) {
                            let msg_type = data["type"].as_str().unwrap_or("");
                            match msg_type {
                                "ping" => {
                                    let pong = json!({ "type": "pong" }).to_string();
                                    if socket.send(Message::Text(pong)).await.is_err() {
                                        break;
                                    }
                                }
                                "rename_device" => {
                                    if let (Some(did), Some(name)) = (
                                        data["device_id"].as_str(),
                                        data["name"].as_str(),
                                    ) {
                                        let mut upd = DeviceUpdate::new();
                                        upd.name = Some(name.to_string());
                                        state.update_device(did, upd).await;
                                        state.broadcast_devices().await;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            else => break,
        }
    }

    tracing::info!("WebSocket client disconnected");
}
