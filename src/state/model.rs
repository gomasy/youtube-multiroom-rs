//! Data types shared across the state modules: what is stored in Redis, what
//! is sent to clients, and the tokens that tie an AudioPlayer directive back to
//! the queue entry that produced it. No Redis access lives here.

use super::now_f64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Failures closer than this are counted as consecutive. Retries arrive within
/// seconds, so this threshold is generous.
const FAILURE_RESET_SECS: f64 = 60.0;

/// Generate an AudioPlayer token / queue entry "{track_id}#{millis}".
/// Alexa ignores ENQUEUE with the same token as the current one, so each play
/// must produce a unique token.
pub fn new_token(track_id: &str) -> String {
    let millis = (now_f64() * 1000.0) as u64;
    format!("{track_id}#{millis}")
}

/// Extract the track ID portion from a token / queue entry.
/// YouTube IDs never contain '#'.
pub fn token_track_id(token: &str) -> &str {
    token.split('#').next().unwrap_or(token)
}

const AUTO_TOKEN_SUFFIX: &str = "#auto";

/// Build a token that marks auto-continued playback (loop/shuffle). If the
/// playback mode is switched to "off" after ENQUEUE, the directive cannot be
/// revoked, so PlaybackStarted uses this marker to stop playback.
pub fn auto_token(track_id: &str) -> String {
    format!("{}{AUTO_TOKEN_SUFFIX}", new_token(track_id))
}

pub fn is_auto_token(token: &str) -> bool {
    token.ends_with(AUTO_TOKEN_SUFFIX)
}

const START_SEGMENT_PREFIX: &str = "from";

/// Append `#from<start_ms>` (ahead of the auto suffix). An Echo playing a relay
/// that starts mid-track reports positions relative to that start, and the
/// token is what each event carries back.
pub fn with_start_offset(token: &str, start_ms: u64) -> String {
    if start_ms == 0 {
        return token.to_string();
    }
    let (head, auto) = match token.strip_suffix(AUTO_TOKEN_SUFFIX) {
        Some(head) => (head, AUTO_TOKEN_SUFFIX),
        None => (token, ""),
    };
    format!("{head}#{START_SEGMENT_PREFIX}{start_ms}{auto}")
}

/// The start recorded by [`with_start_offset`], else zero.
pub fn token_start_offset(token: &str) -> u64 {
    token
        .split('#')
        .skip(1)
        .find_map(|segment| segment.strip_prefix(START_SEGMENT_PREFIX)?.parse().ok())
        .unwrap_or(0)
}

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
    /// On air or upcoming; both play via the live relay.
    #[serde(default)]
    pub is_live: bool,
    /// Added without downloading: every playback relays from the CDN. Never
    /// set together with `is_live`.
    #[serde(default)]
    pub streamed: bool,
    /// Scheduled start (Unix secs) while the stream is upcoming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_start: Option<f64>,
    #[serde(default)]
    pub created_at: f64,
    /// Excluded from API responses; only included by to_redis_json for Redis.
    #[serde(skip)]
    pub file_path: String,
}

impl AudioTrack {
    /// Redis representation: the API shape plus the `serde(skip)`ped file path,
    /// which never reaches clients.
    pub(crate) fn to_redis_json(&self) -> serde_json::Result<String> {
        let mut v = serde_json::to_value(self)?;
        v["file_path"] = json!(self.file_path);
        Ok(v.to_string())
    }

    /// The latest offset worth starting from: a second short of the end.
    /// `None` when the length is unknown.
    pub fn max_offset_ms(&self) -> Option<u64> {
        (self.duration > 0).then(|| self.duration.saturating_mul(1000).saturating_sub(1000))
    }

    /// `offset_ms` held to [`Self::max_offset_ms`]; unchanged when the length
    /// is unknown.
    pub fn clamp_offset_ms(&self, offset_ms: u64) -> u64 {
        self.max_offset_ms()
            .map_or(offset_ms, |max| offset_ms.min(max))
    }

