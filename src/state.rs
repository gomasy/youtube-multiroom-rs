use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::{Mutex, broadcast};
use tokio::time;

const REDIS_KEY_TRACKS: &str = "youtube:tracks";
/// トラックの表示・再生順 (先頭が一覧の先頭) を保持するリスト
const REDIS_KEY_TRACKS_ORDER: &str = "youtube:tracks_order";
const REDIS_KEY_DEVICES: &str = "youtube:devices";
/// 再生終了時の挙動 ("loop" | "shuffle" | "off") を保持するキー
const REDIS_KEY_PLAYBACK_MODE: &str = "youtube:playback_mode";

const PLAYBACK_MODES: [&str; 3] = ["loop", "shuffle", "off"];
const DEFAULT_PLAYBACK_MODE: &str = "off";
/// pending コマンドのキー接頭辞 (デバイスごとに youtube:pending:{device_id})
const REDIS_PENDING_PREFIX: &str = "youtube:pending";

/// キューされた再生コマンドの有効期限 (秒) — Redis のキー TTL で失効する
const PENDING_TTL_SECS: u64 = 600;

/// YouTube 動画 ID の形式 (11 文字)
const VIDEO_ID_PATTERN: &str = "[a-zA-Z0-9_-]{11}";

/// audio_cache 復元時のメタデータ再取得 1 件あたりの制限時間 (秒)
const REFETCH_TIMEOUT_SECS: u64 = 60;

/// キャッシュする音声フォーマットの拡張子。AUDIO_MIME と対で保つこと
const AUDIO_EXT: &str = "m4a";
/// stream_audio が返す Content-Type (AUDIO_EXT に対応するコンテナの MIME)
pub const AUDIO_MIME: &str = "audio/mp4";

fn pending_key(device_id: &str) -> String {
    format!("{REDIS_PENDING_PREFIX}:{device_id}")
}

// ════════════════════════════════════════
// データモデル
// ════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrack {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub thumbnail: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub created_at: f64,
    /// API レスポンスには含めない (Redis 保存時のみ to_redis_json が付与)
    #[serde(skip)]
    pub file_path: String,
}

impl AudioTrack {
    fn to_redis_json(&self) -> String {
        let mut v = serde_json::to_value(self).expect("AudioTrack serializes to JSON");
        v["file_path"] = json!(self.file_path);
        v.to_string()
    }

    /// yt-dlp のメタデータ JSON からトラックを組み立てる。
    /// 欠けているフィールドは空値 (タイトルのみ ID) で埋める
    fn from_meta(id: &str, meta: &Value, created_at: f64, file_path: String) -> Self {
        Self {
            id: id.to_string(),
            title: meta["title"].as_str().unwrap_or(id).to_string(),
            thumbnail: meta["thumbnail"].as_str().unwrap_or("").to_string(),
            duration: meta["duration"].as_u64().unwrap_or(0),
            channel: meta["channel"]
                .as_str()
                .or(meta["uploader"].as_str())
                .unwrap_or("")
                .to_string(),
            is_live: meta["is_live"].as_bool().unwrap_or(false),
            created_at,
            file_path,
        }
    }

