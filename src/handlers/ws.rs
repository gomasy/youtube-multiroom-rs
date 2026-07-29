//! The WebSocket push channel: a state snapshot on connect, broadcasts after,
//! and the handful of commands a client can send back.

use crate::state::{AppState, DownloadError, UrlKind, classify_url, tracks_update_message};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::Response;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;

/// WS /ws
pub async fn ws_upgrade(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_handler(socket, state))
}

/// Snapshot of the pushed state a client mirrors. Deliberately excludes the
/// track list, which the client pages in over REST — callers that also need
/// tracks refreshed must send tracks_update_message() alongside it. Applied
/// idempotently by the client, so resending is always safe.
///
/// The six reads are independent, so they are issued together: the Redis
/// connection is multiplexed, and this runs on both connect and lag resync,
/// where the client is already behind.
async fn init_message(state: &AppState) -> String {
    let (devices, playback_mode, downloads, playlists, active_playlist, sleep_timer) = tokio::join!(
        state.devices_json(),
        state.playback_mode(),
        state.downloads_json(),
        state.playlists_json(),
        state.active_playlist(),
        state.sleep_timer(),
    );
    json!({
        "type": "init",
        "version": crate::VERSION,
        "devices": devices,
        "playback_mode": playback_mode,
        "downloads": downloads,
        "playlists": playlists,
        "active_playlist": active_playlist,
        "sleep_timer": sleep_timer,
    })
    .to_string()
}

async fn ws_handler(mut socket: WebSocket, state: Arc<AppState>) {
    tracing::info!("WebSocket client connected");

    // Subscribe before building the snapshot to avoid missing updates that
    // arrive during snapshot assembly (e.g., download completion removing an
    // entry), which would leave init stale with no subsequent correction.
    let mut rx = state.tx.subscribe();

    // Send initial state (track list is fetched via REST pagination).
    // Include in-progress downloads so the progress display is restored after reload.
    if socket
        .send(Message::Text(init_message(&state).await.into()))
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
            broadcast = rx.recv() => {
                // Must be matched exhaustively rather than with `Ok(msg) = ...`:
                // select! disables a branch whose pattern fails to match, so a
                // single Lagged error would stall every later broadcast until
                // another branch happened to fire.
                match broadcast {
                    Ok(msg) => {
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    // This client fell behind and the skipped messages are gone.
                    // Resend the full snapshot plus a tracks_update so it
                    // re-syncs everything instead of drifting silently.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket client lagged by {n} message(s); resyncing");
                        let init = init_message(&state).await;
                        let tracks = tracks_update_message().to_string();
                        if socket.send(Message::Text(init.into())).await.is_err()
                            || socket.send(Message::Text(tracks.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    // Only possible once every sender is gone, which cannot
                    // happen while this task holds an Arc<AppState>.
                    Err(broadcast::error::RecvError::Closed) => break,
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
    client_tx: &UnboundedSender<String>,
    data: &Value,
) {
    match data["type"].as_str().unwrap_or("") {
        "ping" => {
            let _ = client_tx.send(json!({ "type": "pong" }).to_string());
        }
        "extract_audio" => start_extract(state, client_tx, data["url"].as_str()).await,
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
        "set_sleep_timer" => {
            let requested = &data["minutes"];
            // Absent and null both mean "no timer"; anything else has to be a
            // whole number of minutes. A field that is present but unusable
            // (negative, fractional, non-numeric) is a malformed request, not a
            // cancellation — reading it as one would silently drop a running
            // timer the user never asked to stop.
            let minutes = if requested.is_null() {
                Some(0)
            } else {
                requested.as_u64()
            };
            match minutes {
                Some(0) => state.cancel_sleep_timer().await,
                Some(minutes) => {
                    // The valid range belongs to set_sleep_timer; a rejected
                    // value leaves any running timer alone, so there is nothing
                    // to broadcast
                    if state.set_sleep_timer(minutes).await.is_none() {
                        return;
                    }
                }
                None => {
                    tracing::warn!("Ignoring set_sleep_timer with unusable minutes: {requested}");
                    return;
                }
            }
            state.broadcast_sleep_timer().await;
        }
        _ => {}
    }
}

/// Kick off a download for the requested URL. Downloads can take minutes, so
/// the work runs on its own task and reports back over client_tx; the select
/// loop must stay free to service broadcasts and further commands meanwhile.
async fn start_extract(
    state: &Arc<AppState>,
    client_tx: &UnboundedSender<String>,
    url: Option<&str>,
) {
    let Some(url) = url else {
        let msg = json!({
            "type": "extract_audio_error",
            "error": "Missing 'url' field",
        });
        let _ = client_tx.send(msg.to_string());
        return;
    };
    // Capture the token before spawning, so a stop received immediately
    // afterward still cancels this request.
    let cancel = state.download_token().await;
    let state = state.clone();
    let tx = client_tx.clone();
    let url = url.to_string();
    tokio::spawn(async move {
        // Playlist URLs are expanded and trigger a batch import.
        let result = match classify_url(&url) {
            UrlKind::Video => match state.extract_audio(&url, &cancel).await {
                Ok(track) => {
                    state.broadcast_tracks();
                    json!({ "type": "extract_audio_result", "track": track })
                }
                Err(DownloadError::Cancelled) => json!({ "type": "extract_audio_cancelled" }),
                Err(e) => json!({ "type": "extract_audio_error", "error": e.to_string() }),
            },
            UrlKind::Playlist(list_id) => match state.import_playlist(&list_id, &cancel).await {
                Ok(info) => json!({
                    "type": "playlist_import_result",
                    "name": info.name,
                    "total": info.total,
                }),
                Err(DownloadError::Cancelled) => json!({ "type": "extract_audio_cancelled" }),
                Err(e) => json!({ "type": "extract_audio_error", "error": e.to_string() }),
            },
            UrlKind::Unknown => json!({
                "type": "extract_audio_error",
                "error": "Could not recognize YouTube URL",
            }),
        };
        let _ = tx.send(result.to_string());
    });
}
