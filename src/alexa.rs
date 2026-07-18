use crate::state::{
    AppState, AudioTrack, DeviceUpdate, auto_token, is_auto_token, new_token, token_track_id,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// PlaybackFailed 時に同じ曲を再試行する最大回数。これを超えて連続で
/// 失敗した場合のみ error 状態にする
const MAX_PLAYBACK_RETRIES: u32 = 3;

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
        // SessionEndedRequest への応答は Alexa 側で破棄されるため空でよい
        "SessionEndedRequest" => alexa_response(json!({})),
        t if t.starts_with("AudioPlayer.") => {
            on_audio_event(state, t, &body, &device_id, base_url).await
        }
        t if t.starts_with("PlaybackController.") => {
            on_playback_controller(state, t, &body, &device_id, base_url).await
        }
        _ => speech("すみません、よくわかりませんでした。", true),
    };

    state.broadcast_devices().await;
    resp
}

// ── Launch ──

async fn on_launch(state: &Arc<AppState>, device_id: &str, base_url: &str) -> Value {
    // 保留中コマンドか「次に再生」キューがあれば即再生
    if let Some(resp) = start_pending_or_queue(state, device_id, base_url).await {
        return resp;
    }

    // 起動セッションで再生は中断されるため、playing のままにせず推定位置で
    // paused へ落とす (Resume で続きから再生できる)。一時停止中もそのまま
    // 維持し、それ以外は待機へ戻す
    if let Some(dev) = state.get_device(device_id).await {
        let status = if dev.playback_in_progress() {
            "paused"
        } else {
            "idle"
        };
        state
            .update_device(device_id, DeviceUpdate::new().status(status))
            .await;
    }

    speech(
        "YouTube マルチルームに接続しました。Web 画面から操作できます。",
        false,
    )
}

/// pending コマンド、なければ「次に再生」キューの先頭から再生を開始する。
/// どちらも無ければ None。キューからの開始は何も再生していないときに限る
/// (シーク反映などの起動で再生中・一時停止中の曲を破棄しないため)。
/// キューエントリはそのまま token に使い、消費は再生開始を確認した
/// PlaybackStarted が値一致で行う。
///
/// pending_or_queue_next と似ているが役割が違う: こちらは「起動・再開の
/// 応答としてただちに再生を始める」ため pending を take で消費し、
/// 再生中の曲を守るゲートを持つ。次曲の選定 (スキップ・自動継続) は
/// pending_or_queue_next を使うこと
async fn start_pending_or_queue(
    state: &Arc<AppState>,
    device_id: &str,
    base_url: &str,
) -> Option<Value> {
    if let Some(cmd) = state.take_pending(device_id).await
        && cmd.action == "play"
    {
        tracing::info!("Auto-playing queued track on {}", tail_chars(device_id, 8));
        let token = new_token(&cmd.track.id);
        return Some(
            play_directive(state, &cmd.track, device_id, cmd.offset_ms, base_url, token).await,
        );
    }

    let in_progress = state
        .get_device(device_id)
        .await
        .is_some_and(|d| d.playback_in_progress());
    if !in_progress && let Some((entry, track)) = state.peek_queue(device_id).await {
        tracing::info!("Starting next-up track on {}", tail_chars(device_id, 8));
        return Some(play_directive(state, &track, device_id, 0, base_url, entry).await);
    }
    None
}

// ── Intents ──

