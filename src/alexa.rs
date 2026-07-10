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
            on_audio_event(state, t, &body, &device_id, base_url).await
        }
        _ => speech("すみません、よくわかりませんでした。", true),
    };

    state.broadcast_devices().await;
    resp
}

// ── Launch ──

async fn on_launch(state: &Arc<AppState>, device_id: &str, base_url: &str) -> Value {
    // 保留中コマンドがあれば即再生
    if let Some(cmd) = state.take_pending(device_id).await
        && cmd.action == "play"
    {
        tracing::info!("Auto-playing queued track on {}", tail_chars(device_id, 8));
        return play_directive(state, &cmd.track, device_id, 0, base_url).await;
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
            if let Some(dev) = state.get_device(device_id).await
                && let Some(track) = dev.current_track
            {
                return play_directive(state, &track, device_id, dev.position_ms, base_url)
                    .await;
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
    base_url: &str,
) -> Value {
    let offset = body["request"]["offsetInMilliseconds"]
        .as_u64()
        .unwrap_or(0);
    let token = body["request"]["token"].as_str().unwrap_or("");
    let track_id = token_track_id(token);

    match event_type {
        "AudioPlayer.PlaybackStarted" => {
            tracing::info!("Playback started: {}", tail_chars(device_id, 8));
            // エンキューされた曲が始まった場合に備え、token から現在の曲を反映する
            let mut upd = DeviceUpdate::new().status("playing");
            if let Some(track) = state.get_track(track_id).await {
                upd = upd.track(track);
            }
            state.update_device(device_id, upd).await;
            // 始まったのが Web からキューされた曲なら、pending は役目を終えたので消す
            if state
                .peek_pending(device_id)
                .await
                .is_some_and(|cmd| cmd.track.id == track_id)
            {
                state.clear_pending(device_id).await;
            }
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
            // Web からキューされた曲を優先し、なければ再生モードに従って次の曲を決める。
            // pending はここでは消費しない (再生開始を確認した PlaybackStarted で消す)。
            // ENQUEUE が破棄されても曲を失わず、イベントが再送されても同じ結果になる
            let next = match state.peek_pending(device_id).await {
                Some(cmd) if cmd.action == "play" => Some(cmd.track),
                _ => state.auto_next_track(track_id).await,
            };
            if let Some(track) = next {
                tracing::info!(
                    "Enqueueing next track '{}' on {}",
                    track.title,
                    tail_chars(device_id, 8)
                );
                return play_response(state, &track, base_url, 0, Some(token));
            }
        }
        "AudioPlayer.PlaybackFailed" => {
            let err = &body["request"]["error"];
            // ライブ配信は終了すると CDN の URL が解決できなくなり、Echo の
            // 再接続が失敗して PlaybackFailed が届く。これは正常な終わり方
            // なのでエラー扱いせず、通常の再生終了と同様に次の曲へ進める
            if let Some(track) = state.get_track(track_id).await.filter(|t| t.is_live) {
                tracing::info!(
                    "Live stream '{}' ended on {} ({:?})",
                    track.title,
                    tail_chars(device_id, 8),
                    err
                );
                state
                    .update_device(
                        device_id,
                        DeviceUpdate::new().status("idle").position(0),
                    )
                    .await;
                // 自動続行で別のライブを選ぶと、それも終了済みだった場合に
                // 失敗が連鎖するため、ライブ以外に限る。pending (Web からの
                // 明示的な指示) は一度だけ試す (失敗しても pending は残り、
                // 次回は同一トラック除外で止まるため無限には繰り返さない)
                let next = match state.peek_pending(device_id).await {
                    Some(cmd) if cmd.action == "play" => Some(cmd.track),
                    _ => state.auto_next_track(track_id).await.filter(|t| !t.is_live),
                };
                if let Some(next) = next.filter(|t| t.id != track_id) {
                    return play_directive(state, &next, device_id, 0, base_url).await;
                }
            } else {
                tracing::error!(
                    "Playback failed on {}: {:?}",
                    tail_chars(device_id, 8),
                    err
                );
                state
                    .update_device(device_id, DeviceUpdate::new().status("error"))
                    .await;
            }
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
    state
        .update_device(
            device_id,
            DeviceUpdate::new()
                .status("playing")
                .track(track.clone())
                .position(offset_ms),
        )
        .await;

    play_response(state, track, base_url, offset_ms, None)
}

/// AudioPlayer.Play レスポンスを組み立てる。
/// enqueue_after (直前の token) を渡すと ENQUEUE、なければ REPLACE_ALL。
/// エンキュー時のデバイス状態は更新しない (再生が始まると PlaybackStarted で反映される)
fn play_response(
    state: &Arc<AppState>,
    track: &AudioTrack,
    base_url: &str,
    offset_ms: u64,
    enqueue_after: Option<&str>,
) -> Value {
    // Echo は認証ヘッダを付けられないため、署名付き URL でストリームを認証する。
    // ライブ配信はファイルがないため、CDN の音声を中継する /live を使う
    let endpoint = if track.is_live { "live" } else { "stream" };
    let mut stream_url =
        format!("{}/api/audio/{}/{}", base_url, track.id, endpoint);
    if let Some(secret) = &state.api_token {
        stream_url.push('?');
        stream_url.push_str(&crate::auth::stream_query(secret, &track.id));
    }

    let mut stream = json!({
        "url": stream_url,
        "token": new_token(&track.id),
        "offsetInMilliseconds": offset_ms
    });
    let play_behavior = if let Some(prev) = enqueue_after {
        stream["expectedPreviousToken"] = json!(prev);
        "ENQUEUE"
    } else {
        "REPLACE_ALL"
    };

    json!({
        "version": "1.0",
        "response": {
            "directives": [{
                "type": "AudioPlayer.Play",
                "playBehavior": play_behavior,
                "audioItem": {
                    "stream": stream,
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

/// token は "{track_id}#{発行時刻ミリ秒}" 形式。
/// Alexa は直前と同じ token の ENQUEUE を無視するため (1 曲ループ対策)、再生ごとに一意化する
fn new_token(track_id: &str) -> String {
    let millis = (crate::state::now_f64() * 1000.0) as u64;
    format!("{track_id}#{millis}")
}

/// token からトラック ID 部分を取り出す (YouTube の ID に '#' は含まれない)
fn token_track_id(token: &str) -> &str {
    token.split('#').next().unwrap_or(token)
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