    /// Whether playback is served from a cached file.
    pub fn keeps_file(&self) -> bool {
        !self.is_live && !self.streamed
    }

    /// The video's title, or its ID when the metadata carries none.
    pub(crate) fn extract_title(id: &str, meta: &Value) -> String {
        meta["title"].as_str().unwrap_or(id).to_string()
    }

    pub(crate) fn extract_channel(meta: &Value) -> String {
        meta["channel"]
            .as_str()
            .or_else(|| meta["uploader"].as_str())
            .unwrap_or("")
            .to_string()
    }

    /// The video length in seconds from yt-dlp metadata. Integer and float
    /// forms are both accepted (flat playlist entries carry a float); anything
    /// unusable reads as zero.
    pub(crate) fn extract_duration(meta: &Value) -> u64 {
        let duration = &meta["duration"];
        duration
            .as_u64()
            .or_else(|| {
                let secs = duration.as_f64()?;
                (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
            })
            .unwrap_or(0)
    }

    /// On air or upcoming, per yt-dlp's `live_status` (full or flat metadata).
    pub(crate) fn extract_is_live(meta: &Value) -> bool {
        matches!(
            meta["live_status"].as_str(),
            Some("is_live" | "is_upcoming")
        )
    }

    /// `release_timestamp`, only while upcoming (finished videos carry it too).
    pub(crate) fn extract_scheduled_start(meta: &Value) -> Option<f64> {
        (meta["live_status"] == "is_upcoming")
            .then(|| meta["release_timestamp"].as_f64())
            .flatten()
            .filter(|t| *t > 0.0)
    }

    /// Scheduled start is still ahead of `now`.
    pub(crate) fn is_upcoming(&self, now: f64) -> bool {
        self.scheduled_start.is_some_and(|start| start > now)
    }

    pub(crate) fn from_meta(id: &str, meta: &Value, created_at: f64, file_path: String) -> Self {
        Self {
            id: id.to_string(),
            title: Self::extract_title(id, meta),
            thumbnail: meta["thumbnail"].as_str().unwrap_or("").to_string(),
            duration: Self::extract_duration(meta),
            channel: Self::extract_channel(meta),
            is_live: Self::extract_is_live(meta),
            streamed: false,
            scheduled_start: Self::extract_scheduled_start(meta),
            created_at,
            file_path,
        }
    }

    pub(crate) fn from_redis_json(s: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(s).ok()?;
        let file_path = v["file_path"].as_str()?.to_string();
        let mut track: Self = serde_json::from_value(v).ok()?;
        track.file_path = file_path;
        Some(track)
    }

    /// A file-backed track with nothing but an ID, for tests to build on.
    #[cfg(test)]
    pub(crate) fn stub(id: &str) -> Self {
        Self::from_meta(id, &Value::Null, 0.0, String::new())
    }
}

/// A track as the library listing serves it: the stored entry plus the one
/// thing only the filesystem can answer.
#[derive(Serialize)]
pub struct TrackJson {
    #[serde(flatten)]
    pub track: AudioTrack,
    /// Registered but its cache file is gone. Omitted when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub file_missing: bool,
}

/// Named playlist. Track IDs are stored in a separate list (youtube:playlist:{id}).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub created_at: f64,
    /// The YouTube playlist ID it was imported from, which a sync reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Wire format for playlists_json: metadata with track count.
#[derive(Serialize)]
pub(crate) struct PlaylistJson {
    #[serde(flatten)]
    pub(crate) playlist: Playlist,
    pub(crate) count: usize,
}

/// Returned when a playlist import or sync starts; a sync that found nothing
/// new reports zero.
pub struct PlaylistImportInfo {
    pub name: String,
    pub total: usize,
}

/// What the cache directory holds, measured against what the library expects
/// of it (see [`super::cache`]).
#[derive(Serialize)]
pub struct CacheReport {
    /// Bytes held by every `{video_id}.m4a` in the cache, orphans included.
    pub total_bytes: u64,
    pub file_count: usize,
    /// Cache files no registered track claims, largest first.
    pub orphans: Vec<OrphanFile>,
    /// Registered tracks whose cache file is gone, in library order.
    pub missing: Vec<AudioTrack>,
}

/// A cache file nothing in the library refers to.
#[derive(Serialize)]
pub struct OrphanFile {
    /// The video ID its name spells.
    pub id: String,
    pub bytes: u64,
}

/// The library structure Redis holds and no audio file records: which tracks
/// exist, the order they play in, and the playlists they belong to. Exported
/// and imported as one document (see [`super::library`]).
#[derive(Serialize, Deserialize)]
pub struct LibraryExport {
    /// Document format version, checked on import.
    pub version: u32,
    pub exported_at: f64,
    pub playback_mode: String,
    /// The selection scope at export time, as a playlist ID of the *exporting*
    /// server. An import re-resolves it through the playlists it matched.
    pub active_playlist: Option<String>,
    /// Every track, in library order. `AudioTrack` skips `file_path` itself, so
    /// nothing here names a path on the machine it came from.
    pub tracks: Vec<AudioTrack>,
    pub playlists: Vec<PlaylistExport>,
}

/// One playlist and its membership, as an export carries it.
#[derive(Serialize, Deserialize)]
pub struct PlaylistExport {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub track_ids: Vec<String>,
}

/// What an import changed, for the reply that reports it.
pub struct ImportOutcome {
    /// Playlists created or appended to.
    pub playlists: usize,
    /// Tracks the restored library order names.
    pub tracks: usize,
    /// Videos the document lists that this server holds no track for. Their
    /// place in the order is restored regardless, so downloading one later
    /// drops it straight back where it belongs.
    pub missing_ids: Vec<String>,
}

/// Result of reorder_track. Lets callers distinguish "not in list" (not enrolled
/// or concurrently deleted) from Redis errors.
pub enum ReorderOutcome {
    Moved,
    NotInList,
    Failed,
}

/// Why a playlist could not be created or renamed.
#[derive(Debug)]
pub enum PlaylistNameError {
    /// Empty or too long once trimmed.
    Invalid,
    /// Another playlist already has the name.
    Taken,
    NotFound,
    Failed,
}

/// The outcome of a write guarded by a Redis script checking the target still
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// The target is no longer registered.
    Gone,
    /// The write failed; the cause is logged where it happened.
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
    /// Apply an update and advance last_update to `now`. Every device state
    /// change must go through here: position_ms means "position as of
    /// last_update", so advancing last_update alone would rewind the client's
    /// estimated position.
    pub(crate) fn apply(&mut self, upd: DeviceUpdate, now: f64) {
        // Without an explicit position (a re-registration, say), the elapsed
        // time is folded in to keep the two consistent.
        if upd.position_ms.is_none() {
            self.position_ms = self.estimated_position_ms(now);
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

    pub(crate) fn prior(&self) -> PriorState {
        PriorState {
            status: self.status.clone(),
            track_id: self.current_track.as_ref().map(|t| t.id.clone()),
            position_ms: self.position_ms,
            last_update: self.last_update,
        }
    }

    /// Put back `prior` with the current copy of its track, advancing the
    /// position by the play time since.
    pub(crate) fn restore(&mut self, prior: PriorState, track: Option<AudioTrack>, now: f64) {
        self.status = prior.status;
        self.current_track = track;
        self.position_ms = prior.position_ms;
        self.last_update = prior.last_update;
        self.apply(DeviceUpdate::new(), now);
    }

    /// Playing or paused.
    pub fn playback_in_progress(&self) -> bool {
        matches!(self.status.as_str(), "playing" | "paused")
    }

    /// The last reported position, advanced by the time elapsed while playing
    /// (clamped to the track end). Mirrors the web UI's seek bar.
    pub fn estimated_position_ms(&self, now: f64) -> u64 {
        if self.status != "playing" {
            return self.position_ms;
        }
        let elapsed_ms = ((now - self.last_update).max(0.0) * 1000.0) as u64;
        let max_ms = self
            .current_track
            .as_ref()
            .map(|t| t.duration.saturating_mul(1000))
            .filter(|&d| d > 0)
            .unwrap_or(u64::MAX);
        self.position_ms.saturating_add(elapsed_ms).min(max_ms)
    }
}

/// Per-track consecutive playback failure record. Used to decide when to stop
/// retrying PlaybackFailed. Kept in-process only; reset on restart.
#[derive(Debug, Default)]
pub(crate) struct FailureRecord {
    count: u32,
    last_failure: f64,
    /// Playback position (ms) at last failure. If position has advanced since
    /// then, the retry succeeded, so it is not counted as consecutive.
    last_offset: u64,
}

impl FailureRecord {
    /// Record a failure and return the consecutive count (including this one).
    /// Resets to 1 if more than FAILURE_RESET_SECS have passed since the last
    /// failure, or if the position has advanced (proving an intervening success).
    pub(crate) fn record(&mut self, offset_ms: u64, now: f64) -> u32 {
        if now - self.last_failure > FAILURE_RESET_SECS || offset_ms > self.last_offset {
            self.count = 0;
        }
        self.count += 1;
        self.last_failure = now;
        self.last_offset = offset_ms;
        self.count
    }
}

/// A single Up Next queue item (for API responses).
#[derive(Serialize)]
pub struct QueueItem {
    /// Unique within the queue ("{track_id}#{millis}"); identifies the item
    /// for removal.
    pub entry: String,
    #[serde(flatten)]
    pub track: AudioTrack,
}

/// Wire format for devices_json: device state plus Up Next queue. Kept separate
/// from DeviceState to avoid write_device persisting queue data to Redis.
#[derive(Serialize)]
pub(crate) struct DeviceJson {
    #[serde(flatten)]
    pub(crate) device: DeviceState,
    pub(crate) queue: Vec<QueueItem>,
}

/// Download stage. Lowercase wire format must match the frontend
/// DownloadProgress["status"] union.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    /// Fetching metadata (before title / live detection is resolved).
    Metadata,
    Downloading,
    /// Post-processing: yt-dlp's conversion to m4a, then the container rebuild.
    Processing,
    Error,
}

/// What a progress entry is tracking. Lowercase wire format must match the
/// frontend DownloadProgress["kind"] union.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadKind {
    Video,
    /// A playlist import that has not expanded into per-video jobs yet.
    Playlist,
}

