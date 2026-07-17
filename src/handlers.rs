use crate::alexa::handle_alexa;
use crate::state::{
    AUDIO_MIME, AppState, AudioTrack, DeviceUpdate, PlayRequest, ReorderRequest, SeekRequest,
    run_yt_dlp,
};
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

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
// 音声 API
// ════════════════════════════════════════

/// GET /api/audio/:id/stream
///
/// ファイルはメモリに読み込まず、Range に応じて seek + ストリーミングで返す
/// (Echo は再生中に Range リクエストを繰り返すため)
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
/// ライブ配信はファイルとして保存できないため、yt-dlp で CDN の HLS URL を
/// 都度解決し、ffmpeg で音声 (AAC) のみを抜き出して ADTS ストリームとして
/// 中継する。Echo は映像入りの muxed HLS を再生できないため、リダイレクト
/// ではなくサーバー側で音声を分離する必要がある。音声はコーデックコピー
/// (再エンコードなし) のため CPU 負荷は小さい
pub async fn live_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Response> {
    let track = track_or_404(&state, &audio_id).await?;

    if !track.is_live {
        return Err(AppError::bad_request("Track is not a live stream"));
    }

    let url = format!("https://www.youtube.com/watch?v={audio_id}");
    // ffmpeg が扱いやすい HLS を最優先する。ライブ配信は音声のみの
    // フォーマットが提供されないことが多く、その場合は最低ビットレートの
    // muxed HLS (映像+音声) にフォールバックする。
    // acodec も一緒に取得し、AAC 以外を掴んだ場合の再エンコード判定に使う。
    // Echo はレスポンスを長く待てないため制限時間は短めに取る
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

    // 出力は URL (DASH では複数行) → acodec の順。先頭の URL のみ使う
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

    // AAC ならコンテナ載せ替えのみで済むが、それ以外 (Opus など) は ADTS に
    // 格納できないため AAC へ再エンコードする。muxed HLS では acodec が
    // "unknown" のこともあり、その場合も安全側 (再エンコード) に倒す
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

    // Echo が切断するとレスポンスボディと共に stdout パイプが閉じ、
    // ffmpeg は EPIPE で自然終了する。ゾンビ化しないようここで回収し、
    // エラー出力があれば原因調査のためログに残す
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

fn parse_byte_range(header: &str, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;
    let (start, end) = if start_str.is_empty() {
        // サフィックス範囲 (bytes=-N): 末尾 N バイト
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
/// yt-dlp の ytsearch で YouTube を検索し、/api/tracks と同じ形の軽量な
/// メタデータ一覧を返す。--flat-playlist で各動画ページの解決を省き、
/// 応答を数秒に収める
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

    // 出力は 1 行 1 動画の JSON
    let results: Vec<AudioTrack> = String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| search_entry(&v))
        .collect();

    Ok(Json(json!({ "results": results })))
}

/// yt-dlp の flat-playlist エントリを検索結果用の AudioTrack に変換する。
/// AudioTrack を経由することで /api/tracks とのワイヤ形式の一致を
/// コンパイラに保証させる (file_path は serde(skip) なので露出しない)
fn search_entry(v: &Value) -> Option<AudioTrack> {
    let id = v["id"].as_str()?;
    Some(AudioTrack {
        id: id.to_string(),
        title: v["title"].as_str().unwrap_or(id).to_string(),
        // flat エントリのサムネイルは有無・形式が揺れるため既知の URL 形式で組み立てる
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
}

/// GET /api/tracks?page=1&per_page=10
pub async fn list_tracks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TracksQuery>,
) -> Json<Value> {
    // Redis 初期化などでトラック情報が消えていたら audio_cache から復元
    state.restore_tracks_if_missing().await;

    let per_page = query.per_page.unwrap_or(10).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let (tracks, total) = state.list_tracks_page(page, per_page).await;
    Json(json!({
        "tracks": tracks,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
}

/// POST /api/tracks/reorder
pub async fn reorder_track(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReorderRequest>,
) -> AppResult<Json<Value>> {
    track_or_404(&state, &req.track_id).await?;
    if !state.reorder_track(&req.track_id, req.new_index).await {
        return Err(AppError::internal("Failed to save track order"));
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
    let track = track_or_404(&state, &req.track_id).await?;
    queue_on_devices(&state, track, req.device_ids).await
}

/// POST /api/play-all
pub async fn play_on_all(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &req.track_id).await?;
    let device_ids = state
        .device_ids()
        .await
        .map_err(|e| AppError::internal(format!("Failed to list devices: {e}")))?;
    queue_on_devices(&state, track, device_ids).await
}

async fn track_or_404(state: &AppState, track_id: &str) -> AppResult<AudioTrack> {
    state
        .get_track(track_id)
        .await
        .ok_or_else(|| AppError::not_found("Track not found"))
}

/// トラックを各デバイスの pending キューに積み、デバイス状態を通知する
async fn queue_on_devices(
    state: &AppState,
    track: AudioTrack,
    device_ids: Vec<String>,
) -> AppResult<Json<Value>> {
    for did in &device_ids {
        state.queue_play(did, track.clone(), 0).await;
    }

    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "devices": device_ids,
        "message": "Say 'Alexa, open YouTube Player' on each Echo device"
    })))
}

/// POST /api/queue
///
/// トラックを選択デバイスの「次に再生」キュー末尾に追加する。
/// 現在の曲が終わるとき (PlaybackNearlyFinished) に先頭から消費される
pub async fn queue_next(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlayRequest>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &req.track_id).await?;
    let mut queued = Vec::new();
    for did in &req.device_ids {
        // 未登録デバイスに積むと参照されないキーが残るため登録済みに限る
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
        "message": format!("「{}」を次に再生に追加しました", track.title),
    })))
}