    fn from_redis_json(s: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(s).ok()?;
        let file_path = v["file_path"].as_str()?.to_string();
        let mut track: Self = serde_json::from_value(v).ok()?;
        track.file_path = file_path;
        Some(track)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub device_id: String,
    pub name: String,
    pub status: String,
    pub current_track: Option<AudioTrack>,
    pub position_ms: u64,
    pub connected: bool,
    pub last_update: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommand {
    pub action: String,
    pub track: AudioTrack,
}

// API リクエスト
#[derive(Deserialize)]
pub struct PlayRequest {
    pub track_id: String,
    pub device_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub track_id: String,
    /// 移動先の全体インデックス (0 始まり、範囲外は末尾に丸める)
    pub new_index: usize,
}

// ════════════════════════════════════════
// DeviceUpdate ビルダー
// ════════════════════════════════════════

#[derive(Default)]
pub struct DeviceUpdate {
    status: Option<String>,
    current_track: Option<AudioTrack>,
    position_ms: Option<u64>,
    name: Option<String>,
}

impl DeviceUpdate {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn status(mut self, s: impl Into<String>) -> Self {
        self.status = Some(s.into());
        self
    }
    pub fn track(mut self, t: AudioTrack) -> Self {
        self.current_track = Some(t);
        self
    }
    pub fn position(mut self, p: u64) -> Self {
        self.position_ms = Some(p);
        self
    }
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
}

// ════════════════════════════════════════
// AppState — 全体の共有状態
// ════════════════════════════════════════

pub struct AppState {
    redis: ConnectionManager,
    pub tx: broadcast::Sender<String>,
    pub cache_dir: PathBuf,
    pub api_token: Option<String>,
    /// audio_cache からのトラック復元が進行中かどうか (多重起動防止)
    restoring: AtomicBool,
    /// youtube:tracks_order の変更を直列化するロック。
    /// reorder の全置換 (読み→書き) と extract/remove の LPUSH/LREM が
    /// 交錯すると更新が失われるため
    order_lock: Mutex<()>,
    /// 同一動画の並行ダウンロードが同じ出力ファイルへ同時に書き込まないよう
    /// 直列化する動画 ID ごとのロック
    extract_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl AppState {
    pub async fn new(
        api_token: Option<String>,
        redis_url: &str,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let (tx, _) = broadcast::channel::<String>(256);
        let cache_dir = std::env::current_dir()
            .unwrap_or_default()
            .join("audio_cache");
        std::fs::create_dir_all(&cache_dir).ok();

        let client = redis::Client::open(redis_url)?;
        let redis = time::timeout(time::Duration::from_secs(5), ConnectionManager::new(client))
            .await
            .map_err(|_| format!("Redis connection timed out ({redis_url})"))??;

        Ok(Arc::new(Self {
            redis,
            tx,
            cache_dir,
            api_token,
            restoring: AtomicBool::new(false),
            order_lock: Mutex::new(()),
            extract_locks: Mutex::new(HashMap::new()),
        }))
    }

    // ── 音声取得 ──

    pub async fn extract_audio(&self, url: &str) -> Result<AudioTrack, String> {
        let video_id = extract_video_id(url).ok_or("Could not recognize YouTube URL")?;

        // 同一動画の並行リクエストを直列化する。後続の呼び出しはロック獲得後の
        // キャッシュ確認で即座に返る
        let lock = {
            let mut locks = self.extract_locks.lock().await;
            locks.entry(video_id.clone()).or_default().clone()
        };
        let guard = lock.lock().await;
        let result = self.extract_audio_locked(&video_id, url).await;
        drop(guard);

        // 待機中の呼び出しがなければエントリを片付ける (2 = マップ + 自分の分)
        let mut locks = self.extract_locks.lock().await;
        if locks
            .get(&video_id)
            .is_some_and(|l| Arc::strong_count(l) <= 2)
        {
            locks.remove(&video_id);
        }

        result
    }

    async fn extract_audio_locked(&self, video_id: &str, url: &str) -> Result<AudioTrack, String> {
        // Redis キャッシュ確認。旧フォーマット (mp3 など) のパスを指す
        // エントリは AUDIO_MIME と食い違うため無視して取り直す
        if let Some(track) = self.get_track(video_id).await {
            if track.is_live {
                tracing::info!("Cache hit (live): {}", video_id);
                return Ok(track);
            }
            let path = Path::new(&track.file_path);
            if path.extension().is_some_and(|ext| ext == AUDIO_EXT) && path.exists() {
                tracing::info!("Cache hit: {}", video_id);
                return Ok(track);
            }
        }

        // メタデータ取得
        tracing::info!("Fetching metadata: {}", video_id);
        let meta = fetch_metadata(url).await?;

        // ライブ配信はファイルとして保存できないため、メタデータのみ登録し
        // 再生時に CDN URL を都度解決する (handlers::live_audio)
        let track = if meta["is_live"].as_bool().unwrap_or(false) {
            tracing::info!("Live stream detected, skipping download: {}", video_id);
            AudioTrack::from_meta(video_id, &meta, now_f64(), String::new())
        } else {
            let output_path = self.cache_dir.join(format!("{video_id}.{AUDIO_EXT}"));
            let output_str = output_path.to_string_lossy().to_string();

            // 音声ダウンロード
            tracing::info!(
                "Downloading: {}",
                meta["title"].as_str().unwrap_or(video_id)
            );

            // AAC ソースを優先して選べば AUDIO_EXT へは再エンコード不要 (remux のみ)
            let format_spec = format!("bestaudio[ext={AUDIO_EXT}]/bestaudio");
            let dl_out = Command::new("yt-dlp")
                .args([
                    "-f",
                    &format_spec,
                    "-x",
                    "--audio-format",
                    AUDIO_EXT,
                    "-o",
                    &output_str,
                    "--no-playlist",
                    "--no-part",
                    url,
                ])
                .output()
                .await
                .map_err(|e| format!("Download error: {e}"))?;

            if !dl_out.status.success() {
                // --no-part のため書きかけのファイルが最終名で残る。復元時に
                // 壊れたトラックとして登録されないよう消しておく
                let _ = tokio::fs::remove_file(&output_path).await;
                return Err(format!(
                    "Failed to download audio: {}",
                    stderr_snippet(&dl_out)
                ));
            }

            AudioTrack::from_meta(video_id, &meta, now_f64(), output_str)
        };

        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn
            .hset(REDIS_KEY_TRACKS, video_id, track.to_redis_json())
            .await;
        // 並び順リストの先頭に追加 (再取得時の重複を避けるため一旦除去)
        {
            let _guard = self.order_lock.lock().await;
            let _: Result<(), _> = conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, video_id).await;
            let _: Result<(), _> = conn.lpush(REDIS_KEY_TRACKS_ORDER, video_id).await;
        }

        tracing::info!("Ready: {} ({}s)", track.title, track.duration);
        Ok(track)
    }

    pub async fn get_track(&self, id: &str) -> Option<AudioTrack> {
        let mut conn = self.redis.clone();
        let json_str: String = conn.hget(REDIS_KEY_TRACKS, id).await.ok()?;
        AudioTrack::from_redis_json(&json_str)
    }

    pub async fn remove_track(&self, id: &str) -> Option<AudioTrack> {
        let track = self.get_track(id).await?;

        // ファイルを先に消す。最後のトラック削除で tracks キーが消滅すると
        // restore_tracks_if_missing が走りうるため、その時点でファイルが
        // 残っていると削除したはずのトラックが復活してしまう
        if !track.file_path.is_empty() {
            let _ = tokio::fs::remove_file(&track.file_path).await;
        }

        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn.hdel(REDIS_KEY_TRACKS, id).await;
        {
            let _guard = self.order_lock.lock().await;
            let _: Result<(), _> = conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, id).await;
        }

        // 削除トラックをキューしている pending コマンドを除去
        let pattern = format!("{REDIS_PENDING_PREFIX}:*");
        let keys: Vec<String> = match conn.scan_match::<_, String>(&pattern).await {
            Ok(mut iter) => {
                let mut keys = Vec::new();
                while let Some(key) = iter.next_item().await {
                    match key {
                        Ok(key) => keys.push(key),
                        Err(e) => {
                            tracing::warn!("Redis error scanning pending commands: {e}");
                            break;
                        }
                    }
                }
                keys
            }
            Err(e) => {
                tracing::warn!("Redis error scanning pending commands: {e}");
                Vec::new()
            }
        };
        for key in keys {
            let json_str: Option<String> = conn.get(&key).await.unwrap_or_default();
            if json_str
                .and_then(|s| serde_json::from_str::<PendingCommand>(&s).ok())
                .is_some_and(|cmd| cmd.track.id == id)
            {
                let _: Result<(), _> = conn.del(&key).await;
            }
        }

        for mut dev in self.all_devices().await.into_values() {
            if dev.current_track.as_ref().is_some_and(|t| t.id == id) {
                dev.current_track = None;
                dev.status = "idle".to_string();
                self.write_device(&dev).await;
            }
        }

        Some(track)
    }

    /// 全トラックを保存済みの並び順で返す。
    /// 並び順リストに無いトラック (並べ替え導入前のデータや復元直後) は
    /// 従来どおり新しい順で末尾に続ける
    pub async fn list_tracks(&self) -> Vec<AudioTrack> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> = conn.hgetall(REDIS_KEY_TRACKS).await.unwrap_or_default();
        let mut by_id: HashMap<String, AudioTrack> = all
            .values()
            .filter_map(|s| AudioTrack::from_redis_json(s))
            .map(|t| (t.id.clone(), t))
            .collect();

        let order: Vec<String> = conn
            .lrange(REDIS_KEY_TRACKS_ORDER, 0, -1)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error reading track order: {e}");
                Vec::new()
            });
        let mut tracks: Vec<AudioTrack> = order.iter().filter_map(|id| by_id.remove(id)).collect();

        let mut rest: Vec<AudioTrack> = by_id.into_values().collect();
        rest.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        tracks.extend(rest);
        tracks
    }

    /// トラックを全体の並びの new_index (0 始まり) に移動して保存する。
    /// 成功時は true、Redis エラー時は false
    pub async fn reorder_track(&self, track_id: &str, new_index: usize) -> bool {
        // 読み→全置換の間に他の変更が割り込むと失われるため直列化する
        let _guard = self.order_lock.lock().await;
        let mut ids: Vec<String> = self.list_tracks().await.into_iter().map(|t| t.id).collect();
        let Some(pos) = ids.iter().position(|id| id == track_id) else {
            return false;
        };
        let id = ids.remove(pos);
        ids.insert(new_index.min(ids.len()), id);

        // 並び順リスト全体を書き換える (件数は高々数百なので都度全置換で十分)
        let mut pipe = redis::pipe();
        pipe.atomic()
            .del(REDIS_KEY_TRACKS_ORDER)
            .rpush(REDIS_KEY_TRACKS_ORDER, &ids);
        let mut conn = self.redis.clone();
        match pipe.query_async::<()>(&mut conn).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error writing track order: {e}");
                false
            }
        }
    }

    /// Redis に youtube:tracks キーが存在しない場合 (初期化直後など) に限り、
    /// audio_cache の m4a ファイル名からメタデータを再取得して登録する。
    /// yt-dlp は 1 件ごとに時間がかかるため復元はバックグラウンドで行い、
    /// 完了後に tracks_update を通知してクライアントに再取得させる
    pub async fn restore_tracks_if_missing(self: &Arc<Self>) {
        let mut conn = self.redis.clone();
        match conn.exists::<_, bool>(REDIS_KEY_TRACKS).await {
            Ok(false) => {}
            Ok(true) => return,
            Err(e) => {
                tracing::warn!("Redis error checking tracks key: {e}");
                return;
            }
        }

        let cached = cached_video_ids(&self.cache_dir);
        if cached.is_empty() {
            return;
        }

        if self
            .restoring
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let state = self.clone();
        tokio::spawn(async move {
            tracing::info!(
                "Tracks key missing: restoring {} track(s) from audio_cache",
                cached.len()
            );
            for (video_id, path) in cached {
                let track = state.refetch_track_metadata(&video_id, &path).await;
                let mut conn = state.redis.clone();
                if let Err(e) = conn
                    .hset::<_, _, _, ()>(REDIS_KEY_TRACKS, &video_id, track.to_redis_json())
                    .await
                {
                    tracing::warn!("Redis error restoring track {video_id}: {e}");
                }
            }
            state.broadcast_tracks().await;
            state.restoring.store(false, Ordering::SeqCst);
            tracing::info!("Track restore finished");
        });
    }

    /// yt-dlp でメタデータのみ再取得する。動画が削除済みなどで取得できない
    /// 場合もファイル自体は再生できるため、ID をタイトルにした最小情報で返す
    async fn refetch_track_metadata(&self, video_id: &str, path: &Path) -> AudioTrack {
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        // yt-dlp が固まると復元全体が止まったままになるため時間を区切る
        let meta = match time::timeout(
            time::Duration::from_secs(REFETCH_TIMEOUT_SECS),
            fetch_metadata(&url),
        )
        .await
        {
            Ok(Ok(meta)) => meta,
            Ok(Err(e)) => {
                tracing::warn!("Metadata refetch failed for {video_id}: {e}");
                Value::Null
            }
            Err(_) => {
                tracing::warn!("Metadata refetch timed out for {video_id}");
                Value::Null
            }
        };

        // 登録順を保つため元ファイルの更新時刻を登録時刻として使う
        AudioTrack::from_meta(
            video_id,
            &meta,
            file_mtime_f64(path),
            path.to_string_lossy().to_string(),
        )
    }

    /// 再生モードに従い、再生終了後に続ける曲を返す ("off" なら None)
    pub async fn auto_next_track(&self, current_id: &str) -> Option<AudioTrack> {
        match self.playback_mode().await.as_str() {
            "loop" => self.next_track(current_id).await,
            "shuffle" => self.random_track(current_id).await,
            _ => None, // "off": 自動再生しない
        }
    }

    /// 保存済みの並び順で現在トラックの次を返す。
    /// 末尾なら先頭に戻り、現在トラックが見つからない (削除済みなど) 場合も先頭を返す
    async fn next_track(&self, current_id: &str) -> Option<AudioTrack> {
        let tracks = self.list_tracks().await;
        let next = match tracks.iter().position(|t| t.id == current_id) {
            Some(i) => tracks.get(i + 1).or_else(|| tracks.first()),
            None => tracks.first(),
        };
        next.cloned()
    }

    /// シャッフル用に現在トラック以外からランダムに 1 曲返す (1 曲しかなければその曲)
    async fn random_track(&self, current_id: &str) -> Option<AudioTrack> {
        let mut tracks = self.list_tracks().await;
        if tracks.len() > 1 {
            tracks.retain(|t| t.id != current_id);
        }
        if tracks.is_empty() {
            return None;
        }
        // 選曲のばらつき程度で十分なので時刻のナノ秒を乱数代わりに使う
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;
        Some(tracks.swap_remove(nanos % tracks.len()))
    }

    /// 指定ページのトラックと総件数を返す (page は 1 始まり)
    pub async fn list_tracks_page(&self, page: usize, per_page: usize) -> (Vec<AudioTrack>, usize) {
        let tracks = self.list_tracks().await;
        let total = tracks.len();
        let start = page.saturating_sub(1).saturating_mul(per_page);
        let items = tracks.into_iter().skip(start).take(per_page).collect();
        (items, total)
    }

    // ── デバイス管理 ──

    async fn write_device(&self, dev: &DeviceState) {
        let json_str = match serde_json::to_string(dev) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to serialize device {}: {e}", dev.device_id);
                return;
            }
        };
        let mut conn = self.redis.clone();
        if let Err(e) = conn
            .hset::<_, _, _, ()>(REDIS_KEY_DEVICES, &dev.device_id, json_str)
            .await
        {
            tracing::warn!("Redis error writing device {}: {e}", dev.device_id);
        }
    }

    pub async fn get_device(&self, device_id: &str) -> Option<DeviceState> {
        let mut conn = self.redis.clone();
        match conn
            .hget::<_, _, Option<String>>(REDIS_KEY_DEVICES, device_id)
            .await
        {
            Ok(Some(s)) => serde_json::from_str(&s).ok(),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("Redis error reading device {device_id}: {e}");
                None
            }
        }
    }

    async fn all_devices(&self) -> HashMap<String, DeviceState> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> =
            conn.hgetall(REDIS_KEY_DEVICES).await.unwrap_or_else(|e| {
                tracing::warn!("Redis error listing devices: {e}");
                HashMap::new()
            });
        all.into_iter()
            .filter_map(|(k, s)| serde_json::from_str(&s).ok().map(|d| (k, d)))
            .collect()
    }

    pub async fn device_ids(&self) -> redis::RedisResult<Vec<String>> {
        let mut conn = self.redis.clone();
        conn.hkeys(REDIS_KEY_DEVICES).await
    }

    pub async fn register_device(&self, device_id: &str, name: &str) -> DeviceState {
        let mut dev = self
            .get_device(device_id)
            .await
            .unwrap_or_else(|| DeviceState {
                device_id: device_id.to_string(),
                name: name.to_string(),
                status: "idle".to_string(),
                current_track: None,
                position_ms: 0,
                connected: true,
                last_update: now_f64(),
            });
        dev.connected = true;
        dev.last_update = now_f64();
        self.write_device(&dev).await;
        dev
    }

    pub async fn update_device(&self, device_id: &str, upd: DeviceUpdate) {
        let Some(mut dev) = self.get_device(device_id).await else {
            return;
        };
        if let Some(s) = upd.status {
            dev.status = s;
        }
        if let Some(t) = upd.current_track {
            dev.current_track = Some(t);
        }
        if let Some(p) = upd.position_ms {
            dev.position_ms = p;
        }
        if let Some(n) = upd.name {
            dev.name = n;
        }
        dev.last_update = now_f64();
        self.write_device(&dev).await;
    }

    pub async fn remove_device(&self, device_id: &str) -> Option<DeviceState> {
        let device = self.get_device(device_id).await?;
        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn.hdel(REDIS_KEY_DEVICES, device_id).await;
        let _: Result<(), _> = conn.del(pending_key(device_id)).await;
        Some(device)
    }

    pub async fn devices_json(&self) -> Value {
        json!(self.all_devices().await)
    }

    // ── ブロードキャスト ──

    /// 接続中の全 WebSocket クライアントへメッセージを送る (購読者ゼロは無視)
    fn broadcast(&self, msg: Value) {
        let _ = self.tx.send(msg.to_string());
    }

    pub async fn broadcast_devices(&self) {
        self.broadcast(json!({
            "type": "device_update",
            "devices": self.devices_json().await,
        }));
    }

    /// トラック一覧の変更をクライアントに通知する (内容は REST で再取得させる)
    pub async fn broadcast_tracks(&self) {
        self.broadcast(json!({ "type": "tracks_update" }));
    }

    // ── 再生モード ──

    /// 再生終了時の挙動を返す。未設定・不正値・Redis エラー時はデフォルト
    pub async fn playback_mode(&self) -> String {
        let mut conn = self.redis.clone();
        match conn.get::<_, Option<String>>(REDIS_KEY_PLAYBACK_MODE).await {
            Ok(Some(m)) if PLAYBACK_MODES.contains(&m.as_str()) => m,
            Ok(_) => DEFAULT_PLAYBACK_MODE.to_string(),
            Err(e) => {
                tracing::warn!("Redis error reading playback mode: {e}");
                DEFAULT_PLAYBACK_MODE.to_string()
            }
        }
    }

    /// 再生モードを保存する。未知の値や Redis エラー時は false
    pub async fn set_playback_mode(&self, mode: &str) -> bool {
        if !PLAYBACK_MODES.contains(&mode) {
            return false;
        }
        let mut conn = self.redis.clone();
        match conn.set::<_, _, ()>(REDIS_KEY_PLAYBACK_MODE, mode).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error writing playback mode: {e}");
                false
            }
        }
    }

    pub async fn broadcast_playback_mode(&self, mode: &str) {
        self.broadcast(json!({
            "type": "playback_mode_update",
            "mode": mode,
        }));
    }

    // ── コマンドキュー ──

    pub async fn queue_play(&self, device_id: &str, track: AudioTrack) {
        let cmd = PendingCommand {
            action: "play".to_string(),
            track: track.clone(),
        };
        let Ok(json_str) = serde_json::to_string(&cmd) else {
            return;
        };
        let mut conn = self.redis.clone();
        if let Err(e) = conn
            .set_ex::<_, _, ()>(pending_key(device_id), json_str, PENDING_TTL_SECS)
            .await
        {
            // キューを保存できなかった場合は queued 表示にしない
            tracing::warn!("Redis error queueing play for {device_id}: {e}");
            return;
        }
        self.update_device(device_id, DeviceUpdate::new().status("queued").track(track))
            .await;
    }

    /// キューされたコマンドを取り出す (GETDEL でアトミックに取得+削除、失効は Redis の TTL 任せ)
    pub async fn take_pending(&self, device_id: &str) -> Option<PendingCommand> {
        let mut conn = self.redis.clone();
        let result = conn
            .get_del::<_, Option<String>>(pending_key(device_id))
            .await;
        Self::parse_pending(device_id, result)
    }

    /// キューされたコマンドを消費せずに参照する (取り出す場合は take_pending)
    pub async fn peek_pending(&self, device_id: &str) -> Option<PendingCommand> {
        let mut conn = self.redis.clone();
        let result = conn.get::<_, Option<String>>(pending_key(device_id)).await;
        Self::parse_pending(device_id, result)
    }

    /// キューされたコマンドを破棄する
    pub async fn clear_pending(&self, device_id: &str) {
        let mut conn = self.redis.clone();
        if let Err(e) = conn.del::<_, ()>(pending_key(device_id)).await {
            tracing::warn!("Redis error clearing pending for {device_id}: {e}");
        }
    }

    fn parse_pending(
        device_id: &str,
        result: redis::RedisResult<Option<String>>,
    ) -> Option<PendingCommand> {
        let json_str = match result {
            Ok(Some(s)) => s,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!("Redis error reading pending for {device_id}: {e}");
                return None;
            }
        };
        match serde_json::from_str(&json_str) {
            Ok(cmd) => Some(cmd),
            Err(e) => {
                tracing::warn!("Discarding unparsable pending command for {device_id}: {e}");
                None
            }
        }
    }
}