/// In-progress download progress. Kept in-process and broadcast to all
/// WebSocket clients so any browser (including after a reload) can track it.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    /// Identifies the entry, not the video: a playlist import is namespaced
    /// apart from the video IDs (see progress::playlist_progress_key).
    pub id: String,
    pub kind: DownloadKind,
    /// Filled with the video ID (a playlist import: the playlist ID) before
    /// metadata is fetched.
    pub title: String,
    pub status: DownloadStatus,
    /// Download progress percentage (0.0–100.0).
    pub percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Start time (UNIX seconds). Used for display order and identity check
    /// when cleaning up errors.
    pub started_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommand {
    pub action: String,
    pub track: AudioTrack,
    /// Playback start position in milliseconds. Non-zero when seeking from web.
    #[serde(default)]
    pub offset_ms: u64,
    /// The device state this play replaced; restored on cancel.
    #[serde(default)]
    pub prior: Option<PriorState>,
}

/// Only the track id is kept: the track itself is re-read on restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorState {
    pub status: String,
    pub track_id: Option<String>,
    pub position_ms: u64,
    pub last_update: f64,
}

#[derive(Deserialize)]
pub struct PlayRequest {
    pub track_id: String,
    pub device_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct SeekRequest {
    /// Seek target in milliseconds (clamped to just before track end).
    pub position_ms: u64,
}

#[derive(Deserialize)]
pub struct SyncRequest {
    /// Devices to line up with the one named in the path. Absent means every
    /// other registered device, which is what the web UI asks for.
    #[serde(default)]
    pub device_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub track_id: String,
    /// Target index (0-based, clamped to list end if out of range).
    pub new_index: usize,
    /// Playlist ID to reorder within (omit for library-wide order).
    #[serde(default)]
    pub playlist: Option<String>,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_json_round_trips_and_tolerates_legacy_entries() {
        let track = AudioTrack {
            title: "配信".into(),
            is_live: true,
            scheduled_start: Some(1_800_000_000.0),
            ..AudioTrack::stub("dQw4w9WgXcQ")
        };
        let restored = AudioTrack::from_redis_json(&track.to_redis_json().unwrap()).unwrap();
        assert!(restored.is_live);
        assert_eq!(restored.scheduled_start, Some(1_800_000_000.0));
        assert!(restored.file_path.is_empty());

        // Entries written before is_live existed must still parse
        let legacy = r#"{"id":"dQw4w9WgXcQ","title":"t","thumbnail":"","duration":10,"channel":"","created_at":1.0,"file_path":"/tmp/a.m4a"}"#;
        let legacy = AudioTrack::from_redis_json(legacy).unwrap();
        assert!(!legacy.is_live);
        assert!(legacy.scheduled_start.is_none());
    }

