use crate::state::{AppState, AudioTrack, DeviceUpdate};
use serde_json::{json, Value};
use std::sync::Arc;

/// Alexa スキルリクエストを処理し、レスポンス JSON を返す
pub async fn handle_alexa(state: &Arc<AppState>, body: Value, base_url: &str) -> Value {
    let req = &body["request"];
    let req_type = req["type"].as_str().unwrap_or("");

    let device_id = body["context"]["System"]["device"]["deviceId"]
        .as_str()
        .unwrap_or("unknown-device")
        .to_string();

    let short_id = tail_chars(&device_id, 6);
    let name = format!("Echo-{short_id}");
    state.register_device(&device_id, &name).await;

    let resp = match req_type {
        "LaunchRequest" => on_launch(state, &device_id, base_url).await,
        "IntentRequest" => on_intent(state, &body, &device_id, base_url).await,
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

async fn on_launch(state: &Arc<AppState>, device_id: &str, base_url: &str) -> Value {
    // 保留中コマンドがあれば即再生
    if let Some(cmd) = state.take_pending(device_id).await {
        if cmd.action == "play" {
            tracing::info!("Auto-playing queued track on {}", tail_chars(device_id, 8));
            return play_directive(state, &cmd.track, device_id, 0, base_url).await;
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

async fn on_intent(state: &Arc<AppState>, body: &Value, device_id: &str, base_url: &str) -> Value {
    let intent = body["request"]["intent"]["name"]
        .as_str()
        .unwrap_or("");

    match intent {
        "PlayFromWebIntent" => {
            if let Some(cmd) = state.take_pending(device_id).await {
                return play_directive(state, &cmd.track, device_id, 0, base_url).await;
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
            if let Some(dev) = state.get_device(device_id).await {
                if let Some(track) = dev.current_track {
                    return play_directive(state, &track, device_id, dev.position_ms, base_url)
                        .await;
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
            tracing::info!("Playback started: {}", tail_chars(device_id, 8));
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
            tracing::error!("Playback failed on {}: {:?}", tail_chars(device_id, 8), err);
            state
                .update_device(device_id, DeviceUpdate::new().status("error"))
                .await;
        }
        _ => {}
    }

    json!({ "version": "1.0", "response": { "shouldEndSession": true } })
}

// ── ヘルパー ──

async fn play_directive(
    state: &Arc<AppState>,
    track: &AudioTrack,
    device_id: &str,
    offset_ms: u64,
    base_url: &str,
) -> Value {
    let stream_url = format!("{}/api/audio/{}/stream", base_url, track.id);

    state
        .update_device(
            device_id,
            DeviceUpdate::new()
                .status("playing")
                .track(track.clone())
                .position(offset_ms),
        )
        .await;

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

fn tail_chars(s: &str, n: usize) -> &str {
    let skip = s.chars().count().saturating_sub(n);
    let offset = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
    &s[offset..]
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
