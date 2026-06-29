use crate::state::{AppState, AudioTrack, DeviceUpdate};
use serde_json::{json, Value};
use std::sync::Arc;

/// Alexa スキルリクエストを処理し、レスポンス JSON を返す
pub async fn handle_alexa(state: &Arc<AppState>, body: Value) -> Value {
    let req = &body["request"];
    let req_type = req["type"].as_str().unwrap_or("");

    let device_id = body["context"]["System"]["device"]["deviceId"]
        .as_str()
        .unwrap_or("unknown-device")
        .to_string();

    // デバイスを自動登録
    let short_id = &device_id[device_id.len().saturating_sub(6)..];
    let name = format!("Echo-{short_id}");
    state.register_device(&device_id, &name).await;

    let resp = match req_type {
        "LaunchRequest" => on_launch(state, &device_id).await,
        "IntentRequest" => on_intent(state, &body, &device_id).await,
        "SessionEndedRequest" => speech("セッション終了", true),
        t if t.starts_with("AudioPlayer.") => {
            on_audio_event(state, t, &body, &device_id).await
        }
        _ => speech("すみません、よくわかりませんでした。", true),
    };

    state.broadcast_devices().await;
    resp
}

// ── Launch ──

async fn on_launch(state: &Arc<AppState>, device_id: &str) -> Value {
    // 保留中コマンドがあれば即再生
    if let Some(cmd) = state.take_pending(device_id).await {
        if cmd.action == "play" {
            tracing::info!("Auto-playing queued track on {}", &device_id[device_id.len().saturating_sub(8)..]);
            return play_directive(state, &cmd.track, device_id, 0);
        }
    }

    state
        .update_device(device_id, DeviceUpdate::new().status("idle"))
        .await;

    speech(
        "YouTube マルチルームに接続しました。Web 画面から操作できます。",
        false,
    )
}

// ── Intents ──

async fn on_intent(state: &Arc<AppState>, body: &Value, device_id: &str) -> Value {
    let intent = body["request"]["intent"]["name"]
        .as_str()
        .unwrap_or("");

    match intent {
        "PlayFromWebIntent" => {
            if let Some(cmd) = state.take_pending(device_id).await {
                return play_directive(state, &cmd.track, device_id, 0);
            }
            speech(
                "再生する曲がキューされていません。Web 画面で曲を選んでください。",
                true,
            )
        }

        "AMAZON.PauseIntent" => {
            state
                .update_device(device_id, DeviceUpdate::new().status("paused"))
                .await;
            json!({
                "version": "1.0",
                "response": {
                    "directives": [{ "type": "AudioPlayer.Stop" }],
                    "shouldEndSession": true
                }
            })
        }

        "AMAZON.StopIntent" | "AMAZON.CancelIntent" => {
            state
                .update_device(device_id, DeviceUpdate::new().status("stopped"))
                .await;
            json!({
                "version": "1.0",
                "response": {
                    "directives": [{ "type": "AudioPlayer.Stop" }],
                    "shouldEndSession": true
                }
            })
        }

        "AMAZON.ResumeIntent" => {
            let devices = state.devices.read().await;
            if let Some(dev) = devices.get(device_id) {
                if let Some(ref track) = dev.current_track {
                    let track = track.clone();
                    let pos = dev.position_ms;
                    drop(devices);
                    return play_directive(state, &track, device_id, pos);
                }
            }
            speech("再生する曲がありません。", true)
        }

        "AMAZON.HelpIntent" => speech(
            "Web ブラウザの操作画面で YouTube の URL を貼り付け、\
             再生ボタンを押してください。\
             その後、このデバイスで「アレクサ、YouTube プレーヤーを開いて」\
             と言ってください。",
            false,
        ),

        _ => speech("Web 画面から操作してください。", false),
    }
}

// ── AudioPlayer Events ──

async fn on_audio_event(
    state: &Arc<AppState>,
    event_type: &str,
    body: &Value,
    device_id: &str,
) -> Value {
    let offset = body["request"]["offsetInMilliseconds"]
        .as_u64()
        .unwrap_or(0);

    match event_type {
        "AudioPlayer.PlaybackStarted" => {
            tracing::info!(
                "Playback started: {}",
                &device_id[device_id.len().saturating_sub(8)..]
            );
            state
                .update_device(device_id, DeviceUpdate::new().status("playing"))
                .await;
        }
        "AudioPlayer.PlaybackFinished" => {
            state
                .update_device(
                    device_id,
                    DeviceUpdate::new().status("idle").position(0),
                )
                .await;
        }
        "AudioPlayer.PlaybackStopped" => {
            state
                .update_device(device_id, DeviceUpdate::new().position(offset))
                .await;
        }
        "AudioPlayer.PlaybackNearlyFinished" => {
            // 次トラックのキュー対応はここに追加可能
        }
        "AudioPlayer.PlaybackFailed" => {
            let err = &body["request"]["error"];
            tracing::error!(
                "Playback failed on {}: {:?}",
                &device_id[device_id.len().saturating_sub(8)..],
                err
            );
            state
                .update_device(device_id, DeviceUpdate::new().status("error"))
                .await;
        }
        _ => {}
    }

    json!({ "version": "1.0", "response": { "shouldEndSession": true } })
}

// ── ヘルパー ──

fn play_directive(
    state: &Arc<AppState>,
    track: &AudioTrack,
    device_id: &str,
    offset_ms: u64,
) -> Value {
    let stream_url = format!("{}/api/audio/{}/stream", state.base_url, track.id);

    // 状態更新は fire-and-forget (呼び出し元で broadcast する)
    let state = state.clone();
    let did = device_id.to_string();
    let t = track.clone();
    tokio::spawn(async move {
        state
            .update_device(
                &did,
                DeviceUpdate::new()
                    .status("playing")
                    .track(t)
                    .position(offset_ms),
            )
            .await;
    });

    json!({
        "version": "1.0",
        "response": {
            "directives": [{
                "type": "AudioPlayer.Play",
                "playBehavior": "REPLACE_ALL",
                "audioItem": {
                    "stream": {
                        "url": stream_url,
                        "token": track.id,
                        "offsetInMilliseconds": offset_ms
                    },
                    "metadata": {
                        "title": track.title,
                        "subtitle": if track.channel.is_empty() {
                            "YouTube MultiRoom".to_string()
                        } else {
                            track.channel.clone()
                        }
                    }
                }
            }],
            "shouldEndSession": true
        }
    })
}

fn speech(text: &str, end_session: bool) -> Value {
    json!({
        "version": "1.0",
        "response": {
            "outputSpeech": {
                "type": "PlainText",
                "text": text
            },
            "shouldEndSession": end_session
        }
    })
}