    #[test]
    fn upcoming_streams_register_as_live_with_their_schedule() {
        let upcoming = json!({
            "title": "Stream",
            "is_live": false,
            "live_status": "is_upcoming",
            "release_timestamp": 1_800_000_000,
        });
        let track = AudioTrack::from_meta("dQw4w9WgXcQ", &upcoming, 1.0, String::new());
        assert!(track.is_live);
        assert_eq!(track.scheduled_start, Some(1_800_000_000.0));

        // On air: live, with nothing left to wait for
        let on_air = json!({ "is_live": true, "live_status": "is_live" });
        assert!(AudioTrack::extract_is_live(&on_air));
        assert!(AudioTrack::extract_scheduled_start(&on_air).is_none());

        // A finished stream keeps its release_timestamp but is neither
        let was_live = json!({
            "is_live": false,
            "live_status": "was_live",
            "release_timestamp": 1_700_000_000,
        });
        assert!(!AudioTrack::extract_is_live(&was_live));
        assert!(AudioTrack::extract_scheduled_start(&was_live).is_none());
    }

    #[test]
    fn duration_is_read_from_both_forms_yt_dlp_reports() {
        let secs = |v: Value| AudioTrack::extract_duration(&json!({ "duration": v }));
        // Whole seconds, or fractional from flat playlist entries
        assert_eq!(secs(json!(213)), 213);
        assert_eq!(secs(json!(213.4)), 213);
        // Live streams and unavailable videos report no length
        assert_eq!(AudioTrack::extract_duration(&json!({})), 0);
        assert_eq!(secs(Value::Null), 0);
        // Nothing unusable may reach the wire format as a length
        assert_eq!(secs(json!(-5.0)), 0);
        assert_eq!(secs(json!("3:33")), 0);
    }

