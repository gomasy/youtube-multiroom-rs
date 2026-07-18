use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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
/// 「次に再生」キューのキー接頭辞 (デバイスごとに youtube:queue:{device_id})。
/// 各要素は new_token 形式 "{track_id}#{millis}" の一意なエントリで、先頭が
/// 次に再生される。エントリは AudioPlayer の token としてそのまま使われるため、
/// 再生イベントの token と値一致で照合・消費できる
const REDIS_QUEUE_PREFIX: &str = "youtube:queue";
/// 名前付きプレイリストのメタデータ (hash: playlist_id → JSON)
const REDIS_KEY_PLAYLISTS: &str = "youtube:playlists";
/// プレイリスト収録トラック ID リストのキー接頭辞 (youtube:playlist:{id})
const REDIS_PLAYLIST_PREFIX: &str = "youtube:playlist";
/// ループ/シャッフルの選曲範囲にするプレイリスト ID (未設定はライブラリ全体)
const REDIS_KEY_ACTIVE_PLAYLIST: &str = "youtube:active_playlist";

/// プレイリスト名の最大文字数 (表示崩れ・巨大値の保存を防ぐ)
const PLAYLIST_NAME_MAX_CHARS: usize = 100;
/// プレイリスト一括インポートで取り込む最大件数 (ミックスリストなどの
/// 実質無限のプレイリストを際限なく展開しないための上限)
const PLAYLIST_IMPORT_MAX: usize = 100;
/// プレイリストのフラット展開 (メタデータ一覧取得) の制限時間 (秒)
const PLAYLIST_FLAT_TIMEOUT_SECS: u64 = 60;

/// キューされた再生コマンドの有効期限 (秒) — Redis のキー TTL で失効する
const PENDING_TTL_SECS: u64 = 600;

/// 失敗したダウンロードの進捗表示を残す時間 (秒)。リロード直後の
/// クライアントにもエラーが見えるよう、即座には消さない
const DOWNLOAD_ERROR_TTL_SECS: u64 = 60;

/// yt-dlp の進捗行を他の出力と区別するための接頭辞 (--progress-template で付与)
const PROGRESS_PREFIX: &str = "__progress__ ";

/// 再生失敗を「連続」とみなす間隔 (秒)。前回の失敗からこれ以上空いていれば
/// 別件として 1 から数え直す。再試行の失敗は直後 (数秒) に届くため十分長い値
const FAILURE_RESET_SECS: f64 = 60.0;

/// YouTube 動画 ID の形式 (11 文字)
const VIDEO_ID_PATTERN: &str = "[a-zA-Z0-9_-]{11}";

/// メタデータ取得 1 件あたりの制限時間 (秒)。yt-dlp が固まると
/// 抽出やキャッシュ復元が止まったままになるため時間を区切る
const METADATA_TIMEOUT_SECS: u64 = 30;

/// キャッシュする音声フォーマットの拡張子。AUDIO_MIME と対で保つこと
const AUDIO_EXT: &str = "m4a";
/// stream_audio が返す Content-Type (AUDIO_EXT に対応するコンテナの MIME)
pub const AUDIO_MIME: &str = "audio/mp4";

fn pending_key(device_id: &str) -> String {
    format!("{REDIS_PENDING_PREFIX}:{device_id}")
}

fn queue_key(device_id: &str) -> String {
    format!("{REDIS_QUEUE_PREFIX}:{device_id}")
}

fn playlist_key(playlist_id: &str) -> String {
    format!("{REDIS_PLAYLIST_PREFIX}:{playlist_id}")
}

/// AudioPlayer token 兼キューエントリ "{track_id}#{発行時刻ミリ秒}" を生成する。
/// Alexa は直前と同じ token の ENQUEUE を無視するため (1 曲ループ対策)、
/// 再生ごとに一意化する
pub fn new_token(track_id: &str) -> String {
    let millis = (now_f64() * 1000.0) as u64;
    format!("{track_id}#{millis}")
}

/// token / キューエントリからトラック ID 部分を取り出す
/// (YouTube の ID に '#' は含まれない)
pub fn token_track_id(token: &str) -> &str {
    token.split('#').next().unwrap_or(token)
}

/// 自動選曲由来の token に付ける接尾辞 (auto_token / is_auto_token で対に使う)
const AUTO_TOKEN_SUFFIX: &str = "#auto";

/// 自動選曲 (ループ/シャッフル) 由来の再生を示す token を生成する。
/// ENQUEUE 後に再生モードが「オフ」へ戻された場合、ディレクティブは
/// 取り消せないため、再生開始イベントで自動選曲由来と判別して停止する
pub fn auto_token(track_id: &str) -> String {
    format!("{}{AUTO_TOKEN_SUFFIX}", new_token(track_id))
}

/// token が自動選曲由来かどうか
pub fn is_auto_token(token: &str) -> bool {
    token.ends_with(AUTO_TOKEN_SUFFIX)
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

/// 名前付きプレイリスト。収録トラック ID は別リスト (youtube:playlist:{id}) に持つ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub created_at: f64,
}

/// playlists_json のワイヤ形式: メタデータに収録曲数を添えたもの
#[derive(Serialize)]
struct PlaylistJson {
    #[serde(flatten)]
    playlist: Playlist,
    count: usize,
}

/// プレイリスト一括インポートの開始時情報 (取り込みはバックグラウンドで進む)
pub struct PlaylistImportInfo {
    pub name: String,
    pub total: usize,
}

/// reorder_track の結果。並びに無い (未収録・競合する削除) と
/// Redis エラーを呼び出し元が区別できるように分ける
pub enum ReorderOutcome {
    Moved,
    NotInList,
    Failed,
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

impl DeviceState {
    /// 更新内容を適用し、last_update を now に進める。
    /// デバイス状態の変更は必ずこのメソッドを通すこと — position_ms は
    /// 「last_update 時点の位置」を意味するため、last_update だけを進めると
    /// クライアントの推定位置が巻き戻る
    fn apply(&mut self, upd: DeviceUpdate, now: f64) {
        // 位置を伴わない更新 (再登録など) では経過時間ぶん位置を進めて整合を保つ
        if upd.position_ms.is_none() {
            self.advance_position(now);
        }
        if let Some(s) = upd.status {
            self.status = s;
        }
        if let Some(t) = upd.current_track {
            self.current_track = Some(t);
        }
        if let Some(p) = upd.position_ms {
            self.position_ms = p;
        }
        self.last_update = now;
    }

    /// 再生が進行中 (再生中または一時停止中) かどうか。
    /// 起動時に「次に再生」キューを自動開始してよいかの判定などに使う
    pub fn playback_in_progress(&self) -> bool {
        matches!(self.status.as_str(), "playing" | "paused")
    }