async fn on_intent(state: &Arc<AppState>, body: &Value, device_id: &str, base_url: &str) -> Value {
    let intent = body["request"]["intent"]["name"].as_str().unwrap_or("");

    match intent {
        "PlayFromWebIntent" => {
            if let Some(resp) = start_pending_or_queue(state, device_id, base_url).await {
                return resp;
            }
            speech(
                "再生する曲がキューされていません。Web 画面で曲を選んでください。",
                true,
            )
        }

        "AMAZON.PauseIntent" => stop_directive(state, device_id, "paused").await,

        "AMAZON.StopIntent" | "AMAZON.CancelIntent" => {
            stop_directive(state, device_id, "stopped").await
        }

        "AMAZON.ResumeIntent" => resume_playback(state, device_id, base_url, true).await,

        "AMAZON.NextIntent" => skip_next(state, body, device_id, base_url, true).await,

        "AMAZON.PreviousIntent" => skip_prev(state, body, device_id, base_url, true).await,

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

/// 現在のトラックを推定位置から再生し直す (Resume インテント / 再生ボタン)。
/// Web からの再生指示や「次に再生」キューが待っていればそちらを優先して開始する
async fn resume_playback(
    state: &Arc<AppState>,
    device_id: &str,
    base_url: &str,
    can_speak: bool,
) -> Value {
    if let Some(resp) = start_pending_or_queue(state, device_id, base_url).await {
        return resp;
    }
    if let Some(dev) = state.get_device(device_id).await
        && let Some(track) = dev.current_track
    {
        // 明示的な再開指示なので、失敗の連続記録をリセットして
        // 再試行の余地を戻す (error 直後の再開が即 error に戻らないように)
        state.clear_playback_failures(device_id).await;
        let token = new_token(&track.id);
        return play_directive(state, &track, device_id, dev.position_ms, base_url, token).await;
    }
    no_track_response(can_speak, "再生する曲がありません。")
}

/// 「次の曲」への明示スキップ。pending → 「次に再生」キュー → 選曲範囲の
/// 並び順 (シャッフル中はランダム) の優先順で選び、即時再生に切り替える。
/// 再生モードが「オフ」でも明示指示なので次の曲へ進む
async fn skip_next(
    state: &Arc<AppState>,
    body: &Value,
    device_id: &str,
    base_url: &str,
    can_speak: bool,
) -> Value {
    // 明示的な操作なので失敗の連続記録をリセットする
    state.clear_playback_failures(device_id).await;
    let current_token = playing_context_token(state, body, device_id).await;

    let next = match pending_or_queue_next(state, device_id, &current_token).await {
        Ok(Some(next)) => Some(next),
        Ok(None) => state
            .skip_next_track(token_track_id(&current_token))
            .await
            .map(|track| {
                let token = new_token(&track.id);
                (track, 0, token)
            }),
        // キューの状態を確認できないときは安全側 (何も切り替えない) に倒す
        Err(()) => None,
    };
    match next {
        Some((track, offset_ms, token)) => {
            play_directive(state, &track, device_id, offset_ms, base_url, token).await
        }
        None => no_track_response(can_speak, "次の曲がありません。"),
    }
}

/// 「前の曲」への明示スキップ (選曲範囲の並び順で前へ、先頭は末尾へ折り返し)
async fn skip_prev(
    state: &Arc<AppState>,
    body: &Value,
    device_id: &str,
    base_url: &str,
    can_speak: bool,
) -> Value {
    state.clear_playback_failures(device_id).await;
    let current_token = playing_context_token(state, body, device_id).await;
    match state.skip_prev_track(token_track_id(&current_token)).await {
        Some(track) => {
            let token = new_token(&track.id);
            play_directive(state, &track, device_id, 0, base_url, token).await
        }
        None => no_track_response(can_speak, "前の曲がありません。"),
    }
}

// ── PlaybackController Events ──

/// Echo Show のタッチ操作やリモコンの物理ボタンによる再生操作。
/// 応答に音声は含められず、AudioPlayer ディレクティブのみ有効
async fn on_playback_controller(
    state: &Arc<AppState>,
    event_type: &str,
    body: &Value,
    device_id: &str,
    base_url: &str,
) -> Value {
    match event_type {
        "PlaybackController.PlayCommandIssued" => {
            resume_playback(state, device_id, base_url, false).await
        }
        "PlaybackController.PauseCommandIssued" => stop_directive(state, device_id, "paused").await,
        "PlaybackController.NextCommandIssued" => {
            skip_next(state, body, device_id, base_url, false).await
        }
        "PlaybackController.PreviousCommandIssued" => {
            skip_prev(state, body, device_id, base_url, false).await
        }
        _ => alexa_response(json!({})),
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
            // 自動選曲 (ループ/シャッフル) で ENQUEUE した曲は、開始までに
            // モードが「オフ」へ戻されていたら続けない。NearlyFinished は
            // 再生開始直後に届くことが多く、ENQUEUE 時点のモード判定だけでは
            // 曲中のモード変更が反映されないため、開始時に再確認して止める
            // (playback_mode_is_off は Redis エラー時に止めない側へ倒れる)
            if is_auto_token(token) && state.playback_mode_is_off().await {
                tracing::info!(
                    "Stopping auto-continued track on {} (playback mode is off)",
                    tail_chars(device_id, 8)
                );
                return stop_directive(state, device_id, "idle").await;
            }
            tracing::info!("Playback started: {}", tail_chars(device_id, 8));
            // エンキューされた曲が始まった場合に備え、token から現在の曲を反映する。
            // 開始位置も反映する (シーク付き ENQUEUE では 0 以外から始まる)
            let mut upd = DeviceUpdate::new().status("playing").position(offset);
            if let Some(track) = state.get_track(track_id).await {
                upd = upd.track(track);
            }
            state.update_device(device_id, upd).await;
            // 始まったのが Web からキューされた曲なら、pending は役目を終えたので消す。
            // 開始位置も比較し、ディレクティブ発行から再生開始までの間に届いた
            // 新しいシーク指示 (開始位置が異なる) を誤って消さないようにする
            if state
                .peek_pending(device_id)
                .await
                .is_some_and(|cmd| cmd.track.id == track_id && cmd.offset_ms == offset)
            {
                state.clear_pending(device_id).await;
            }
            // 「次に再生」キューから始まった曲なら token がエントリとして
            // 残っているので値一致で取り除く (pending や自動選曲の token は
            // キューに存在しないため何も起きない)。ここまで消費を遅らせる
            // ことで、ENQUEUE が破棄されても曲を失わず、イベントが再送
            // されても二重に消費しない
            state.remove_queue_entry(device_id, token).await;
        }
        "AudioPlayer.PlaybackFinished" => {
            // 最後まで再生できたので失敗の連続記録をリセットする
            state.clear_playback_failures(device_id).await;
            state
                .update_device(device_id, DeviceUpdate::new().status("idle").position(0))
                .await;
        }
        "AudioPlayer.PlaybackStopped" => {
            // Stop ディレクティブへの応答のほか、別コンテンツの再生開始など
            // 外部要因で止まったときも届く。playing のまま放置すると
            // クライアントの推定位置が進み続けるため paused へ落とす
            state.pause_if_playing(device_id, offset).await;
        }
        "AudioPlayer.PlaybackNearlyFinished" => {
            // pending はここでは消費しない (再生開始を確認した PlaybackStarted で消す)。
            // ENQUEUE が破棄されても曲を失わず、イベントが再送されても同じ結果になる
            let next = queued_or_auto_next(state, device_id, token, true).await;
            if let Some((track, offset_ms, next_token)) = next {
                tracing::info!(
                    "Enqueueing next track '{}' on {}",
                    track.title,
                    tail_chars(device_id, 8)
                );
                return play_response(state, &track, base_url, offset_ms, Some(token), next_token);
            }
        }
        "AudioPlayer.PlaybackFailed" => {
            let err = &body["request"]["error"];
            // 失敗した再生が「次に再生」キュー由来ならエントリを消費し、
            // 再生できない項目 (終了済みライブなど) がキュー先頭に残り続けて
            // 後続の曲を塞がないようにする
            state.remove_queue_entry(device_id, token).await;
            let track = state.get_track(track_id).await;
            // ライブ配信は終了すると CDN の URL が解決できなくなり、Echo の
            // 再接続が失敗して PlaybackFailed が届く。これは正常な終わり方
            // なのでエラー扱いせず、通常の再生終了と同様に次の曲へ進める
            if let Some(track) = track.as_ref().filter(|t| t.is_live) {
                tracing::info!(
                    "Live stream '{}' ended on {} ({:?})",
                    track.title,
                    tail_chars(device_id, 8),
                    err
                );
                state
                    .update_device(device_id, DeviceUpdate::new().status("idle").position(0))
                    .await;
                // 自動続行で別のライブを選ぶと、それも終了済みだった場合に
                // 失敗が連鎖するため、ライブ以外に限る。pending (Web からの
                // 明示的な指示) は一度だけ試す (失敗しても pending は残り、
                // 次回は同一トラック除外で止まるため無限には繰り返さない)
                let next = queued_or_auto_next(state, device_id, token, false).await;
                if let Some((next, offset_ms, next_token)) = next.filter(|(t, ..)| t.id != track_id)
                {
                    return play_directive(
                        state, &next, device_id, offset_ms, base_url, next_token,
                    )
                    .await;
                }
            } else {
                // ネットワーク断などの一時的な失敗に備えて数回まで再生し直し、
                // 再試行できない・し尽くした場合のみエラー扱いにする
                if let Some(resp) =
                    retry_playback(state, body, device_id, base_url, token, track, err).await
                {
                    return resp;
                }
                tracing::error!("Playback failed on {}: {:?}", tail_chars(device_id, 8), err);
                state
                    .update_device(device_id, DeviceUpdate::new().status("error"))
                    .await;
            }
        }
        _ => {}
    }

    alexa_response(json!({ "shouldEndSession": true }))
}

// ── ヘルパー ──

/// pending コマンド → 「次に再生」キュー → 再生モードの優先順で、
/// 次に再生する曲を (トラック, 開始位置ミリ秒, AudioPlayer token) で返す。
/// キュー由来の再生はエントリ自体を token に使い、PlaybackStarted /
/// PlaybackFailed が値一致でエントリを消費できるようにする。
/// allow_live_auto が false のとき自動選曲からライブ配信を除外する
/// (pending とキューは Web からの明示的な指示なので除外しない)
async fn queued_or_auto_next(
    state: &Arc<AppState>,
    device_id: &str,
    current_token: &str,
    allow_live_auto: bool,
) -> Option<(AudioTrack, u64, String)> {
    match pending_or_queue_next(state, device_id, current_token).await {
        Ok(Some(next)) => return Some(next),
        Ok(None) => {}
        // キューの状態を確認できないときは安全側 (自動選曲もしない) に倒す
        Err(()) => return None,
    }

    state
        .auto_next_track(token_track_id(current_token))
        .await
        .filter(|t| allow_live_auto || !t.is_live)
        .map(|t| {
            let token = auto_token(&t.id);
            (t, 0, token)
        })
}

/// pending コマンド → 「次に再生」キューの優先順で次の再生候補を返す
/// (再生モード由来の自動選曲は含まない)。候補が無ければ Ok(None)、
/// Redis エラーで状態を確認できなければ Err。pending は消費しない
/// (REPLACE_ALL/ENQUEUE どちらの経路でも PlaybackStarted が消す)
async fn pending_or_queue_next(
    state: &Arc<AppState>,
    device_id: &str,
    current_token: &str,
) -> Result<Option<(AudioTrack, u64, String)>, ()> {
    if let Some(cmd) = state.peek_pending(device_id).await
        && cmd.action == "play"
    {
        let token = new_token(&cmd.track.id);
        return Ok(Some((cmd.track, cmd.offset_ms, token)));
    }

    // 再生中の曲のエントリが先頭に残っていたら (PlaybackStarted での消費が
    // Redis エラーで漏れた場合)、ここで取り除いてから次を見る
    while let Some((entry, track)) = state.peek_queue(device_id).await {
        if entry == current_token {
            if !state.remove_queue_entry(device_id, &entry).await {
                return Err(());
            }
            continue;
        }
        return Ok(Some((track, 0, entry)));
    }
    Ok(None)
}

/// 現在の再生対象を指す token を求める。リクエストの AudioPlayer コンテキスト
/// (一時停止・停止直後でも直前の再生を指す) を優先し、無ければデバイス状態の
/// 現在トラック ID で代用する
async fn playing_context_token(state: &Arc<AppState>, body: &Value, device_id: &str) -> String {
    if let Some(token) = body["context"]["AudioPlayer"]["token"]
        .as_str()
        .filter(|t| !t.is_empty())
    {
        return token.to_string();
    }
    state
        .get_device(device_id)
        .await
        .and_then(|d| d.current_track)
        .map(|t| t.id)
        .unwrap_or_default()
}

/// 再生を切り替えられないときの応答。PlaybackController 由来 (can_speak =
/// false) は仕様上音声を返せないため空の応答にする
fn no_track_response(can_speak: bool, text: &str) -> Value {
    if can_speak {
        speech(text, true)
    } else {
        alexa_response(json!({}))
    }
}

/// PlaybackFailed への再試行ディレクティブを組み立てる。トラックを解決
/// できない、再試行すべき状況でない、連続失敗が MAX_PLAYBACK_RETRIES を
/// 超えた、のいずれかなら None (呼び出し元がエラー扱いにする)
async fn retry_playback(
    state: &Arc<AppState>,
    body: &Value,
    device_id: &str,
    base_url: &str,
    token: &str,
    track: Option<AudioTrack>,
    err: &Value,
) -> Option<Value> {
    let track = track?;
    let cps = &body["request"]["currentPlaybackState"];
    let failed_current = cps["token"].as_str() == Some(token);

    // 失敗したのが ENQUEUE 済みの次曲 (failed_current でない) で、いま何も
    // 再生していない場合は再試行しない。一時停止・停止中に REPLACE_ALL で
    // 再生し直すと、利用者が止めた再生を勝手に再開してしまう
    if !failed_current && cps["playerActivity"].as_str() != Some("PLAYING") {
        return None;
    }

    // 再開位置は失敗した再生の実測位置。まだ始まっていなかった曲 (ENQUEUE
    // の失敗など) は pending の開始位置 (Web からのシーク指定) を引き継ぎ、
    // シーク位置の喪失と PlaybackStarted での pending 取りこぼしを防ぐ
    let offset_ms = if failed_current {
        cps["offsetInMilliseconds"].as_u64().unwrap_or(0)
    } else {
        state
            .peek_pending(device_id)
            .await
            .filter(|cmd| cmd.action == "play" && cmd.track.id == track.id)
            .map_or(0, |cmd| cmd.offset_ms)
    };

    let failures = state
        .record_playback_failure(device_id, &track.id, offset_ms)
        .await;
    if failures > MAX_PLAYBACK_RETRIES {
        return None;
    }

    tracing::warn!(
        "Playback failed on {} (attempt {failures}/{MAX_PLAYBACK_RETRIES}), \
         retrying '{}' from {offset_ms}ms: {:?}",
        tail_chars(device_id, 8),
        track.title,
        err
    );

    // 自動選曲由来の印は、まだ再生が進んでいない場合のみ引き継ぐ。途中まで
    // 聴いていた曲の復帰は新たな自動継続ではないので、モードが「オフ」に
    // 変わっていても開始時に止めない
    let retry_token = if is_auto_token(token) && offset_ms == 0 {
        auto_token(&track.id)
    } else {
        new_token(&track.id)
    };

    // 別の曲を再生中に ENQUEUE 済みの次曲が失敗した場合は、再生中の曲を
    // 中断しないよう ENQUEUE のまま再試行する
    if !failed_current {
        let current = cps["token"].as_str()?;
        return Some(play_response(
            state,
            &track,
            base_url,
            offset_ms,
            Some(current),
            retry_token,
        ));
    }
    Some(play_directive(state, &track, device_id, offset_ms, base_url, retry_token).await)
}

/// Alexa 応答共通のエンベロープ ("version" / "response") を被せる
fn alexa_response(response: Value) -> Value {
    json!({ "version": "1.0", "response": response })
}

/// デバイス状態を更新しつつ AudioPlayer.Stop ディレクティブを返す
async fn stop_directive(state: &Arc<AppState>, device_id: &str, status: &str) -> Value {
    state
        .update_device(device_id, DeviceUpdate::new().status(status))
        .await;
    alexa_response(json!({
        "directives": [{ "type": "AudioPlayer.Stop" }],
        "shouldEndSession": true
    }))
}

async fn play_directive(
    state: &Arc<AppState>,
    track: &AudioTrack,
    device_id: &str,
    offset_ms: u64,
    base_url: &str,
    token: String,
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

    play_response(state, track, base_url, offset_ms, None, token)
}

/// AudioPlayer.Play レスポンスを組み立てる。
/// enqueue_after (直前の token) を渡すと ENQUEUE、なければ REPLACE_ALL。
/// token はこの再生を識別する値 (キュー由来ならキューエントリそのもの)。
/// エンキュー時のデバイス状態は更新しない (再生が始まると PlaybackStarted で反映される)
fn play_response(
    state: &Arc<AppState>,
    track: &AudioTrack,
    base_url: &str,
    offset_ms: u64,
    enqueue_after: Option<&str>,
    token: String,
) -> Value {
    let stream_url = format!(
        "{base_url}{}",
        crate::auth::stream_path(state.api_token.as_deref(), &track.id, track.is_live)
    );

    let mut stream = json!({
        "url": stream_url,
        "token": token,
        "offsetInMilliseconds": offset_ms
    });
    let play_behavior = if let Some(prev) = enqueue_after {
        stream["expectedPreviousToken"] = json!(prev);
        "ENQUEUE"
    } else {
        "REPLACE_ALL"
    };

    alexa_response(json!({
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
    }))
}

fn tail_chars(s: &str, n: usize) -> &str {
    let skip = s.chars().count().saturating_sub(n);
    let offset = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
    &s[offset..]
}

fn speech(text: &str, end_session: bool) -> Value {
    alexa_response(json!({
        "outputSpeech": {
            "type": "PlainText",
            "text": text
        },
        "shouldEndSession": end_session
    }))
}
