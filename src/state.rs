use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio::time;

const REDIS_KEY_TRACKS: &str = "youtube:tracks";
const REDIS_KEY_DEVICES: &str = "youtube:devices";
/// pending コマンドのキー接頭辞 (デバイスごとに youtube:pending:{device_id})
const REDIS_PENDING_PREFIX: &str = "youtube:pending";

/// キューされた再生コマンドの有効期限 (秒) — Redis のキー TTL で失効する
const PENDING_TTL_SECS: u64 = 600;

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
    pub thumbnail: String,
    pub duration: u64,
    pub channel: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(skip)]
    pub file_path: String,
}

impl AudioTrack {
    fn to_redis_json(&self) -> String {
        json!({
            "id": self.id,
            "title": self.title,
            "thumbnail": self.thumbnail,
            "duration": self.duration,
            "channel": self.channel,
            "created_at": self.created_at,
            "file_path": self.file_path,
        })
        .to_string()
    }

    fn from_redis_json(s: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(s).ok()?;
        Some(Self {
            id: v["id"].as_str()?.to_string(),
            title: v["title"].as_str()?.to_string(),
            thumbnail: v["thumbnail"].as_str().unwrap_or("").to_string(),
            duration: v["duration"].as_u64().unwrap_or(0),
            channel: v["channel"].as_str().unwrap_or("").to_string(),
            created_at: v["created_at"].as_f64().unwrap_or(0.0),
            file_path: v["file_path"].as_str()?.to_string(),
        })
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

// ════════════════════════════════════════
// DeviceUpdate ビルダー
// ════════════════════════════════════════

#[derive(Default)]
pub struct DeviceUpdate {
    pub status: Option<String>,
    pub current_track: Option<AudioTrack>,
    pub position_ms: Option<u64>,
    pub name: Option<String>,
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
}

// ════════════════════════════════════════
// AppState — 全体の共有状態
// ════════════════════════════════════════

pub struct AppState {
    redis: ConnectionManager,
    pub tx: broadcast::Sender<String>,
    pub cache_dir: PathBuf,
    pub api_token: Option<String>,
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
        let redis = time::timeout(
            time::Duration::from_secs(5),
            ConnectionManager::new(client),
        )
        .await
        .map_err(|_| format!("Redis connection timed out ({redis_url})"))??;

        Ok(Arc::new(Self {
            redis,
            tx,
            cache_dir,
            api_token,
        }))
    }

    // ── 音声取得 ──

    pub async fn extract_audio(&self, url: &str) -> Result<AudioTrack, String> {
        let video_id =
            extract_video_id(url).ok_or("Could not recognize YouTube URL")?;

        // Redis キャッシュ確認
        if let Some(track) = self.get_track(&video_id).await {
            if Path::new(&track.file_path).exists() {
                tracing::info!("Cache hit: {}", video_id);
                return Ok(track);
            }
        }

        let output_path = self.cache_dir.join(format!("{video_id}.mp3"));
        let output_str = output_path.to_string_lossy().to_string();

        // メタデータ取得
        tracing::info!("Fetching metadata: {}", video_id);
        let meta_out = Command::new("yt-dlp")
            .args(["--dump-json", "--no-download", url])
            .output()
            .await
            .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

        if !meta_out.status.success() {
            let err: String = String::from_utf8_lossy(&meta_out.stderr)
                .chars()
                .take(300)
                .collect();
            return Err(format!("Failed to fetch metadata: {err}"));
        }

        let meta: Value = serde_json::from_slice(&meta_out.stdout)
            .map_err(|e| format!("Failed to parse metadata: {e}"))?;

        // 音声ダウンロード
        let title = meta["title"].as_str().unwrap_or("Unknown");
        tracing::info!("Downloading: {}", title);

        let dl_out = Command::new("yt-dlp")
            .args([
                "-x",
                "--audio-format",
                "mp3",
                "--audio-quality",
                "5",
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
            let err: String = String::from_utf8_lossy(&dl_out.stderr)
                .chars()
                .take(300)
                .collect();
            return Err(format!("Failed to download audio: {err}"));
        }

        let track = AudioTrack {
            id: video_id.clone(),
            title: title.to_string(),
            thumbnail: meta["thumbnail"].as_str().unwrap_or("").to_string(),
            duration: meta["duration"].as_u64().unwrap_or(0),
            channel: meta["channel"]
                .as_str()
                .or(meta["uploader"].as_str())
                .unwrap_or("")
                .to_string(),
            created_at: now_f64(),
            file_path: output_str,
        };

        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn
            .hset(REDIS_KEY_TRACKS, &video_id, track.to_redis_json())
            .await;

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

        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn.hdel(REDIS_KEY_TRACKS, id).await;

        let _ = tokio::fs::remove_file(&track.file_path).await;

        // 削除トラックをキューしている pending コマンドを除去
        let pattern = format!("{REDIS_PENDING_PREFIX}:*");
        let keys: Vec<String> = match conn.scan_match::<_, String>(&pattern).await {
            Ok(mut iter) => {
                let mut keys = Vec::new();
                while let Some(key) = iter.next_item().await {
                    keys.push(key);
                }
                keys
            }
            Err(e) => {
                tracing::warn!("Redis error scanning pending commands: {e}");
                Vec::new()
            }
        };
        for key in keys {
            let json_str: Option<String> =
                conn.get(&key).await.unwrap_or_default();
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

    /// 全トラックを新しい順で返す
    pub async fn list_tracks(&self) -> Vec<AudioTrack> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> = conn
            .hgetall(REDIS_KEY_TRACKS)
            .await
            .unwrap_or_default();
        let mut tracks: Vec<AudioTrack> = all
            .values()
            .filter_map(|s| AudioTrack::from_redis_json(s))
            .collect();
        tracks.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        tracks
    }

    /// 指定ページのトラックと総件数を返す (page は 1 始まり)
    pub async fn list_tracks_page(
        &self,
        page: usize,
        per_page: usize,
    ) -> (Vec<AudioTrack>, usize) {
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
        let all: HashMap<String, String> = conn
            .hgetall(REDIS_KEY_DEVICES)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error listing devices: {e}");
                HashMap::new()
            });
        all.into_iter()
            .filter_map(|(k, s)| {
                serde_json::from_str(&s).ok().map(|d| (k, d))
            })
            .collect()
    }

    pub async fn device_ids(&self) -> redis::RedisResult<Vec<String>> {
        let mut conn = self.redis.clone();
        conn.hkeys(REDIS_KEY_DEVICES).await
    }

    pub async fn register_device(&self, device_id: &str, name: &str) -> DeviceState {
        let mut dev =
            self.get_device(device_id)
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

    pub async fn broadcast_devices(&self) {
        let msg = json!({
            "type": "device_update",
            "devices": self.devices_json().await,
        });
        let _ = self.tx.send(msg.to_string());
    }

    /// トラック一覧の変更をクライアントに通知する (内容は REST で再取得させる)
    pub async fn broadcast_tracks(&self) {
        let msg = json!({ "type": "tracks_update" });
        let _ = self.tx.send(msg.to_string());
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
        self.update_device(
            device_id,
            DeviceUpdate::new().status("queued").track(track),
        )
        .await;
    }

    /// キューされたコマンドを取り出す (GETDEL でアトミックに取得+削除、失効は Redis の TTL 任せ)
    pub async fn take_pending(&self, device_id: &str) -> Option<PendingCommand> {
        let mut conn = self.redis.clone();
        let json_str = match conn
            .get_del::<_, Option<String>>(pending_key(device_id))
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!("Redis error taking pending for {device_id}: {e}");
                return None;
            }
        };
        match serde_json::from_str(&json_str) {
            Ok(cmd) => Some(cmd),
            Err(e) => {
                tracing::warn!(
                    "Discarding unparsable pending command for {device_id}: {e}"
                );
                None
            }
        }
    }
}

// ════════════════════════════════════════
// ユーティリティ
// ════════════════════════════════════════

fn extract_video_id(url: &str) -> Option<String> {
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?:youtube\.com/watch\?.*v=|youtu\.be/)([a-zA-Z0-9_-]{11})",
            r"youtube\.com/embed/([a-zA-Z0-9_-]{11})",
            r"youtube\.com/shorts/([a-zA-Z0-9_-]{11})",
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

pub fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