    /// 再生中なら last_update からの経過時間ぶん位置を進める (トラック終端でクランプ)
    fn advance_position(&mut self, now: f64) {
        if self.status != "playing" {
            return;
        }
        let elapsed_ms = ((now - self.last_update).max(0.0) * 1000.0) as u64;
        let max_ms = self
            .current_track
            .as_ref()
            .map(|t| t.duration.saturating_mul(1000))
            .filter(|&d| d > 0)
            .unwrap_or(u64::MAX);
        self.position_ms = self.position_ms.saturating_add(elapsed_ms).min(max_ms);
    }
}

/// トラックごとの再生失敗の連続記録。PlaybackFailed の再試行を打ち切る
/// 判定に使う (プロセス内のみ保持し、再起動でリセットされる)
#[derive(Debug, Default)]
struct FailureRecord {
    count: u32,
    last_failure: f64,
    /// 前回失敗した再生位置 (ミリ秒)。位置が前進していれば再試行後の
    /// 再生が成功していた証拠なので「連続失敗」とみなさない
    last_offset: u64,
}

impl FailureRecord {
    /// 失敗を記録し、今回を含む連続失敗回数を返す。前回から
    /// FAILURE_RESET_SECS 以上空いた失敗と、前回より先の位置まで
    /// 進んでから起きた失敗は 1 から数え直す
    fn record(&mut self, offset_ms: u64, now: f64) -> u32 {
        if now - self.last_failure > FAILURE_RESET_SECS || offset_ms > self.last_offset {
            self.count = 0;
        }
        self.count += 1;
        self.last_failure = now;
        self.last_offset = offset_ms;
        self.count
    }
}

/// 「次に再生」キューの 1 項目 (API レスポンス用)
#[derive(Serialize)]
pub struct QueueItem {
    /// キュー内で一意なエントリ ("{track_id}#{millis}")。削除時の指定に使う
    pub entry: String,
    #[serde(flatten)]
    pub track: AudioTrack,
}

/// devices_json のワイヤ形式: デバイス状態に「次に再生」キューを添えたもの。
/// DeviceState 自体に持たせると write_device が Redis へ保存してしまうため分ける
#[derive(Serialize)]
struct DeviceJson {
    #[serde(flatten)]
    device: DeviceState,
    queue: Vec<QueueItem>,
}

/// ダウンロードの進行段階。ワイヤ形式 (小文字) はフロントの
/// DownloadProgress["status"] union と一致させること
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    /// メタデータ取得中 (タイトル・ライブ判定の解決前)
    Metadata,
    Downloading,
    /// yt-dlp の後処理 (m4a への変換) 中
    Processing,
    Error,
}

/// 進行中ダウンロードの進捗。プロセス内のみで保持し、変化を WebSocket で
/// 全クライアントへ配ることで、開始したブラウザ以外 (リロード後を含む) でも
/// 進捗を追えるようにする
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub id: String,
    /// メタデータ取得前は動画 ID で埋める
    pub title: String,
    pub status: DownloadStatus,
    /// ダウンロード済み割合 (0.0–100.0)
    pub percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 開始時刻 (UNIX 秒)。表示順と、エラー片付け時の同一性確認に使う
    pub started_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommand {
    pub action: String,
    pub track: AudioTrack,
    /// 再生開始位置 (ミリ秒)。Web からのシークで 0 以外になる
    #[serde(default)]
    pub offset_ms: u64,
}

// API リクエスト
#[derive(Deserialize)]
pub struct PlayRequest {
    pub track_id: String,
    pub device_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct SeekRequest {
    /// シーク先の再生位置 (ミリ秒、トラック終端手前に丸める)
    pub position_ms: u64,
}

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub track_id: String,
    /// 移動先の全体インデックス (0 始まり、範囲外は末尾に丸める)
    pub new_index: usize,
    /// 並べ替え対象のプレイリスト ID (未指定はライブラリ全体の並び)
    #[serde(default)]
    pub playlist: Option<String>,
}

// ════════════════════════════════════════
// DeviceUpdate ビルダー
// ════════════════════════════════════════

