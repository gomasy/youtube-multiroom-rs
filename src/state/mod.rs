//! Shared application state.
//!
//! `AppState` owns the Redis connection, the WebSocket broadcast channel and
//! the in-process bookkeeping that outlives a single request (download
//! progress, per-video extract slots, playback failure counters). Its methods
//! are grouped by subject into sibling modules, each contributing its own
//! `impl AppState` block:
//!
//! - [`model`] — wire types, and the tokens identifying a queued play
//! - [`track`] — the audio library: registration, ordering, selection
//! - [`matching`] — ranking stored text against a spoken phrase
//! - [`device`] — per-device state, pending commands, Up Next queues
//! - [`playback`] — playback mode and the sleep timer
//! - [`playlist`] — named playlists and YouTube playlist import
//! - [`download`] — downloading audio and refreshing track metadata
//! - [`progress`] — what clients are told about that work while it runs
//! - [`remux`] — rebuilding a downloaded file's container before it is served
//! - [`job`] — the stopping rules shared by the background per-video jobs
//! - [`ytdlp`] — running yt-dlp and reaping its process group
//! - [`url`] — recognizing YouTube URLs
//!
//! The modules are private: everything the rest of the crate uses is
//! re-exported here, so `crate::state::X` stays the single import path.

use download::ExtractSlot;
use model::{DownloadProgress, FailureRecord};
use redis::aio::ConnectionManager;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, broadcast};
use tokio::time;
use tokio_util::sync::CancellationToken;

mod device;
mod download;
mod job;
mod matching;
mod model;
mod playback;
mod playlist;
mod progress;
mod remux;
mod track;
mod url;
mod ytdlp;

pub use model::{
    AudioTrack, DeviceState, DeviceUpdate, PendingCommand, PlayRequest, ReorderOutcome,
    ReorderRequest, SeekRequest, WriteOutcome, auto_token, is_auto_token, new_token,
    token_track_id,
};
pub(crate) use url::watch_url;
pub use url::{UrlKind, classify_url};
pub use ytdlp::{DownloadError, run_yt_dlp};

pub(crate) const REDIS_KEY_ACTIVE_PLAYLIST: &str = "youtube:active_playlist";
pub(crate) const REDIS_KEY_DEVICES: &str = "youtube:devices";
pub(crate) const REDIS_KEY_PLAYBACK_MODE: &str = "youtube:playback_mode";
pub(crate) const REDIS_KEY_PLAYLISTS: &str = "youtube:playlists";
pub(crate) const REDIS_KEY_SLEEP_TIMER: &str = "youtube:sleep_timer";
pub(crate) const REDIS_KEY_TRACKS: &str = "youtube:tracks";
pub(crate) const REDIS_KEY_TRACKS_ORDER: &str = "youtube:tracks_order";
pub(crate) const REDIS_PENDING_PREFIX: &str = "youtube:pending";
const REDIS_PLAYLIST_PREFIX: &str = "youtube:playlist";
/// Each entry is a unique "{track_id}#{millis}" string used as the AudioPlayer
/// token, so playback events can match and consume entries by value.
const REDIS_QUEUE_PREFIX: &str = "youtube:queue";

/// Cached audio format extension. Must be kept in sync with AUDIO_MIME.
pub(crate) const AUDIO_EXT: &str = "m4a";
pub const AUDIO_MIME: &str = "audio/mp4";

/// Log the result of a fire-and-forget Redis command. These callers have no
/// recovery to offer, but a silently dropped error would hide a broken Redis
/// behind seemingly successful operations.
///
/// A macro rather than a function so the awaited result is bound before the
/// format arguments are built: `fmt::Arguments` is not `Send`, and holding one
/// across the await would make every enclosing future non-`Send`.
macro_rules! warn_redis {
    ($what:literal, $result:expr) => {{
        let result: redis::RedisResult<()> = $result;
        if let Err(e) = result {
            tracing::warn!("Redis error {}: {e}", format_args!($what));
        }
    }};
}
pub(crate) use warn_redis;

pub(crate) fn pending_key(device_id: &str) -> String {
    format!("{REDIS_PENDING_PREFIX}:{device_id}")
}

pub(crate) fn queue_key(device_id: &str) -> String {
    format!("{REDIS_QUEUE_PREFIX}:{device_id}")
}

pub(crate) fn playlist_key(playlist_id: &str) -> String {
    format!("{REDIS_PLAYLIST_PREFIX}:{playlist_id}")
}