    #[test]
    fn every_token_form_reads_back_its_id_marker_and_start_offset() {
        const ID: &str = "dQw4w9WgXcQ";
        // (token, auto-continued, start offset); a bare ID is a legacy token
        for (token, auto, start_ms) in [
            (ID.to_string(), false, 0),
            (new_token(ID), false, 0),
            (auto_token(ID), true, 0),
            (with_start_offset(&new_token(ID), 90_000), false, 90_000),
            (with_start_offset(&auto_token(ID), 5_000), true, 5_000),
        ] {
            assert_eq!(token_track_id(&token), ID, "{token}");
            assert_eq!(is_auto_token(&token), auto, "{token}");
            assert_eq!(token_start_offset(&token), start_ms, "{token}");
        }

        // Starting from the top leaves the token as it was
        let token = new_token(ID);
        assert_eq!(with_start_offset(&token, 0), token);
    }

    #[test]
    fn queue_item_serializes_flattened() {
        let item = QueueItem {
            entry: "dQw4w9WgXcQ#123".into(),
            track: AudioTrack {
                file_path: "/tmp/a.m4a".into(),
                ..AudioTrack::stub("dQw4w9WgXcQ")
            },
        };
        let v = serde_json::to_value(&item).unwrap();
        // Entry and track sit at the same level, and file_path stays internal
        assert_eq!(v["entry"], "dQw4w9WgXcQ#123");
        assert_eq!(v["id"], "dQw4w9WgXcQ");
        assert!(v.get("file_path").is_none());
    }