#[derive(Default)]
pub struct DeviceUpdate {
    status: Option<String>,
    current_track: Option<AudioTrack>,
    position_ms: Option<u64>,
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
    /// audio_cache からのトラック復元が進行中かどうか (多重起動防止)
    restoring: AtomicBool,
    /// youtube:tracks_order の変更を直列化するロック。
    /// reorder の全置換 (読み→書き) と extract/remove の LPUSH/LREM が
    /// 交錯すると更新が失われるため
    order_lock: Mutex<()>,
    /// 同一動画の並行ダウンロードが同じ出力ファイルへ同時に書き込まないよう
    /// 直列化する動画 ID ごとのロック
    extract_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// 進行中ダウンロードの進捗 (動画 ID → 進捗)。プロセス内のみ保持し、
    /// 再起動で消える (クライアントは init の一覧で同期し直す)
    downloads: Mutex<HashMap<String, DownloadProgress>>,
    /// デバイス×トラックごとの再生失敗の連続記録 (record_playback_failure を参照)。
    /// トラック別に数えることで、再生中の曲と ENQUEUE 済みの次曲の失敗が
    /// 交互に届いてもカウントが互いをリセットしない
    playback_failures: Mutex<HashMap<String, HashMap<String, FailureRecord>>>,
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
            downloads: Mutex::new(HashMap::new()),
            playback_failures: Mutex::new(HashMap::new()),
        }))
    }

    // ── 音声取得 ──

    pub async fn extract_audio(self: &Arc<Self>, url: &str) -> Result<AudioTrack, String> {
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

    async fn extract_audio_locked(
        self: &Arc<Self>,
        video_id: &str,
        url: &str,
    ) -> Result<AudioTrack, String> {
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

        // 進捗を全クライアントへ配りながら取得する。完了 (成功) で一覧から
        // 外し、失敗はしばらくエラー表示として残す
        self.begin_download(video_id).await;
        let result = self.fetch_and_register(video_id, url).await;
        match &result {
            Ok(_) => self.finish_download(video_id).await,
            Err(e) => self.fail_download(video_id, e).await,
        }
        result
    }

    /// メタデータ取得 → (ライブ以外は) ダウンロード → Redis 登録を行う。
    /// 途中経過は downloads の進捗エントリへ反映する
    async fn fetch_and_register(&self, video_id: &str, url: &str) -> Result<AudioTrack, String> {
        // メタデータ取得
        tracing::info!("Fetching metadata: {}", video_id);
        let meta = fetch_metadata(url).await?;
        let title = meta["title"].as_str().unwrap_or(video_id).to_string();
        let is_live = meta["is_live"].as_bool().unwrap_or(false);
        self.set_download_meta(video_id, &title, is_live).await;

        // ライブ配信はファイルとして保存できないため、メタデータのみ登録し
        // 再生時に CDN URL を都度解決する (handlers::live_audio)
        let track = if is_live {
            tracing::info!("Live stream detected, skipping download: {}", video_id);
            AudioTrack::from_meta(video_id, &meta, now_f64(), String::new())
        } else {
            let output_path = self.cache_dir.join(format!("{video_id}.{AUDIO_EXT}"));
            let output_str = output_path.to_string_lossy().to_string();

            // 音声ダウンロード
            tracing::info!("Downloading: {}", title);
            if let Err(e) = self.run_download(video_id, url, &output_str).await {
                // --no-part のため書きかけのファイルが最終名で残る。復元時に
                // 壊れたトラックとして登録されないよう消しておく
                let _ = tokio::fs::remove_file(&output_path).await;
                return Err(e);
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

    /// yt-dlp で音声をダウンロードする。stdout の進捗行を読み取り、
    /// パーセンテージを進捗エントリへ反映しながら完了を待つ
    async fn run_download(
        &self,
        video_id: &str,
        url: &str,
        output_str: &str,
    ) -> Result<(), String> {
        // AAC ソースを優先して選べば AUDIO_EXT へは再エンコード不要 (remux のみ)
        let format_spec = format!("bestaudio[ext={AUDIO_EXT}]/bestaudio");
        // 進捗を 1 行 1 更新の機械可読な形式で stdout に流させる
        let progress_template = format!("download:{PROGRESS_PREFIX}%(progress._percent_str)s");
        let mut child = Command::new("yt-dlp")
            .args([
                "-f",
                &format_spec,
                "-x",
                "--audio-format",
                AUDIO_EXT,
                "-o",
                output_str,
                "--no-playlist",
                "--no-part",
                "--newline",
                "--progress-template",
                &progress_template,
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Download error: {e}"))?;

        // stderr は別タスクで読み切り、パイプ詰まりで yt-dlp が
        // 止まらないようにする (エラー時のメッセージにのみ使う)
        let stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut buf).await;
            }
            buf
        });

        // 進捗行はバイト列で読んで損失変換する。lines() は UTF-8 でない行で
        // エラーになって読み取りが止まり、パイプが閉じて yt-dlp 自体が
        // EPIPE で落ちてしまう
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf);
                        if let Some(percent) = parse_progress_percent(line.trim_end()) {
                            self.set_download_percent(video_id, percent).await;
                        }
                    }
                }
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|e| format!("Download error: {e}"))?;
        if !status.success() {
            let stderr_buf = stderr_task.await.unwrap_or_default();
            return Err(format!(
                "Failed to download audio: {}",
                snippet(&String::from_utf8_lossy(&stderr_buf))
            ));
        }
        Ok(())
    }

    // ── ダウンロード進捗 ──

    /// 進捗一覧を変更し、変更があれば全クライアントへ通知する。
    /// f は通知が必要な変更を加えたかどうかを返す。ペイロードはロックを
    /// 保持したまま構築し、変更と通知内容のずれや二重ロックを避ける
    async fn update_downloads(
        &self,
        f: impl FnOnce(&mut HashMap<String, DownloadProgress>) -> bool,
    ) {
        let payload = {
            let mut downloads = self.downloads.lock().await;
            if !f(&mut downloads) {
                return;
            }
            Self::downloads_payload(&downloads)
        };
        self.broadcast(json!({ "type": "downloads_update", "downloads": payload }));
    }

    /// エントリを 1 件変更する (未登録なら何もしない)。
    /// f は通知が必要な変更を加えたかどうかを返す
    async fn update_download(&self, video_id: &str, f: impl FnOnce(&mut DownloadProgress) -> bool) {
        self.update_downloads(|downloads| downloads.get_mut(video_id).is_some_and(f))
            .await;
    }

    /// ダウンロードを進捗一覧に登録して通知する (タイトル判明前は動画 ID で表示)
    async fn begin_download(&self, video_id: &str) {
        self.update_downloads(|downloads| {
            downloads.insert(
                video_id.to_string(),
                DownloadProgress {
                    id: video_id.to_string(),
                    title: video_id.to_string(),
                    status: DownloadStatus::Metadata,
                    percent: 0.0,
                    error: None,
                    started_at: now_f64(),
                },
            );
            true
        })
        .await;
    }

    /// メタデータ取得後にタイトルを反映する。ライブ配信はダウンロードしない
    /// (直後に登録完了で一覧から外れる) ため metadata のまま進めない
    async fn set_download_meta(&self, video_id: &str, title: &str, is_live: bool) {
        self.update_download(video_id, |d| {
            d.title = title.to_string();
            if !is_live {
                d.status = DownloadStatus::Downloading;
            }
            true
        })
        .await;
    }

    /// パーセンテージを更新する。yt-dlp は進捗行を高頻度に出すため、
    /// 通知は整数部が変わったときだけ行う。100% 到達後は変換
    /// (yt-dlp の後処理) 中とみなす
    async fn set_download_percent(&self, video_id: &str, percent: f64) {
        self.update_download(video_id, |d| {
            let before = d.percent as u64;
            d.percent = percent.clamp(0.0, 100.0);
            if d.percent >= 100.0 {
                d.status = DownloadStatus::Processing;
            }
            d.percent as u64 != before
        })
        .await;
    }

    /// 完了したダウンロードを進捗一覧から外して通知する
    async fn finish_download(&self, video_id: &str) {
        self.update_downloads(|downloads| downloads.remove(video_id).is_some())
            .await;
    }

    /// 失敗した進捗エントリをエラー表示へ切り替え、TTL 経過後に片付ける
    async fn fail_download(self: &Arc<Self>, video_id: &str, error: &str) {
        let mut started_at = None;
        self.update_download(video_id, |d| {
            d.status = DownloadStatus::Error;
            d.error = Some(error.to_string());
            started_at = Some(d.started_at);
            true
        })
        .await;
        let Some(started_at) = started_at else {
            return;
        };

        let state = self.clone();
        let video_id = video_id.to_string();
        tokio::spawn(async move {
            time::sleep(time::Duration::from_secs(DOWNLOAD_ERROR_TTL_SECS)).await;
            state
                .update_downloads(|downloads| {
                    // 再試行で上書きされた新しいエントリは消さない (started_at で識別)
                    match downloads.get(&video_id) {
                        Some(d) if d.started_at == started_at => {
                            downloads.remove(&video_id).is_some()
                        }
                        _ => false,
                    }
                })
                .await;
        });
    }

    /// 進行中ダウンロードの一覧を開始順で返す (init / downloads_update のワイヤ形式)
    pub async fn downloads_json(&self) -> Value {
        Self::downloads_payload(&*self.downloads.lock().await)
    }

    fn downloads_payload(downloads: &HashMap<String, DownloadProgress>) -> Value {
        let mut list: Vec<&DownloadProgress> = downloads.values().collect();
        list.sort_by(|a, b| a.started_at.total_cmp(&b.started_at));
        json!(list)
    }

    pub async fn get_track(&self, id: &str) -> Option<AudioTrack> {
        self.try_get_track(id).await.ok().flatten()
    }

    /// トラックを取得する。Redis エラー (Err) と未登録 (Ok(None)) を区別して
    /// 返すため、「見つからないなら消してよい」判断が必要な呼び出し元は
    /// こちらを使うこと。パース不能なエントリは未登録扱い
    async fn try_get_track(&self, id: &str) -> redis::RedisResult<Option<AudioTrack>> {
        let mut conn = self.redis.clone();
        let json_str: Option<String> = conn.hget(REDIS_KEY_TRACKS, id).await?;
        Ok(json_str.and_then(|s| AudioTrack::from_redis_json(&s)))
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
            // reorder の全置換 (読み→書き) と交錯すると削除がなかったことに
            // なるため、並び順とプレイリストからの除去も直列化する
            let _guard = self.order_lock.lock().await;
            let _: Result<(), _> = conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, id).await;

            // 全プレイリストの収録リストからも 1 往復でまとめて取り除く
            let playlists = self.playlists().await;
            if !playlists.is_empty() {
                let mut pipe = redis::pipe();
                for playlist in &playlists {
                    pipe.lrem(playlist_key(&playlist.id), 0, id).ignore();
                }
                if let Err(e) = pipe.query_async::<()>(&mut conn).await {
                    tracing::warn!("Redis error removing track {id} from playlists: {e}");
                }
            }
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
            // 削除トラックを各デバイスの「次に再生」キューからも取り除く
            let key = queue_key(&dev.device_id);
            let entries: Vec<String> = conn.lrange(&key, 0, -1).await.unwrap_or_default();
            for entry in entries.iter().filter(|e| token_track_id(e) == id) {
                let _: Result<(), _> = conn.lrem(&key, 0, entry).await;
            }
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

    /// トラックを並びの new_index (0 始まり) に移動して保存する。
    /// playlist_id を指定するとそのプレイリスト内の並び、なければライブラリ全体
    pub async fn reorder_track(
        &self,
        playlist_id: Option<&str>,
        track_id: &str,
        new_index: usize,
    ) -> ReorderOutcome {
        // 読み→全置換の間に他の変更が割り込むと失われるため直列化する
        let _guard = self.order_lock.lock().await;
        let (key, mut ids) = match playlist_id {
            Some(pid) => (playlist_key(pid), self.playlist_track_ids(pid).await),
            None => (
                REDIS_KEY_TRACKS_ORDER.to_string(),
                self.list_tracks().await.into_iter().map(|t| t.id).collect(),
            ),
        };
        let Some(pos) = ids.iter().position(|id| id == track_id) else {
            return ReorderOutcome::NotInList;
        };
        let id = ids.remove(pos);
        ids.insert(new_index.min(ids.len()), id);

        // 並び順リスト全体を書き換える (件数は高々数百なので都度全置換で十分)
        let mut pipe = redis::pipe();
        pipe.atomic().del(&key).rpush(&key, &ids);
        let mut conn = self.redis.clone();
        match pipe.query_async::<()>(&mut conn).await {
            Ok(()) => ReorderOutcome::Moved,
            Err(e) => {
                tracing::warn!("Redis error writing track order: {e}");
                ReorderOutcome::Failed
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
        let meta = match fetch_metadata(&url).await {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!("Metadata refetch failed for {video_id}: {e}");
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

    /// 選曲・一覧の範囲を返す。プレイリスト指定があればその収録順、
    /// なければライブラリ全体の並び順
    pub async fn scoped_tracks(&self, playlist_id: Option<&str>) -> Vec<AudioTrack> {
        match playlist_id {
            Some(pid) => self.list_playlist_tracks(pid).await,
            None => self.list_tracks().await,
        }
    }

    /// 選曲範囲 (アクティブプレイリスト、なければライブラリ全体) のトラックを返す
    async fn active_scope_tracks(&self) -> Vec<AudioTrack> {
        let scope = self.active_playlist().await;
        self.scoped_tracks(scope.as_deref()).await
    }

    /// 再生モードに従い、再生終了後に続ける曲を返す ("off" なら None)。
    /// 選曲はアクティブプレイリストの範囲内で行う
    pub async fn auto_next_track(&self, current_id: &str) -> Option<AudioTrack> {
        match self.playback_mode().await.as_str() {
            "loop" => neighbor_track(&self.active_scope_tracks().await, current_id, 1),
            "shuffle" => random_track_from(self.active_scope_tracks().await, current_id),
            _ => None, // "off": 自動再生しない
        }
    }

    /// 「次の曲」の明示指示で再生する曲を返す。シャッフル中はランダム、
    /// それ以外は範囲内の並び順で次 (モードが「オフ」でも明示指示なので進む)
    pub async fn skip_next_track(&self, current_id: &str) -> Option<AudioTrack> {
        if self.playback_mode().await == "shuffle" {
            random_track_from(self.active_scope_tracks().await, current_id)
        } else {
            neighbor_track(&self.active_scope_tracks().await, current_id, 1)
        }
    }

    /// 「前の曲」の明示指示で再生する曲を返す (範囲内の並び順で前、先頭は末尾へ折り返し)
    pub async fn skip_prev_track(&self, current_id: &str) -> Option<AudioTrack> {
        neighbor_track(&self.active_scope_tracks().await, current_id, -1)
    }

    /// 指定ページのトラックと総件数を返す (page は 1 始まり)。
    /// playlist_id を指定するとそのプレイリストの収録順で返す
    pub async fn list_tracks_page(
        &self,
        playlist_id: Option<&str>,
        page: usize,
        per_page: usize,
    ) -> (Vec<AudioTrack>, usize) {
        let tracks = self.scoped_tracks(playlist_id).await;
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
        let now = now_f64();
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
                last_update: now,
            });
        dev.connected = true;
        dev.apply(DeviceUpdate::new(), now);
        self.write_device(&dev).await;
        dev
    }

    pub async fn update_device(&self, device_id: &str, upd: DeviceUpdate) {
        let Some(mut dev) = self.get_device(device_id).await else {
            return;
        };
        dev.apply(upd, now_f64());
        self.write_device(&dev).await;
    }

    /// 再生停止の実測位置を記録し、playing のままなら paused へ落とす。
    /// Pause/Stop インテントが先に設定した paused/stopped は上書きしない
    pub async fn pause_if_playing(&self, device_id: &str, offset_ms: u64) {
        let Some(mut dev) = self.get_device(device_id).await else {
            return;
        };
        let mut upd = DeviceUpdate::new().position(offset_ms);
        if dev.status == "playing" {
            upd = upd.status("paused");
        }
        dev.apply(upd, now_f64());
        self.write_device(&dev).await;
    }

    pub async fn remove_device(&self, device_id: &str) -> Option<DeviceState> {
        let device = self.get_device(device_id).await?;
        let mut conn = self.redis.clone();
        let _: Result<(), _> = conn.hdel(REDIS_KEY_DEVICES, device_id).await;
        let _: Result<(), _> = conn.del(pending_key(device_id)).await;
        let _: Result<(), _> = conn.del(queue_key(device_id)).await;
        self.clear_playback_failures(device_id).await;
        Some(device)
    }

    /// 再生失敗を失敗時点の位置とともに記録し、同一トラックの連続失敗回数
    /// (今回を含む) を返す
    pub async fn record_playback_failure(
        &self,
        device_id: &str,
        track_id: &str,
        offset_ms: u64,
    ) -> u32 {
        self.playback_failures
            .lock()
            .await
            .entry(device_id.to_string())
            .or_default()
            .entry(track_id.to_string())
            .or_default()
            .record(offset_ms, now_f64())
    }

    /// デバイスの再生失敗の記録を破棄する (次の失敗は 1 回目から数える)
    pub async fn clear_playback_failures(&self, device_id: &str) {
        self.playback_failures.lock().await.remove(device_id);
    }

    /// デバイス状態に「次に再生」キューの内容を添えて返す。
    /// キューが参照するトラックは全デバイスぶんまとめて 1 回の HMGET で解決する
    /// (Alexa の全 Webhook 応答経路で呼ばれるため往復回数を抑える)
    pub async fn devices_json(&self) -> Value {
        let devices = self.all_devices().await;

        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        for id in devices.keys() {
            queues.insert(id.clone(), self.queue_entries(id).await);
        }
        let tracks = self.fetch_tracks_for(queues.values().flatten()).await;

        let mut map = serde_json::Map::new();
        for (id, dev) in devices {
            let queue: Vec<QueueItem> = queues
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                // 参照先が見つからないエントリは表示から外す (peek_queue が後で片付ける)
                .filter_map(|entry| {
                    let track = tracks.get(token_track_id(&entry)).cloned()?;
                    Some(QueueItem { entry, track })
                })
                .collect();
            match serde_json::to_value(DeviceJson { device: dev, queue }) {
                Ok(v) => {
                    map.insert(id, v);
                }
                Err(e) => tracing::warn!("Failed to serialize device {id}: {e}"),
            }
        }
        Value::Object(map)
    }

    /// キューエントリ群が参照するトラックを 1 回の HMGET でまとめて取得する
    async fn fetch_tracks_for(
        &self,
        entries: impl Iterator<Item = &String>,
    ) -> HashMap<String, AudioTrack> {
        let mut ids: Vec<&str> = entries.map(|e| token_track_id(e)).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return HashMap::new();
        }

        let mut conn = self.redis.clone();
        let vals: Vec<Option<String>> = match conn.hmget(REDIS_KEY_TRACKS, &ids).await {
            Ok(vals) => vals,
            Err(e) => {
                tracing::warn!("Redis error resolving queue tracks: {e}");
                return HashMap::new();
            }
        };
        ids.into_iter()
            .zip(vals)
            .filter_map(|(id, v)| {
                let track = v.and_then(|s| AudioTrack::from_redis_json(&s))?;
                Some((id.to_string(), track))
            })
            .collect()
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

    /// プレイリストの一覧・収録内容の変更を全クライアントへ通知する
    pub async fn broadcast_playlists(&self) {
        self.broadcast(json!({
            "type": "playlists_update",
            "playlists": self.playlists_json().await,
        }));
    }

    /// 選曲範囲 (アクティブプレイリスト) の変更を全クライアントへ通知する
    pub async fn broadcast_active_playlist(&self) {
        self.broadcast(json!({
            "type": "active_playlist_update",
            "playlist": self.active_playlist().await,
        }));
    }

    // ── 再生モード ──

    /// 保存済みの再生モードを返す (未設定・不正値はデフォルトへ正規化、
    /// Redis エラーは Err のまま返して呼び出し元に安全側の判断を委ねる)
    async fn try_playback_mode(&self) -> redis::RedisResult<String> {
        let mut conn = self.redis.clone();
        let mode: Option<String> = conn.get(REDIS_KEY_PLAYBACK_MODE).await?;
        Ok(mode
            .filter(|m| PLAYBACK_MODES.contains(&m.as_str()))
            .unwrap_or_else(|| DEFAULT_PLAYBACK_MODE.to_string()))
    }

    /// 再生終了時の挙動を返す。Redis エラー時はデフォルト
    pub async fn playback_mode(&self) -> String {
        self.try_playback_mode().await.unwrap_or_else(|e| {
            tracing::warn!("Redis error reading playback mode: {e}");
            DEFAULT_PLAYBACK_MODE.to_string()
        })
    }

    /// 再生モードが「オフ」だと確認できた場合のみ true。進行中の再生を
    /// 止める判定に使うため、Redis エラーで確認できないときは止めない側
    /// (false) に倒す (デフォルトが "off" なので playback_mode の値では
    /// 一時的なエラーと本当のオフを区別できない)
    pub async fn playback_mode_is_off(&self) -> bool {
        match self.try_playback_mode().await {
            Ok(mode) => mode == "off",
            Err(e) => {
                tracing::warn!("Redis error reading playback mode: {e}");
                false
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

    pub async fn queue_play(&self, device_id: &str, track: AudioTrack, offset_ms: u64) {
        // Web からの明示的な再生指示なので、失敗の連続記録をリセットして
        // 再試行の余地を戻す
        self.clear_playback_failures(device_id).await;
        let cmd = PendingCommand {
            action: "play".to_string(),
            track: track.clone(),
            offset_ms,
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
        // position もキューした開始位置に合わせる (Resume や Web の表示が参照する)
        self.update_device(
            device_id,
            DeviceUpdate::new()
                .status("queued")
                .track(track)
                .position(offset_ms),
        )
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

    // ── 「次に再生」キュー ──

    /// トラックをデバイスの「次に再生」キュー末尾に追加する。Redis エラー時は false
    pub async fn push_queue(&self, device_id: &str, track_id: &str) -> bool {
        let mut conn = self.redis.clone();
        match conn
            .rpush::<_, _, ()>(queue_key(device_id), new_token(track_id))
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error pushing queue for {device_id}: {e}");
                false
            }
        }
    }

    /// デバイスのキューエントリ一覧を返す (トラックへの解決は devices_json が行う)
    async fn queue_entries(&self, device_id: &str) -> Vec<String> {
        let mut conn = self.redis.clone();
        conn.lrange(queue_key(device_id), 0, -1)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error reading queue for {device_id}: {e}");
                Vec::new()
            })
    }

    /// キュー先頭の (エントリ, トラック) を消費せずに返す。
    /// 参照先のトラックが削除済みと確認できたエントリだけを取り除いて次を見る。
    /// Redis エラー時は安全側 (何も消さず None) に倒す
    pub async fn peek_queue(&self, device_id: &str) -> Option<(String, AudioTrack)> {
        let mut conn = self.redis.clone();
        loop {
            let front: Option<String> = match conn.lindex(queue_key(device_id), 0).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Redis error reading queue for {device_id}: {e}");
                    return None;
                }
            };
            let entry = front?;
            match self.try_get_track(token_track_id(&entry)).await {
                Ok(Some(track)) => return Some((entry, track)),
                Ok(None) => {
                    // 消せなかった場合はループしない (次回の peek で再試行)
                    if !self.remove_queue_entry(device_id, &entry).await {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// エントリを値一致 (LREM) で 1 件取り除く。見つからなければ false。
    /// エントリは一意なので index 指定が不要で、並行する消費と競合しない
    pub async fn remove_queue_entry(&self, device_id: &str, entry: &str) -> bool {
        let mut conn = self.redis.clone();
        match conn.lrem::<_, _, i64>(queue_key(device_id), 1, entry).await {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("Redis error removing queue entry for {device_id}: {e}");
                false
            }
        }
    }

    /// キューを空にする
    pub async fn clear_queue(&self, device_id: &str) {
        let mut conn = self.redis.clone();
        if let Err(e) = conn.del::<_, ()>(queue_key(device_id)).await {
            tracing::warn!("Redis error clearing queue for {device_id}: {e}");
        }
    }

    // ── プレイリスト ──

    /// プレイリストを作成する。名前は前後空白を除いた 1〜PLAYLIST_NAME_MAX_CHARS
    /// 文字。不正な名前・Redis エラー時は None
    pub async fn create_playlist(&self, name: &str) -> Option<Playlist> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > PLAYLIST_NAME_MAX_CHARS {
            return None;
        }
        let playlist = Playlist {
            id: new_playlist_id(),
            name: name.to_string(),
            created_at: now_f64(),
        };
        let json_str = serde_json::to_string(&playlist).expect("Playlist serializes to JSON");
        let mut conn = self.redis.clone();
        match conn
            .hset::<_, _, _, ()>(REDIS_KEY_PLAYLISTS, &playlist.id, json_str)
            .await
        {
            Ok(()) => Some(playlist),
            Err(e) => {
                tracing::warn!("Redis error creating playlist: {e}");
                None
            }
        }
    }

    pub async fn get_playlist(&self, playlist_id: &str) -> Option<Playlist> {
        let mut conn = self.redis.clone();
        match conn
            .hget::<_, _, Option<String>>(REDIS_KEY_PLAYLISTS, playlist_id)
            .await
        {
            Ok(s) => s.and_then(|s| serde_json::from_str(&s).ok()),
            Err(e) => {
                tracing::warn!("Redis error reading playlist {playlist_id}: {e}");
                None
            }
        }
    }

    /// 全プレイリストを作成順で返す
    pub async fn playlists(&self) -> Vec<Playlist> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> =
            conn.hgetall(REDIS_KEY_PLAYLISTS).await.unwrap_or_else(|e| {
                tracing::warn!("Redis error listing playlists: {e}");
                HashMap::new()
            });
        let mut playlists: Vec<Playlist> = all
            .values()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        playlists.sort_by(|a, b| {
            a.created_at
                .total_cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        playlists
    }

    /// プレイリストを削除する。収録リストと、選曲範囲になっていればその指定も
    /// 片付ける (トラック自体は消えない)。見つからなければ false
    pub async fn delete_playlist(&self, playlist_id: &str) -> bool {
        let mut conn = self.redis.clone();
        let removed: i64 = match conn.hdel(REDIS_KEY_PLAYLISTS, playlist_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Redis error deleting playlist {playlist_id}: {e}");
                return false;
            }
        };
        if removed == 0 {
            return false;
        }
        let _: Result<(), _> = conn.del(playlist_key(playlist_id)).await;
        // 選曲範囲に指定されていたらライブラリ全体へ戻す
        if self.raw_active_playlist().await.as_deref() == Some(playlist_id) {
            let _: Result<(), _> = conn.del(REDIS_KEY_ACTIVE_PLAYLIST).await;
        }
        true
    }

    /// トラックをプレイリスト末尾に追加する (収録済みなら末尾へ移動)。
    /// Redis エラー時は false
    pub async fn add_playlist_track(&self, playlist_id: &str, track_id: &str) -> bool {
        // 重複を避けるため一旦除去してから追加する。reorder の全置換 (読み→書き)
        // と交錯しないよう直列化する
        let _guard = self.order_lock.lock().await;
        let key = playlist_key(playlist_id);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .lrem(&key, 0, track_id)
            .ignore()
            .rpush(&key, track_id);
        let mut conn = self.redis.clone();
        match pipe.query_async::<()>(&mut conn).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error adding track to playlist {playlist_id}: {e}");
                false
            }
        }
    }

    /// トラックをプレイリストから外す。収録されていなければ false
    pub async fn remove_playlist_track(&self, playlist_id: &str, track_id: &str) -> bool {
        // reorder の全置換 (読み→書き) の間に割り込むと、置換の書き戻しで
        // 外したはずのトラックが復活してしまうため直列化する
        let _guard = self.order_lock.lock().await;
        let mut conn = self.redis.clone();
        match conn
            .lrem::<_, _, i64>(playlist_key(playlist_id), 0, track_id)
            .await
        {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("Redis error removing track from playlist {playlist_id}: {e}");
                false
            }
        }
    }

    async fn playlist_track_ids(&self, playlist_id: &str) -> Vec<String> {
        let mut conn = self.redis.clone();
        conn.lrange(playlist_key(playlist_id), 0, -1)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error reading playlist {playlist_id}: {e}");
                Vec::new()
            })
    }

    /// プレイリストの収録トラックを収録順で返す (削除済みトラックは飛ばす)
    pub async fn list_playlist_tracks(&self, playlist_id: &str) -> Vec<AudioTrack> {
        let ids = self.playlist_track_ids(playlist_id).await;
        let mut by_id = self.fetch_tracks_for(ids.iter()).await;
        ids.iter().filter_map(|id| by_id.remove(id)).collect()
    }

    /// 全プレイリストのメタデータと収録曲数を作成順で返す (API / WS のワイヤ形式)
    pub async fn playlists_json(&self) -> Value {
        let playlists = self.playlists().await;
        if playlists.is_empty() {
            return json!([]);
        }

        // 収録曲数は 1 往復でまとめて取得する
        let mut pipe = redis::pipe();
        for playlist in &playlists {
            pipe.llen(playlist_key(&playlist.id));
        }
        let mut conn = self.redis.clone();
        let counts: Vec<usize> = match pipe.query_async(&mut conn).await {
            Ok(counts) => counts,
            Err(e) => {
                tracing::warn!("Redis error reading playlist counts: {e}");
                vec![0; playlists.len()]
            }
        };

        let list: Vec<PlaylistJson> = playlists
            .into_iter()
            .zip(counts)
            .map(|(playlist, count)| PlaylistJson { playlist, count })
            .collect();
        json!(list)
    }

    /// 保存されている選曲範囲プレイリスト ID (存在チェックなし)
    async fn raw_active_playlist(&self) -> Option<String> {
        let mut conn = self.redis.clone();
        match conn
            .get::<_, Option<String>>(REDIS_KEY_ACTIVE_PLAYLIST)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Redis error reading active playlist: {e}");
                None
            }
        }
    }

    /// ループ/シャッフルの選曲範囲プレイリスト ID。未設定、または削除済みの
    /// プレイリストを指している場合は None (ライブラリ全体)
    pub async fn active_playlist(&self) -> Option<String> {
        let id = self.raw_active_playlist().await?;
        if self.get_playlist(&id).await.is_some() {
            Some(id)
        } else {
            None
        }
    }

    /// 選曲範囲を設定する (None でライブラリ全体へ戻す)。
    /// 存在しないプレイリスト・Redis エラー時は false
    pub async fn set_active_playlist(&self, playlist_id: Option<&str>) -> bool {
        let mut conn = self.redis.clone();
        let result = match playlist_id {
            Some(pid) => {
                if self.get_playlist(pid).await.is_none() {
                    return false;
                }
                conn.set::<_, _, ()>(REDIS_KEY_ACTIVE_PLAYLIST, pid).await
            }
            None => conn.del::<_, ()>(REDIS_KEY_ACTIVE_PLAYLIST).await,
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error writing active playlist: {e}");
                false
            }
        }
    }

    /// YouTube プレイリストをフラット展開し、同名のローカルプレイリスト
    /// (なければ作成) への取り込みをバックグラウンドで開始する。戻り値は
    /// 開始時点の情報 (プレイリスト名と対象件数)。各動画のダウンロード進捗は
    /// extract_audio と同じ downloads_update で全クライアントへ配られる
    pub async fn import_playlist(
        self: &Arc<Self>,
        list_id: &str,
    ) -> Result<PlaylistImportInfo, String> {
        let url = format!("https://www.youtube.com/playlist?list={list_id}");
        let items = format!("1:{PLAYLIST_IMPORT_MAX}");
        let stdout = run_yt_dlp(
            &[
                "--dump-single-json",
                "--flat-playlist",
                "--playlist-items",
                &items,
                &url,
            ],
            time::Duration::from_secs(PLAYLIST_FLAT_TIMEOUT_SECS),
        )
        .await
        .map_err(|e| format!("Failed to expand playlist: {e}"))?;
        let meta: Value = serde_json::from_slice(&stdout)
            .map_err(|e| format!("Failed to parse playlist metadata: {e}"))?;

        // 動画以外のエントリや重複 (同じ動画が複数回入ったプレイリスト) は除く
        let mut video_ids: Vec<String> = Vec::new();
        for entry in meta["entries"].as_array().map_or(&[][..], |v| v) {
            if let Some(id) = entry["id"].as_str()
                && is_video_id(id)
                && !video_ids.iter().any(|v| v == id)
            {
                video_ids.push(id.to_string());
            }
        }
        if video_ids.is_empty() {
            return Err("Playlist has no importable videos".to_string());
        }

        // プレイリスト名は作成の制限内に丸める (タイトル不明なら list ID)
        let name: String = meta["title"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(list_id)
            .chars()
            .take(PLAYLIST_NAME_MAX_CHARS)
            .collect();

        // 同名プレイリストがあればそこへ追記する (再インポートで重複させない)
        let playlist = match self.playlists().await.into_iter().find(|p| p.name == name) {
            Some(p) => p,
            None => {
                let p = self
                    .create_playlist(&name)
                    .await
                    .ok_or("Failed to create playlist")?;
                self.broadcast_playlists().await;
                p
            }
        };

        let total = video_ids.len();
        let state = self.clone();
        tokio::spawn(async move {
            let mut imported = 0;
            for video_id in &video_ids {
                let url = format!("https://www.youtube.com/watch?v={video_id}");
                match state.extract_audio(&url).await {
                    Ok(track) => {
                        state.add_playlist_track(&playlist.id, &track.id).await;
                        // 取り込めた曲から順に一覧へ反映させる
                        state.broadcast_tracks().await;
                        state.broadcast_playlists().await;
                        imported += 1;
                    }
                    // 失敗はダウンロード進捗のエラー表示で伝わるためログのみ
                    Err(e) => tracing::warn!("Playlist import: skipping {video_id}: {e}"),
                }
            }
            tracing::info!(
                "Playlist import finished: '{}' ({imported}/{total} imported)",
                playlist.name
            );
        });

        Ok(PlaylistImportInfo { name, total })
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
    snippet(&String::from_utf8_lossy(&out.stderr))
}

/// エラーメッセージ用に先頭 300 文字へ切り詰める
fn snippet(s: &str) -> String {
    s.chars().take(300).collect()
}

/// yt-dlp の進捗行 (PROGRESS_PREFIX + " 23.4%" など) からパーセント値を
/// 取り出す。進捗以外の行や割合が未確定 ("N/A" / 非有限値) の行は None
/// (f64::parse は "nan"/"inf" を受理し、NaN は JSON で null になってしまう)
fn parse_progress_percent(line: &str) -> Option<f64> {
    let rest = line.strip_prefix(PROGRESS_PREFIX)?;
    rest.trim()
        .strip_suffix('%')?
        .trim()
        .parse()
        .ok()
        .filter(|p: &f64| p.is_finite())
}

/// yt-dlp を実行し、時間内の正常終了時に stdout を返す。
/// 失敗・タイムアウト時はエラーメッセージ (stderr の先頭を含む) を返す
pub async fn run_yt_dlp(args: &[&str], timeout: time::Duration) -> Result<Vec<u8>, String> {
    let cmd = Command::new("yt-dlp").args(args).output();
    let out = time::timeout(timeout, cmd)
        .await
        .map_err(|_| "yt-dlp timed out".to_string())?
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if !out.status.success() {
        return Err(format!("yt-dlp failed: {}", stderr_snippet(&out)));
    }
    Ok(out.stdout)
}

/// yt-dlp でメタデータ JSON を取得する (ダウンロードなし)
async fn fetch_metadata(url: &str) -> Result<Value, String> {
    let stdout = run_yt_dlp(
        &["--dump-json", "--no-download", url],
        time::Duration::from_secs(METADATA_TIMEOUT_SECS),
    )
    .await
    .map_err(|e| format!("Failed to fetch metadata: {e}"))?;

    serde_json::from_slice(&stdout).map_err(|e| format!("Failed to parse metadata: {e}"))
}

/// 並び順で current_id の次 (dir=1) / 前 (dir=-1) を返す。端は反対側へ
/// 折り返し、current_id が見つからない (削除済みなど) 場合は先頭 (dir=1)
/// または末尾 (dir=-1) を返す
fn neighbor_track(tracks: &[AudioTrack], current_id: &str, dir: isize) -> Option<AudioTrack> {
    if tracks.is_empty() {
        return None;
    }
    let len = tracks.len() as isize;
    let idx = match tracks.iter().position(|t| t.id == current_id) {
        Some(i) => (i as isize + dir).rem_euclid(len),
        None if dir >= 0 => 0,
        None => len - 1,
    };
    tracks.get(idx as usize).cloned()
}

/// current_id 以外からランダムに 1 曲返す (1 曲しかなければその曲)
fn random_track_from(mut tracks: Vec<AudioTrack>, current_id: &str) -> Option<AudioTrack> {
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

/// プレイリスト ID を生成する ("pl" + 時刻由来の値。作成頻度的に十分一意)
fn new_playlist_id() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("pl{:x}{:05x}", d.as_millis(), d.subsec_nanos() & 0xfffff)
}

/// 入力 URL の種類 (動画 / プレイリスト / 不明)
pub enum UrlKind {
    Video,
    Playlist(String),
    Unknown,
}

/// URL を動画・プレイリスト・不明に分類する。動画 ID とプレイリスト ID の
/// 両方を含む URL (プレイリスト再生中の watch URL など) は従来どおり
/// 動画として扱う
pub fn classify_url(url: &str) -> UrlKind {
    if extract_video_id(url).is_some() {
        UrlKind::Video
    } else if let Some(list_id) = extract_playlist_id(url) {
        UrlKind::Playlist(list_id)
    } else {
        UrlKind::Unknown
    }
}

/// URL の list= パラメータから YouTube プレイリスト ID を取り出す。
/// 形式の検証は最小限にとどめ、展開できるかどうかは yt-dlp に任せる
/// (WL などの認証が要るリストもエラーメッセージが利用者に届くように)
fn extract_playlist_id(url: &str) -> Option<String> {
    use std::sync::OnceLock;

    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"youtube\.com/\S*[?&]list=([a-zA-Z0-9_-]{2,})").ok())
        .as_ref()?;
    Some(re.captures(url)?.get(1)?.as_str().to_string())
}