// ════════════════════════════════════════
// ユーティリティ
// ════════════════════════════════════════

/// コマンドの stderr をエラーメッセージ用に先頭 300 文字へ切り詰める
pub fn stderr_snippet(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .chars()
        .take(300)
        .collect()
}

/// yt-dlp でメタデータ JSON を取得する (ダウンロードなし)
async fn fetch_metadata(url: &str) -> Result<Value, String> {
    let out = Command::new("yt-dlp")
        .args(["--dump-json", "--no-download", url])
        .output()
        .await
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "Failed to fetch metadata: {}",
            stderr_snippet(&out)
        ));
    }

    serde_json::from_slice(&out.stdout).map_err(|e| format!("Failed to parse metadata: {e}"))
}

fn extract_video_id(url: &str) -> Option<String> {
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            format!(r"(?:youtube\.com/watch\?.*v=|youtu\.be/)({VIDEO_ID_PATTERN})"),
            format!(r"youtube\.com/embed/({VIDEO_ID_PATTERN})"),
            format!(r"youtube\.com/shorts/({VIDEO_ID_PATTERN})"),
            format!(r"youtube\.com/live/({VIDEO_ID_PATTERN})"),
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    });

    for re in patterns {
        if let Some(caps) = re.captures(url) {
            return caps.get(1).map(|m| m.as_str().to_string());
        }
    }
    None
}