    #[test]
    fn pending_command_without_offset_defaults_to_zero() {
        // Commands queued before offset_ms existed must still parse
        let legacy = r#"{"action":"play","track":{"id":"dQw4w9WgXcQ","title":"t"}}"#;
        let cmd: PendingCommand = serde_json::from_str(legacy).unwrap();
        assert_eq!(cmd.offset_ms, 0);
    }

    /// A device playing a 10-second track at position 5s with last_update 100.0.
    fn playing_device() -> DeviceState {
        DeviceState {
            device_id: "d".into(),
            name: "n".into(),
            status: "playing".into(),
            current_track: Some(AudioTrack {
                duration: 10,
                ..AudioTrack::stub("dQw4w9WgXcQ")
            }),
            position_ms: 5_000,
            connected: true,
            last_update: 100.0,
        }
    }

    #[test]
    fn restore_folds_in_the_play_time_since_the_snapshot() {
        let mut dev = playing_device();
        let track = dev.current_track.clone();
        let prior = dev.prior();
        dev.apply(
            DeviceUpdate::new()
                .status("queued")
                .track(AudioTrack::stub("other"))
                .position(0),
            101.0,
        );
        dev.restore(prior, track.clone(), 103.0);
        assert_eq!(dev.status, "playing");
        assert_eq!(dev.current_track.map(|t| t.id), track.map(|t| t.id));
        assert_eq!(dev.position_ms, 8_000);
        assert_eq!(dev.last_update, 103.0);
    }

    #[test]
    fn the_estimate_only_moves_while_playing_and_clamps_to_duration() {
        let mut dev = playing_device();
        assert_eq!(dev.estimated_position_ms(102.0), 7_000);
        // Clamped at the end of the track
        assert_eq!(dev.estimated_position_ms(200.0), 10_000);

        // Nothing advances while not playing
        dev.status = "paused".into();
        assert_eq!(dev.estimated_position_ms(300.0), 5_000);
    }

    #[test]
    fn apply_keeps_position_consistent_with_last_update() {
        // Without an explicit position the estimate must not rewind
        let mut dev = playing_device();
        dev.apply(DeviceUpdate::new(), 102.0);
        assert_eq!(dev.position_ms, 7_000);
        assert_eq!(dev.last_update, 102.0);

        // An explicit one is adopted as given
        let mut dev = playing_device();
        dev.apply(DeviceUpdate::new().position(1_000), 102.0);
        assert_eq!(dev.position_ms, 1_000);
        assert_eq!(dev.last_update, 102.0);
    }

    #[test]
    fn failure_record_counts_consecutive_failures() {
        let mut rec = FailureRecord::default();

        // Consecutive as long as they happen at the same position
        assert_eq!(rec.record(5_000, 100.0), 1);
        assert_eq!(rec.record(5_000, 105.0), 2);
        assert_eq!(rec.record(5_000, 110.0), 3);

        // A later position proves a retry succeeded in between
        assert_eq!(rec.record(30_000, 115.0), 1);
        // One that has not advanced (a restart failure) is consecutive
        assert_eq!(rec.record(0, 120.0), 2);
        // So is time: past FAILURE_RESET_SECS the count starts over
        assert_eq!(rec.record(0, 120.0 + FAILURE_RESET_SECS + 1.0), 1);
    }

    #[test]
    fn download_status_serializes_lowercase() {
        // Pins the wire format to the frontend's status union in types.ts
        for (status, expected) in [
            (DownloadStatus::Metadata, "metadata"),
            (DownloadStatus::Downloading, "downloading"),
            (DownloadStatus::Processing, "processing"),
            (DownloadStatus::Error, "error"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(expected));
        }
    }
}