/// 文字列が YouTube 動画 ID の形式かどうか
fn is_video_id(s: &str) -> bool {
    use std::sync::OnceLock;

    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!("^{VIDEO_ID_PATTERN}$")).ok())
        .as_ref()
        .is_some_and(|re| re.is_match(s))
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
            if !is_video_id(stem) {
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
    fn token_roundtrip_preserves_track_id() {
        let token = new_token("dQw4w9WgXcQ");
        assert_eq!(token_track_id(&token), "dQw4w9WgXcQ");
        // '#' を含まない値 (旧形式やトラック ID そのもの) も受け付ける
        assert_eq!(token_track_id("dQw4w9WgXcQ"), "dQw4w9WgXcQ");
    }

    #[test]
    fn auto_token_is_detectable_and_preserves_track_id() {
        let token = auto_token("dQw4w9WgXcQ");
        assert!(is_auto_token(&token));
        assert_eq!(token_track_id(&token), "dQw4w9WgXcQ");
        // 通常の token やキューエントリは自動選曲由来と判定されない
        assert!(!is_auto_token(&new_token("dQw4w9WgXcQ")));
        assert!(!is_auto_token("dQw4w9WgXcQ"));
    }

    #[test]
    fn queue_item_serializes_flattened() {
        let item = QueueItem {
            entry: "dQw4w9WgXcQ#123".into(),
            track: AudioTrack {
                id: "dQw4w9WgXcQ".into(),
                title: "t".into(),
                thumbnail: String::new(),
                duration: 10,
                channel: String::new(),
                is_live: false,
                created_at: 0.0,
                file_path: "/tmp/a.m4a".into(),
            },
        };
        let v = serde_json::to_value(&item).unwrap();
        // entry とトラックのフィールドが同じ階層に並ぶ (file_path は露出しない)
        assert_eq!(v["entry"], "dQw4w9WgXcQ#123");
        assert_eq!(v["id"], "dQw4w9WgXcQ");
        assert!(v.get("file_path").is_none());
    }

    #[test]
    fn pending_command_without_offset_defaults_to_zero() {
        // offset_ms 導入前にキューされた pending コマンドも読めること
        let legacy = r#"{"action":"play","track":{"id":"dQw4w9WgXcQ","title":"t"}}"#;
        let cmd: PendingCommand = serde_json::from_str(legacy).unwrap();
        assert_eq!(cmd.offset_ms, 0);
    }

    /// 長さ 10 秒のトラックを位置 5 秒・last_update 100.0 で再生中のデバイス
    fn playing_device() -> DeviceState {
        DeviceState {
            device_id: "d".into(),
            name: "n".into(),
            status: "playing".into(),
            current_track: Some(AudioTrack {
                id: "dQw4w9WgXcQ".into(),
                title: "t".into(),
                thumbnail: String::new(),
                duration: 10,
                channel: String::new(),
                is_live: false,
                created_at: 0.0,
                file_path: String::new(),
            }),
            position_ms: 5_000,
            connected: true,
            last_update: 100.0,
        }
    }

    #[test]
    fn advance_position_only_moves_while_playing_and_clamps_to_duration() {
        let mut dev = playing_device();
        dev.advance_position(102.0);
        assert_eq!(dev.position_ms, 7_000);

        // トラック終端でクランプ
        dev.last_update = 102.0;
        dev.advance_position(200.0);
        assert_eq!(dev.position_ms, 10_000);

        // 再生中以外は進めない
        dev.status = "paused".into();
        dev.position_ms = 1_000;
        dev.advance_position(300.0);
        assert_eq!(dev.position_ms, 1_000);
    }

    #[test]
    fn apply_keeps_position_consistent_with_last_update() {
        // 位置を伴わない更新でも推定位置 (position_ms + 経過時間) が巻き戻らない
        let mut dev = playing_device();
        dev.apply(DeviceUpdate::new(), 102.0);
        assert_eq!(dev.position_ms, 7_000);
        assert_eq!(dev.last_update, 102.0);

        // 明示的な位置指定はそのまま採用される
        let mut dev = playing_device();
        dev.apply(DeviceUpdate::new().position(1_000), 102.0);
        assert_eq!(dev.position_ms, 1_000);
        assert_eq!(dev.last_update, 102.0);
    }

    #[test]
    fn failure_record_counts_consecutive_failures() {
        let mut rec = FailureRecord::default();

        // 同じ位置で失敗し続ける限り連続として数える
        assert_eq!(rec.record(5_000, 100.0), 1);
        assert_eq!(rec.record(5_000, 105.0), 2);
        assert_eq!(rec.record(5_000, 110.0), 3);

        // 位置が前進した失敗は数え直す (間の再試行が成功していた証拠)
        assert_eq!(rec.record(30_000, 115.0), 1);

        // 位置が進んでいない失敗 (再始動の失敗など) は連続として数える
        assert_eq!(rec.record(0, 120.0), 2);

        // 前回から FAILURE_RESET_SECS を超えて空いた失敗も数え直す
        assert_eq!(rec.record(0, 120.0 + FAILURE_RESET_SECS + 1.0), 1);
    }

    #[test]
    fn parses_progress_lines() {
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX} 23.4%")),
            Some(23.4)
        );
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}100.0%")),
            Some(100.0)
        );
        // 割合が未確定の行や進捗以外の出力は無視する
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}N/A")),
            None
        );
        assert_eq!(
            parse_progress_percent("[download] Destination: x.m4a"),
            None
        );
        // f64::parse が受理する非有限値は弾く (JSON で null になるため)
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}nan%")),
            None
        );
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}inf%")),
            None
        );
    }

    #[test]
    fn download_status_serializes_lowercase() {
        // フロント (types.ts) の status union と一致するワイヤ形式を固定する
        for (status, expected) in [
            (DownloadStatus::Metadata, "metadata"),
            (DownloadStatus::Downloading, "downloading"),
            (DownloadStatus::Processing, "processing"),
            (DownloadStatus::Error, "error"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(expected));
        }
    }

    /// テスト用の最小トラック
    fn track(id: &str) -> AudioTrack {
        AudioTrack {
            id: id.into(),
            title: id.into(),
            thumbnail: String::new(),
            duration: 10,
            channel: String::new(),
            is_live: false,
            created_at: 0.0,
            file_path: String::new(),
        }
    }

    #[test]
    fn neighbor_track_wraps_and_falls_back() {
        let tracks = vec![
            track("aaaaaaaaaa1"),
            track("aaaaaaaaaa2"),
            track("aaaaaaaaaa3"),
        ];

        // 順方向: 次へ進み、末尾は先頭へ折り返す
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa1", 1).unwrap().id,
            "aaaaaaaaaa2"
        );
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa3", 1).unwrap().id,
            "aaaaaaaaaa1"
        );
        // 逆方向: 前へ戻り、先頭は末尾へ折り返す
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa2", -1).unwrap().id,
            "aaaaaaaaaa1"
        );
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa1", -1).unwrap().id,
            "aaaaaaaaaa3"
        );
        // 見つからない (削除済みなど): 先頭 / 末尾
        assert_eq!(
            neighbor_track(&tracks, "gone", 1).unwrap().id,
            "aaaaaaaaaa1"
        );
        assert_eq!(
            neighbor_track(&tracks, "gone", -1).unwrap().id,
            "aaaaaaaaaa3"
        );
        // 空なら None
        assert!(neighbor_track(&[], "aaaaaaaaaa1", 1).is_none());
    }

    #[test]
    fn random_track_excludes_current_unless_only_one() {
        // 2 曲なら必ずもう一方が選ばれる
        let tracks = vec![track("aaaaaaaaaa1"), track("aaaaaaaaaa2")];
        for _ in 0..5 {
            let picked = random_track_from(tracks.clone(), "aaaaaaaaaa1").unwrap();
            assert_eq!(picked.id, "aaaaaaaaaa2");
        }
        // 1 曲しかなければその曲
        let only = vec![track("aaaaaaaaaa1")];
        assert_eq!(
            random_track_from(only, "aaaaaaaaaa1").unwrap().id,
            "aaaaaaaaaa1"
        );
        assert!(random_track_from(Vec::new(), "aaaaaaaaaa1").is_none());
    }

    #[test]
    fn classifies_urls() {
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            UrlKind::Video
        ));
        // v= と list= の両方を含む URL は動画として扱う
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL0123456789abcdefghij"),
            UrlKind::Video
        ));
        match classify_url("https://www.youtube.com/playlist?list=PL0123456789abcdefghij") {
            UrlKind::Playlist(id) => assert_eq!(id, "PL0123456789abcdefghij"),
            _ => panic!("expected playlist"),
        }
        // v= なしの watch?list= もプレイリスト扱い
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?list=PL0123456789abcdefghij"),
            UrlKind::Playlist(_)
        ));
        // WL などの短い特殊リスト ID も受け付ける (展開可否は yt-dlp に任せる)
        assert!(matches!(
            classify_url("https://www.youtube.com/playlist?list=WL"),
            UrlKind::Playlist(_)
        ));
        assert!(matches!(
            classify_url("https://example.com/watch?v=x"),
            UrlKind::Unknown
        ));
        assert!(matches!(
            classify_url("https://www.youtube.com/feed/library"),
            UrlKind::Unknown
        ));
    }

    #[test]
    fn video_id_format_check() {
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("short"));
        assert!(!is_video_id("dQw4w9WgXcQ-too-long"));
    }

    #[test]
    fn playlist_ids_are_distinct() {
        // 連続生成でも重複しない (時刻由来のため同一ミリ秒でもナノ秒で分かれる)
        let a = new_playlist_id();
        let b = new_playlist_id();
        assert_ne!(a, b);
        assert!(a.starts_with("pl"));
    }

    #[test]
    fn redis_json_without_is_live_defaults_to_false() {
        let legacy = r#"{"id":"dQw4w9WgXcQ","title":"t","thumbnail":"","duration":10,"channel":"","created_at":1.0,"file_path":"/tmp/a.m4a"}"#;
        let track = AudioTrack::from_redis_json(legacy).unwrap();
        assert!(!track.is_live);
    }
}
