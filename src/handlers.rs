use crate::alexa::handle_alexa;
use crate::state::{AppState, DeviceUpdate, ExtractRequest, PlayRequest};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::StatusCode;
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
    Ok(Json(json!(track)))
}

/// GET /api/audio/:id/stream
pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Response> {
    let track = state
        .get_track(&audio_id)
        .await
        .ok_or_else(|| AppError::not_found("Audio not found"))?;

    let bytes = fs::read(&track.file_path)
        .await
        .map_err(|e| AppError::not_found(format!("ファイル読み取りエラー: {e}")))?;

    Ok((
        StatusCode::OK,
        [
            ("content-type", "audio/mpeg"),
            ("accept-ranges", "bytes"),
            ("cache-control", "public, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
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
        "message": "各 Echo で「アレクサ、YouTube プレーヤーを開いて」と言ってください"
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
        "message": "各 Echo で「アレクサ、YouTube プレーヤーを開いて」と言ってください"
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
    Json(body): Json<Value>,
) -> Json<Value> {
    let req_type = body["request"]["type"].as_str().unwrap_or("unknown");
    tracing::info!("Alexa request: {}", req_type);
    Json(handle_alexa(&state, body).await)
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