/// DELETE /api/devices/:id/queue/:entry
///
/// エントリ値の一致でキュー項目を 1 件削除する。エントリは一意なので、
/// デバイス側の消費と競合しても別の曲を消すことはない
pub async fn remove_queue_item(
    State(state): State<Arc<AppState>>,
    Path((device_id, entry)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let removed = state.remove_queue_entry(&device_id, &entry).await;
    // 見つからない場合もクライアントの表示が古い可能性があるため最新状態を配る
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
/// デバイスの現在トラックを指定位置から再生するコマンドをキューする。
/// Alexa スキルはサーバーからディレクティブをプッシュできないため、
/// Echo がスキルに接続したタイミング (音声起動または曲の切り替わり) で反映される
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
    // 長さ不明 (メタデータ再取得失敗など) だと丸め先が定まらない。
    // 黙って先頭へ丸めるのではなく拒否する
    if track.duration == 0 {
        return Err(AppError::bad_request("Track duration is unknown"));
    }

    // 終端ちょうどにキューすると再生が即終了するため 1 秒手前までに丸める
    let max_ms = track.duration.saturating_mul(1000).saturating_sub(1000);
    let position_ms = req.position_ms.min(max_ms);
    state.queue_play(&device_id, track, position_ms).await;
    state.broadcast_devices().await;

    Ok(Json(json!({
        "status": "queued",
        "position_ms": position_ms,
        "message": "Say 'Alexa, open YouTube Player' on the Echo device to apply"
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
/// Bearer 認証の対象外のため、Amazon の署名検証でリクエストが本当に
/// Alexa から送られたものであることを確認する。検証失敗時は Amazon の
/// 規定どおり 400 を返す
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

    // broadcast チャンネルはスナップショット作成前に購読する。後から購読すると
    // 作成中に流れた更新 (ダウンロード完了による一覧からの除去など) を
    // 取りこぼし、init の内容が古いまま二度と補正されない
    let mut rx = state.tx.subscribe();

    // 初期状態を送信 (トラック一覧は REST でページ取得させる)。
    // 進行中ダウンロードを含めることで、リロード後もすぐ進捗表示が復元される
    let init_msg = json!({
        "type": "init",
        "devices": state.devices_json().await,
        "playback_mode": state.playback_mode().await,
        "downloads": state.downloads_json().await,
    });
    if socket
        .send(Message::Text(init_msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // クライアント固有メッセージ用チャンネル (extract 結果など)
    let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    loop {
        tokio::select! {
            // サーバー → クライアント (broadcast)
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }

            // サーバー → クライアント (個別応答)
            Some(msg) = client_rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }

            // クライアント → サーバー
            recv = socket.recv() => {
                // Close フレームなしの切断 (None / Err) でも確実にループを
                // 抜ける。パターンマッチで受けると不一致時にこのブランチが
                // 無効化されるだけで、タスクが残留してしまう
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

/// クライアントからの 1 メッセージを処理する。
/// 応答はすべて client_tx に積み、ws_handler の select ループから送信させる
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
            // ダウンロードは長時間かかるため別タスクで行い、結果だけ返す
            let state = state.clone();
            let tx = client_tx.clone();
            let url = url.to_string();
            tokio::spawn(async move {
                let result = match state.extract_audio(&url).await {
                    Ok(track) => {
                        state.broadcast_tracks().await;
                        json!({ "type": "extract_audio_result", "track": track })
                    }
                    Err(e) => json!({ "type": "extract_audio_error", "error": e }),
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
        "rename_device" => {
            if let (Some(did), Some(name)) = (data["device_id"].as_str(), data["name"].as_str()) {
                state
                    .update_device(did, DeviceUpdate::new().name(name))
                    .await;
                state.broadcast_devices().await;
            }
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
        // 内部用の file_path はワイヤ形式に露出しない
        assert!(entry.get("file_path").is_none());
    }

    #[test]
    fn search_entry_fills_missing_fields() {
        // duration 無し (ライブなど)・channel の代わりに uploader
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

        // id が無いエントリは捨てる
        assert!(search_entry(&json!({ "title": "x" })).is_none());
    }

    #[test]
    fn parses_byte_ranges() {
        // 通常範囲・末尾丸め・開始のみ
        assert_eq!(parse_byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_byte_range("bytes=900-1999", 1000), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=500-", 1000), Some((500, 999)));
        // サフィックス範囲は末尾 N バイト
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
        // 複数範囲は未対応 (呼び出し側が 200 全体にフォールバック)
        assert_eq!(parse_byte_range("bytes=0-1,5-6", 1000), None);
    }
}
