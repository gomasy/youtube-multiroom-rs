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
use tokio::sync::{broadcast, RwLock};
use tokio::time;

const REDIS_KEY_TRACKS: &str = "youtube:tracks";

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

#[derive(Debug, Clone)]
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
    pub connected: Option<bool>,
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
    pub devices: RwLock<HashMap<String, DeviceState>>,
    pub pending: RwLock<HashMap<String, PendingCommand>>,
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
            devices: RwLock::new(HashMap::new()),
            pending: RwLock::new(HashMap::new()),
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
        self.pending.write().await.retain(|_, cmd| cmd.track.id != id);

        let mut devices = self.devices.write().await;
        for dev in devices.values_mut() {
            if dev.current_track.as_ref().is_some_and(|t| t.id == id) {
                dev.current_track = None;
                dev.status = "idle".to_string();
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

    pub async fn register_device(&self, device_id: &str, name: &str) -> DeviceState {
        let mut devices = self.devices.write().await;
        let dev = devices.entry(device_id.to_string()).or_insert(DeviceState {
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
        dev.clone()
    }

    pub async fn update_device(&self, device_id: &str, upd: DeviceUpdate) {
        let mut devices = self.devices.write().await;
        if let Some(dev) = devices.get_mut(device_id) {
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
            if let Some(c) = upd.connected {
                dev.connected = c;
            }
            dev.last_update = now_f64();
        }
    }

    pub async fn remove_device(&self, device_id: &str) -> Option<DeviceState> {
        let device = self.devices.write().await.remove(device_id)?;
        self.pending.write().await.remove(device_id);
        Some(device)
    }

    pub async fn devices_json(&self) -> Value {
        let devices = self.devices.read().await;
        let map: HashMap<String, Value> = devices
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!(map)
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
        self.pending.write().await.insert(
            device_id.to_string(),
            PendingCommand {
                action: "play".to_string(),
                track: track.clone(),
            },
        );
        self.update_device(
            device_id,
            DeviceUpdate::new().status("queued").track(track),
        )
        .await;
    }

    pub async fn take_pending(&self, device_id: &str) -> Option<PendingCommand> {
        self.pending.write().await.remove(device_id)
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