pub struct AppState {
    redis: ConnectionManager,
    pub tx: broadcast::Sender<String>,
    pub cache_dir: PathBuf,
    pub api_token: Option<String>,
    /// Whether track restoration from audio_cache is in progress (prevents
    /// concurrent runs).
    restoring: AtomicBool,
    /// Serializes modifications to youtube:tracks_order so reorder's
    /// read-then-replace and extract/remove's LPUSH/LREM don't interleave.
    order_lock: Mutex<()>,
    /// Per-video coordination between downloads, metadata refreshes and a
    /// deletion of the track they register (see [`ExtractSlot`]). An entry
    /// lives only while one of those is in flight.
    extract_slots: Mutex<HashMap<String, Arc<ExtractSlot>>>,
    /// In-progress download progress (video ID → progress). In-process only;
    /// lost on restart (clients re-sync via the init snapshot).
    downloads: Mutex<HashMap<String, DownloadProgress>>,
    /// Shared cancellation generation. Cancelling swaps this token so work
    /// already in progress stops while subsequently started work is unaffected.
    download_cancel: Mutex<CancellationToken>,
    /// Per-device per-track consecutive playback failure records (see
    /// record_playback_failure). Tracked per-track so interleaved failures from
    /// the current and ENQUEUE'd next track don't reset each other's count.
    playback_failures: Mutex<HashMap<String, HashMap<String, FailureRecord>>>,
    /// Bumped when the sleep timer is set/cancelled; the spawned timer task
    /// checks this to know if it's been superseded.
    sleep_timer_gen: AtomicU64,
    /// Bumped on every track-list change. A client that fetched a page over
    /// REST before its WebSocket existed compares the revision it was served
    /// with the one in `init`: equal means the page it holds is still current.
    /// In-process only, so it restarts at 0 — clients treat every reconnect as
    /// a change regardless.
    tracks_rev: AtomicU64,
}

impl AppState {
    pub async fn new(
        api_token: Option<String>,
        redis_url: &str,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let (tx, _) = broadcast::channel::<String>(256);
        // An unreadable working directory leaves the cache path relative, which
        // resolves to the same place for a server started the usual way. Both
        // that and a failed create are reported rather than swallowed: a cache
        // directory that never appeared explains failures that would otherwise
        // surface much later, one staging-directory error at a time.
        let cache_dir = std::env::current_dir()
            .inspect_err(|e| tracing::warn!("Cannot read the working directory: {e}"))
            .unwrap_or_default()
            .join("audio_cache");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            tracing::warn!(
                "Failed to create the audio cache directory {}: {e}",
                cache_dir.display()
            );
        }

        let client = redis::Client::open(redis_url)?;
        // Naming the URL is what makes this error actionable, but it is the one
        // place a password could reach stderr, so it goes out redacted. The
        // Client::open error above carries no URL of its own.
        let redis = time::timeout(time::Duration::from_secs(5), ConnectionManager::new(client))
            .await
            .map_err(|_| {
                format!(
                    "Redis connection timed out ({})",
                    crate::redact_url(redis_url)
                )
            })??;

        Ok(Arc::new(Self {
            redis,
            tx,
            cache_dir,
            api_token,
            restoring: AtomicBool::new(false),
            order_lock: Mutex::new(()),
            extract_slots: Mutex::new(HashMap::new()),
            downloads: Mutex::new(HashMap::new()),
            download_cancel: Mutex::new(CancellationToken::new()),
            playback_failures: Mutex::new(HashMap::new()),
            sleep_timer_gen: AtomicU64::new(0),
            tracks_rev: AtomicU64::new(0),
        }))
    }

    // ── Broadcast ──

    /// Send a message to all connected WebSocket clients (no-op if no subscribers).
    pub(crate) fn broadcast(&self, msg: Value) {
        let _ = self.tx.send(msg.to_string());
    }

    pub async fn broadcast_devices(&self) {
        self.broadcast(json!({
            "type": "device_update",
            "devices": self.devices_json().await,
        }));
    }

    /// Notify clients that the track list changed (content is re-fetched via
    /// REST). Not `async`, unlike its siblings: the frame carries no payload,
    /// so there is no state to read before sending.
    pub fn broadcast_tracks(&self) {
        // Bumped before sending, so a client reading the revision after this
        // frame is never told the list is older than what it just heard about.
        self.tracks_rev.fetch_add(1, Ordering::SeqCst);
        self.broadcast(tracks_update_message());
    }

    /// The current track-list revision (see `tracks_rev`). Sample it *before*
    /// reading the list itself, so a change landing mid-read shows up as a
    /// newer revision rather than being hidden by the page that was served.
    pub fn tracks_rev(&self) -> u64 {
        self.tracks_rev.load(Ordering::SeqCst)
    }

    /// Broadcast playlist list/content changes to all clients.
    pub async fn broadcast_playlists(&self) {
        self.broadcast(json!({
            "type": "playlists_update",
            "playlists": self.playlists_json().await,
        }));
    }

    /// Broadcast active playlist (selection scope) changes to all clients.
    pub async fn broadcast_active_playlist(&self) {
        self.broadcast(json!({
            "type": "active_playlist_update",
            "playlist": self.active_playlist().await,
        }));
    }
}

/// The "track list changed, re-fetch it" nudge. Tracks are paginated over REST
/// rather than pushed, so this frame carries no payload. Shared with the
/// WebSocket resync path so the message shape has one definition.
pub fn tracks_update_message() -> Value {
    json!({ "type": "tracks_update" })
}

/// Time elapsed since the UNIX epoch. A clock predating 1970 reads as zero
/// rather than an error: every caller wants a number to derive a timestamp or
/// an ID from, and none has a better answer than the epoch itself.
pub(crate) fn since_epoch() -> std::time::Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

pub(crate) fn now_f64() -> f64 {
    since_epoch().as_secs_f64()
}