/// audio_cache 内の {video_id}.m4a を (video_id, パス) で列挙する
fn cached_video_ids(cache_dir: &Path) -> Vec<(String, PathBuf)> {
    use std::sync::OnceLock;

    static ID_RE: OnceLock<Option<Regex>> = OnceLock::new();
    let Some(id_re) = ID_RE.get_or_init(|| Regex::new(&format!("^{VIDEO_ID_PATTERN}$")).ok())
    else {
        return Vec::new();
    };

    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|ext| ext != AUDIO_EXT) {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            if !id_re.is_match(stem) {
                return None;
            }
            Some((stem.to_string(), path))
        })
        .collect()
}

/// ファイルの更新時刻を UNIX 秒で返す (取得できなければ現在時刻)
fn file_mtime_f64(path: &Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or_else(now_f64)
}

pub fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_id_from_url_variants() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                extract_video_id(url).as_deref(),
                Some("dQw4w9WgXcQ"),
                "failed for {url}"
            );
        }
        assert_eq!(extract_video_id("https://example.com/watch?v=x"), None);
    }

    #[test]
    fn redis_json_roundtrip_preserves_is_live() {
        let track = AudioTrack {
            id: "dQw4w9WgXcQ".into(),
            title: "配信".into(),
            thumbnail: String::new(),
            duration: 0,
            channel: "ch".into(),
            is_live: true,
            created_at: 1.0,
            file_path: String::new(),
        };
        let restored = AudioTrack::from_redis_json(&track.to_redis_json()).unwrap();
        assert!(restored.is_live);
        assert!(restored.file_path.is_empty());
    }

    #[test]
    fn redis_json_without_is_live_defaults_to_false() {
        let legacy = r#"{"id":"dQw4w9WgXcQ","title":"t","thumbnail":"","duration":10,"channel":"","created_at":1.0,"file_path":"/tmp/a.m4a"}"#;
        let track = AudioTrack::from_redis_json(legacy).unwrap();
        assert!(!track.is_live);
    }
}
