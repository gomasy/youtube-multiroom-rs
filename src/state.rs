use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::{broadcast, RwLock};

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
    #[serde(skip)]
    pub file_path: String,
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
pub struct ExtractRequest {
    pub url: String,
}

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
    pub tracks: RwLock<HashMap<String, AudioTrack>>,
    pub devices: RwLock<HashMap<String, DeviceState>>,
    pub pending: RwLock<HashMap<String, PendingCommand>>,
    pub tx: broadcast::Sender<String>,
    pub base_url: String,
    pub cache_dir: PathBuf,
    pub api_token: Option<String>,
}

impl AppState {
    pub fn new(base_url: String, api_token: Option<String>) -> Arc<Self> {
        let (tx, _) = broadcast::channel::<String>(256);
        let cache_dir = std::env::current_dir()
            .unwrap_or_default()
            .join("audio_cache");
        std::fs::create_dir_all(&cache_dir).ok();

        Arc::new(Self {
            tracks: RwLock::new(HashMap::new()),
            devices: RwLock::new(HashMap::new()),
            pending: RwLock::new(HashMap::new()),
            tx,
            base_url,
            cache_dir,
            api_token,
        })
    }

    // ── 音声取得 ──

    pub async fn extract_audio(&self, url: &str) -> Result<AudioTrack, String> {
        let video_id =
            extract_video_id(url).ok_or("YouTube の URL を認識できませんでした")?;

        // キャッシュ確認
        {
            let tracks = self.tracks.read().await;
            if let Some(track) = tracks.get(&video_id) {
                if Path::new(&track.file_path).exists() {
                    tracing::info!("Cache hit: {}", video_id);
                    return Ok(track.clone());
                }
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
            .map_err(|e| format!("yt-dlp 実行エラー: {e}"))?;

        if !meta_out.status.success() {
            let err: String = String::from_utf8_lossy(&meta_out.stderr)
                .chars()
                .take(300)
                .collect();
            return Err(format!("メタデータ取得に失敗: {err}"));
        }

        let meta: Value = serde_json::from_slice(&meta_out.stdout)
            .map_err(|e| format!("メタデータ解析エラー: {e}"))?;

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
            .map_err(|e| format!("ダウンロードエラー: {e}"))?;

        if !dl_out.status.success() {
            let err: String = String::from_utf8_lossy(&dl_out.stderr)
                .chars()
                .take(300)
                .collect();
            return Err(format!("音声ダウンロードに失敗: {err}"));
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
            file_path: output_str,
        };

        self.tracks.write().await.insert(video_id, track.clone());
        tracing::info!("Ready: {} ({}s)", track.title, track.duration);
        Ok(track)
    }

    pub async fn get_track(&self, id: &str) -> Option<AudioTrack> {
        self.tracks.read().await.get(id).cloned()
    }

    pub async fn remove_track(&self, id: &str) -> Option<AudioTrack> {
        let track = self.tracks.write().await.remove(id)?;
        let _ = tokio::fs::remove_file(&track.file_path).await;
        self.pending.write().await.retain(|_, cmd| cmd.track.id != id);

        let mut devices = self.devices.write().await;
        for dev in devices.values_mut() {
            if dev.current_track.as_ref().is_some_and(|t| t.id == id) {
                dev.current_track = None;
                dev.status = "idle".to_string();
            }
        }
        drop(devices);

        Some(track)
    }

    pub async fn list_tracks(&self) -> Vec<AudioTrack> {
        self.tracks.read().await.values().cloned().collect()
    }

    pub async fn tracks_json(&self) -> Value {
        let tracks = self.tracks.read().await;
        let map: HashMap<String, Value> = tracks
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!(map)
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

    pub async fn broadcast_tracks(&self) {
        let msg = json!({
            "type": "tracks_update",
            "tracks": self.tracks_json().await,
        });
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
