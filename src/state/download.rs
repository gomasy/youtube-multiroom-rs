//! Downloading audio with yt-dlp, and reporting progress to clients. Also
//! where a registered track's metadata is re-fetched, which is the same work
//! without the audio.
//!
//! A download writes into a staging directory owned by the attempt and is
//! published into the cache only once it is complete, so a failure or a stop
//! never leaves a partial file where it could be served.

use super::job::{VideoJob, Visited};
use super::model::{AudioTrack, DownloadProgress, DownloadStatus};
use super::url::extract_video_id;
use super::warn_redis;
use super::ytdlp::{
    DownloadError, PROGRESS_PREFIX, abort_reader, drain_output, fetch_metadata,
    parse_progress_percent, snippet, spawn_reader, spawn_yt_dlp, stop_yt_dlp,
};
use super::{AUDIO_EXT, AppState, REDIS_KEY_TRACKS, REDIS_KEY_TRACKS_ORDER, now_f64, watch_url};
use redis::AsyncCommands;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio::time;
use tokio_util::sync::CancellationToken;

/// Keep failed download progress visible for a while so reloading clients can
/// still see the error.
const DOWNLOAD_ERROR_TTL_SECS: u64 = 60;

/// Subdirectory of the cache holding one isolated directory per download
/// attempt. Nothing here is ever served, so a leftover from a crash is garbage.
const DOWNLOAD_STAGING_DIR: &str = ".downloads";
/// Tries before giving up on finding an unused staging directory name.
const STAGING_DIR_ATTEMPTS: u32 = 16;
/// Distinguishes concurrent staging directories for the same video.
static DOWNLOAD_ATTEMPT_SEQ: AtomicU64 = AtomicU64::new(0);

/// What the yt-dlp-backed work on one video — a download, a metadata refresh —
/// and a deletion of the track it registers use to stay out of each other's
/// way. Held in `AppState::extract_slots` for as long as any of them is
/// running.
#[derive(Default)]
pub(crate) struct ExtractSlot {
    /// Serializes the yt-dlp-backed work for one video — downloads and metadata
    /// refreshes alike — so the second caller finds the finished track in the
    /// cache instead of fetching it again.
    lock: Mutex<()>,
    /// Set by a deletion of this video's track. A writer that sees it at its
    /// commit point undoes its own registration instead of resurrecting what
    /// the user deleted — which is what lets a deletion return immediately
    /// rather than waiting out a fetch that can run for minutes.
    deleted: AtomicBool,
}

impl ExtractSlot {
    /// Mark the track deleted. Never unset: for the lifetime of this slot every
    /// registration of the video is one the deletion has already superseded.
    pub(crate) fn mark_deleted(&self) {
        self.deleted.store(true, Ordering::SeqCst);
    }

    fn is_deleted(&self) -> bool {
        self.deleted.load(Ordering::SeqCst)
    }
}

impl AppState {
    pub async fn extract_audio(
        self: &Arc<Self>,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        let video_id = extract_video_id(url).ok_or("Could not recognize YouTube URL")?;

        // Serialize concurrent requests for the same video. Subsequent callers
        // hit the cache check after acquiring the lock and return immediately.
        self.under_extract_slot(&video_id, cancel, async |slot| {
            self.extract_audio_locked(&video_id, slot, cancel).await
        })
        .await
    }

    /// Run `work` holding the video's extract slot, then release the slot.
    /// Every yt-dlp-backed operation on one video takes its turn here, so a
    /// download and a metadata refresh of the same video cannot interleave and
    /// leave the loser's fields written over the winner's.
    async fn under_extract_slot<F>(
        self: &Arc<Self>,
        video_id: &str,
        cancel: &CancellationToken,
        work: F,
    ) -> Result<AudioTrack, DownloadError>
    where
        // Lent rather than handed over, so `work` cannot outlive the guard and
        // release_extract_slot still sees only the references it counts on.
        F: AsyncFnOnce(&ExtractSlot) -> Result<AudioTrack, DownloadError>,
    {
        let slot = self.extract_slot(video_id).await;
        // Waiting behind another operation on the same video must stay
        // interruptible, and a cancellation that lands while queued still
        // applies once the lock is finally handed over.
        let guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            guard = slot.lock.lock() => Some(guard),
        };
        let result = match guard {
            Some(guard) if !cancel.is_cancelled() => {
                let result = work(&slot).await;
                drop(guard);
                result
            }
            _ => Err(DownloadError::Cancelled),
        };
        self.release_extract_slot(video_id, &slot).await;

        result
    }

    /// The slot coordinating this video's fetches and its deletion, created on
    /// first use. Always paired with `release_extract_slot`.
    pub(crate) async fn extract_slot(&self, video_id: &str) -> Arc<ExtractSlot> {
        let mut slots = self.extract_slots.lock().await;
        slots.entry(video_id.to_string()).or_default().clone()
    }

    /// Drop the per-video slot once this caller is the last user, so the map
    /// does not grow one entry per video for the lifetime of the process.
    pub(crate) async fn release_extract_slot(&self, video_id: &str, slot: &Arc<ExtractSlot>) {
        // The map and this caller account for two references. Any additional
        // reference belongs to another active or waiting caller.
        let mut slots = self.extract_slots.lock().await;
        if slots
            .get(video_id)
            .is_some_and(|current| Arc::ptr_eq(current, slot) && Arc::strong_count(slot) <= 2)
        {
            slots.remove(video_id);
        }
    }

    async fn extract_audio_locked(
        self: &Arc<Self>,
        video_id: &str,
        slot: &ExtractSlot,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        // This video is inside a deletion window: a deletion is running, or an
        // earlier download still owes the discard below. Either way anything
        // registered for it is on its way out, so a cache hit here would report
        // success for a track that is about to disappear, and a fresh download
        // would only be discarded. Report it the way a stop is reported; the
        // caller can ask again once the deletion has drained.
        if slot.is_deleted() {
            return Err(DownloadError::Cancelled);
        }

        // Redis cache check. Entries pointing to old formats (mp3 etc.) are
        // stale w.r.t. AUDIO_MIME, so skip them and re-fetch.
        if let Some(track) = self.get_track(video_id).await {
            // A stop can land during the Redis lookup above. The cache-miss
            // path re-checks inside fetch_and_register, so only the hit needs
            // its own check to avoid reporting success after a cancellation.
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
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

        // Fetch while broadcasting progress to all clients.
        self.begin_progress(video_id, video_id).await;
        let result = self.fetch_and_register(video_id, slot, cancel).await;
        self.settle_progress(video_id, &result).await;
        result
    }

    /// Fetch metadata → download (non-live) → register in Redis.
    /// Progress is reflected in the downloads progress entry.
    async fn fetch_and_register(
        &self,
        video_id: &str,
        slot: &ExtractSlot,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        let url = watch_url(video_id);
        // Fetch metadata
        tracing::info!("Fetching metadata: {}", video_id);
        let meta = fetch_metadata(&url, Some(cancel)).await?;
        let title = meta["title"].as_str().unwrap_or(video_id).to_string();
        let is_live = meta["is_live"].as_bool().unwrap_or(false);
        self.set_download_meta(video_id, &title, is_live).await;
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        // Live streams cannot be saved as files; register metadata only and
        // resolve the CDN URL at playback time (handlers::live_audio).
        let mut published_path = None;
        let track = if is_live {
            tracing::info!("Live stream detected, skipping download: {}", video_id);
            AudioTrack::from_meta(video_id, &meta, now_f64(), String::new())
        } else {
            let output_path = self.cache_dir.join(format!("{video_id}.{AUDIO_EXT}"));
            let output_str = output_path.to_string_lossy().to_string();
            let staging_dir = self.create_staging_dir(video_id).await?;
            let staged_path = staging_dir.join(format!("{video_id}.{AUDIO_EXT}"));

            // Download into the staging directory, then publish. Nothing this
            // attempt wrote is visible until publishing succeeds, so a failure
            // or cancellation only has to discard the whole directory.
            tracing::info!("Downloading: {}", title);
            let published = match self
                .run_download(video_id, &url, &staged_path, cancel)
                .await
            {
                Ok(()) => {
                    self.publish_download(cancel, &staged_path, &output_path)
                        .await
                }
                // Cancellation and failure both leave partial files behind,
                // and both are discarded with the staging directory below.
                Err(e) => Err(e),
            };
            remove_staging_dir(&staging_dir).await;
            published?;
            published_path = Some(output_path);

            AudioTrack::from_meta(video_id, &meta, now_f64(), output_str)
        };

        // Registration is the step that makes the track visible, so a failure
        // here must surface as an error rather than reporting a phantom success.
        let json_str = match track.to_redis_json() {
            Ok(json) => json,
            Err(e) => {
                if let Some(path) = &published_path {
                    let _ = tokio::fs::remove_file(path).await;
                }
                return Err(DownloadError::Failed(format!(
                    "Failed to serialize track: {e}"
                )));
            }
        };
        if is_live {
            // Live tracks never publish a file, so their commit point is this
            // check instead. The guard is released before the Redis call below:
            // holding it across Redis I/O would let a stalled Redis delay
            // stopping unrelated yt-dlp processes.
            let _commit_guard = self.download_cancel.lock().await;
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
        }
        // Past this point the track is committed. Redis may have applied HSET
        // even if the response was lost, so a published file is deliberately
        // left in place: a registered track must never point at a missing
        // cache, and the cache scanner can recover it if HSET truly failed.
        let mut conn = self.redis.clone();
        conn.hset::<_, _, _, ()>(REDIS_KEY_TRACKS, video_id, json_str)
            .await
            .map_err(|e| DownloadError::Failed(format!("Failed to register track: {e}")))?;
        // Prepend to the order list (remove first to avoid duplicates on re-fetch).
        // Order is cosmetic — list_tracks appends unordered tracks — so failures
        // here are logged rather than failing the whole fetch.
        {
            let _guard = self.order_lock.lock().await;
            warn_redis!(
                "removing {video_id} from track order",
                conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, video_id).await
            );
            warn_redis!(
                "prepending {video_id} to track order",
                conn.lpush(REDIS_KEY_TRACKS_ORDER, video_id).await
            );
        }

        // A deletion that started while this download was running has already
        // removed the entry this attempt was replacing, so the writes above put
        // back a track the user deleted. Undoing them here — rather than making
        // the deletion wait for this download — is what keeps deletion prompt.
        // The mark is only ever set, so this cannot miss one that lands later:
        // such a deletion runs entirely after these writes and removes them.
        if slot.is_deleted() {
            self.discard_registration(video_id, published_path.as_deref())
                .await;
            return Err(DownloadError::Cancelled);
        }

        tracing::info!("Ready: {} ({}s)", track.title, track.duration);
        Ok(track)
    }

    /// Undo this attempt's registration after a concurrent deletion, leaving
    /// nothing the deletion itself would no longer know to clean up.
    async fn discard_registration(&self, video_id: &str, published_path: Option<&Path>) {
        // File first, for the same reason remove_track deletes it first: were
        // the HDEL below to empty the tracks key, a surviving file could have
        // the track restored from it by restore_tracks_if_missing.
        if let Some(path) = published_path {
            let _ = tokio::fs::remove_file(path).await;
        }
        let mut conn = self.redis.clone();
        warn_redis!(
            "discarding track {video_id}",
            conn.hdel(REDIS_KEY_TRACKS, video_id).await
        );
        let _guard = self.order_lock.lock().await;
        warn_redis!(
            "removing {video_id} from track order",
            conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, video_id).await
        );
        tracing::info!("Discarded {video_id}: deleted while its registration was in flight");
    }

    // ── Metadata refresh ──

    /// Re-fetch what YouTube says about tracks already in the library, one at a
    /// time in the background. Each track costs a yt-dlp run, so a bulk refresh
    /// takes far longer than a request may wait: this returns as soon as the
    /// job is spawned, and its result reaches clients as the track list
    /// changing underneath them.
    ///
    /// Progress is reported over the same downloads_update channel a download
    /// uses, which is also what makes Stop all end a refresh.
    pub fn start_metadata_refresh(
        self: &Arc<Self>,
        video_ids: Vec<String>,
        cancel: CancellationToken,
    ) {
        let state = self.clone();
        tokio::spawn(async move {
            let job = VideoJob::new(
                "Metadata refresh".to_string(),
                "refreshed",
                "deleted while fetching",
            );
            let (state, cancel) = (&state, &cancel);
            job.run(&video_ids, cancel, |video_id| async move {
                let track = state.refresh_video_metadata(&video_id, cancel).await?;
                // Reflect each refreshed track as it lands, so a long job is
                // visibly making progress in the track list.
                state.broadcast_tracks();
                tracing::info!("Metadata refreshed: {} ({video_id})", track.title);
                Ok(Visited::Done)
            })
            .await;
        });
    }

    /// Refresh one registered track's metadata, under the same slot a download
    /// of the video would take.
    async fn refresh_video_metadata(
        self: &Arc<Self>,
        video_id: &str,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        self.under_extract_slot(video_id, cancel, async |slot| {
            self.refresh_metadata_locked(video_id, slot, cancel).await
        })
        .await
    }

    async fn refresh_metadata_locked(
        self: &Arc<Self>,
        video_id: &str,
        slot: &ExtractSlot,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        // Inside a deletion window: whatever is registered for this video is on
        // its way out, so refreshing it would only rewrite an entry the deletion
        // has already superseded. Reported the way a stop is.
        if slot.is_deleted() {
            return Err(DownloadError::Cancelled);
        }
        // Only a registered track can be refreshed — there is no file to fetch
        // here, so an unknown ID cannot be turned into one by trying.
        let Some(existing) = self.get_track(video_id).await else {
            return Err("Track not found".into());
        };

        // Shown under its current title while the fetch runs: it is the only
        // name the user has for the track being worked on, stale or not.
        self.begin_progress(video_id, &existing.title).await;
        let result = self
            .fetch_and_rewrite_metadata(&existing, slot, cancel)
            .await;
        self.settle_progress(video_id, &result).await;
        result
    }

    /// Fetch metadata for a registered track and write it over the stored entry.
    async fn fetch_and_rewrite_metadata(
        &self,
        existing: &AudioTrack,
        slot: &ExtractSlot,
        cancel: &CancellationToken,
    ) -> Result<AudioTrack, DownloadError> {
        let video_id = &existing.id;
        tracing::info!("Refreshing metadata: {video_id}");
        let meta = fetch_metadata(&watch_url(video_id), Some(cancel)).await?;

        // What YouTube reports about the video is replaced; what describes the
        // copy held locally is carried over. `file_path` and `created_at` are
        // this attempt's to preserve rather than re-derive, and `is_live`
        // decides whether playback serves the cached file or resolves a live
        // URL — a stream that has since ended reports itself as a normal video,
        // and adopting that would leave an entry claiming a file it never had.
        let track = AudioTrack {
            is_live: existing.is_live,
            ..AudioTrack::from_meta(
                video_id,
                &meta,
                existing.created_at,
                existing.file_path.clone(),
            )
        };
        let json_str = track
            .to_redis_json()
            .map_err(|e| DownloadError::Failed(format!("Failed to serialize track: {e}")))?;

        // No commit guard here, unlike a live registration: this write only
        // refreshes an entry that already existed, so a stop racing it cannot
        // leave anything behind. The check just spares Redis a pointless write.
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let mut conn = self.redis.clone();
        conn.hset::<_, _, _, ()>(REDIS_KEY_TRACKS, video_id, json_str)
            .await
            .map_err(|e| DownloadError::Failed(format!("Failed to update track: {e}")))?;

        // A deletion that started while yt-dlp was running has already removed
        // the entry this write just put back. Undo it here for the same reason
        // fetch_and_register does — the deletion never waits for the fetch —
        // and leave the file alone: it belongs to the deletion, which removed
        // it before it could ever be reached from Redis.
        if slot.is_deleted() {
            self.discard_registration(video_id, None).await;
            return Err(DownloadError::Cancelled);
        }
        Ok(track)
    }

    /// Create an empty directory owned by a single download attempt. The name
    /// only has to be unique among live attempts; the retry covers a leftover
    /// directory from a crashed run that happens to collide.
    async fn create_staging_dir(&self, video_id: &str) -> Result<PathBuf, String> {
        let root = self.cache_dir.join(DOWNLOAD_STAGING_DIR);
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| format!("Failed to create download staging directory: {e}"))?;

        for _ in 0..STAGING_DIR_ATTEMPTS {
            let seq = DOWNLOAD_ATTEMPT_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("{video_id}-{}-{seq:x}", std::process::id()));
            match tokio::fs::create_dir(&path).await {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Failed to create download staging directory: {e}")),
            }
        }
        Err("Failed to allocate a download staging directory".to_string())
    }

    /// Move a finished download into the cache under the cancellation lock, so
    /// cancellation and publication cannot interleave: either the stop wins and
    /// no file appears, or the file is already published and stays.
    async fn publish_download(
        &self,
        cancel: &CancellationToken,
        staged_path: &Path,
        output_path: &Path,
    ) -> Result<(), DownloadError> {
        let _commit_guard = self.download_cancel.lock().await;
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        Ok(link_into_cache(staged_path, output_path).await?)
    }

    /// Download audio with yt-dlp into `staged_path`. Read progress lines from
    /// stdout and update the download progress entry with the percentage until
    /// done. `staged_path` is an absolute path inside the attempt's staging
    /// directory, so yt-dlp's .part file and post-processing intermediates are
    /// written there too and never touch the served cache.
    async fn run_download(
        &self,
        video_id: &str,
        url: &str,
        staged_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), DownloadError> {
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let staged_str = staged_path.to_string_lossy().to_string();
        // Prefer AAC source so no re-encode is needed for AUDIO_EXT (remux only)
        let format_spec = format!("bestaudio[ext={AUDIO_EXT}]/bestaudio");
        // Have yt-dlp emit progress in a machine-readable one-line-per-update format
        let progress_template = format!("download:{PROGRESS_PREFIX}%(progress._percent_str)s");
        let mut child = spawn_yt_dlp([
            "-f",
            &format_spec,
            "-x",
            "--audio-format",
            AUDIO_EXT,
            "-o",
            &staged_str,
            "--no-playlist",
            "--newline",
            "--progress-template",
            &progress_template,
            url,
        ])
        .map_err(|e| format!("Download error: {e}"))?;

        // Drain stderr in a separate task to prevent pipe-full blocking;
        // its content is only used for error messages.
        let stderr_task = child.stderr.take().map(spawn_reader);

        // Read progress lines as raw bytes with lossy conversion. lines() would
        // error on non-UTF-8, stopping the reader and closing the pipe, which
        // causes yt-dlp to die with EPIPE.
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                let read = tokio::select! {
                    read = reader.read_until(b'\n', &mut buf) => read,
                    _ = cancel.cancelled() => {
                        stop_yt_dlp(&mut child).await;
                        abort_reader(stderr_task);
                        return Err(DownloadError::Cancelled);
                    }
                };
                match read {
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

        let status = tokio::select! {
            result = child.wait() => result.map_err(|e| format!("Download error: {e}"))?,
            _ = cancel.cancelled() => {
                stop_yt_dlp(&mut child).await;
                abort_reader(stderr_task);
                return Err(DownloadError::Cancelled);
            }
        };
        if !status.success() {
            // Only the failure path needs stderr, and draining it is bounded
            // (descendants can keep the pipe open), so success must not pay it.
            let stderr_buf = match stderr_task {
                Some(task) => drain_output(task).await.unwrap_or_default(),
                None => Vec::new(),
            };
            return Err(DownloadError::Failed(format!(
                "Failed to download audio: {}",
                snippet(&String::from_utf8_lossy(&stderr_buf))
            )));
        }
        Ok(())
    }

    // ── Download progress ──

    /// Mutate the download progress map and broadcast if changed.
    /// `f` returns whether a notification-worthy change was made. The payload
    /// is built while holding the lock to avoid drift between state and broadcast.
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

    /// Mutate a single download entry (no-op if not found).
    /// `f` returns whether a notification-worthy change was made.
    async fn update_download(&self, video_id: &str, f: impl FnOnce(&mut DownloadProgress) -> bool) {
        self.update_downloads(|downloads| downloads.get_mut(video_id).is_some_and(f))
            .await;
    }

    /// Register a job in the progress map and broadcast it. Both kinds start at
    /// the metadata stage, and `title` is what the user sees until one of them
    /// moves on: a download has no title yet, so it shows the video ID until
    /// set_download_meta lands the real one, while a metadata refresh has the
    /// stored title to show and never leaves this stage at all.
    async fn begin_progress(&self, video_id: &str, title: &str) {
        self.update_downloads(|downloads| {
            downloads.insert(
                video_id.to_string(),
                DownloadProgress {
                    id: video_id.to_string(),
                    title: title.to_string(),
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

    /// Fold a finished job into the progress display: cleared once it completed
    /// or was stopped — a cancellation is not a failure to report — and kept as
    /// an error display for a while otherwise.
    async fn settle_progress(
        self: &Arc<Self>,
        video_id: &str,
        result: &Result<AudioTrack, DownloadError>,
    ) {
        match result {
            Ok(_) | Err(DownloadError::Cancelled) => self.finish_download(video_id).await,
            Err(e) => self.fail_download(video_id, &e.to_string()).await,
        }
    }

    /// Set the title after metadata fetch. Live streams are not downloaded
    /// (they leave the list immediately on registration), so advance status only for non-live.
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

    /// Update download percentage. yt-dlp emits progress lines at high frequency,
    /// so only broadcast when the integer part changes. At 100%, mark as processing
    /// (yt-dlp post-processing).
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

    /// Remove a finished download from the progress map and broadcast.
    async fn finish_download(&self, video_id: &str) {
        self.update_downloads(|downloads| downloads.remove(video_id).is_some())
            .await;
    }

    /// Switch a failed download entry to error display, then clean it up after TTL.
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
                    // Don't remove a newer entry created by a retry (identified by started_at)
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

    /// Token for work starting now. Capture it before spawning so a stop that
    /// arrives immediately afterwards still cancels the request.
    pub async fn download_token(&self) -> CancellationToken {
        self.download_cancel.lock().await.clone()
    }

    /// Drop staging directories orphaned by a crash or a hard kill. Only safe
    /// at startup, when no download of this process owns one yet.
    pub async fn clear_download_staging(&self) {
        let root = self.cache_dir.join(DOWNLOAD_STAGING_DIR);
        if let Err(e) = tokio::fs::remove_dir_all(&root).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "Failed to clear download staging directory {}: {e}",
                root.display()
            );
        }
    }

    /// Cancel all work started under the current token. Each active yt-dlp
    /// process observes this token, stops its process group, and removes only its
    /// own staging directory before returning.
    pub async fn cancel_downloads(&self) {
        // The guard is held until the stale progress snapshot has been cleared,
        // so a download that already picked up the replacement token cannot
        // have its progress entry erased by this call.
        let mut token = self.download_cancel.lock().await;
        token.cancel();
        *token = CancellationToken::new();
        self.update_downloads(|downloads| {
            if downloads.is_empty() {
                return false;
            }
            downloads.clear();
            true
        })
        .await;
    }

    /// Return in-progress downloads sorted by start time (wire format for init / downloads_update).
    pub async fn downloads_json(&self) -> Value {
        Self::downloads_payload(&*self.downloads.lock().await)
    }

    fn downloads_payload(downloads: &HashMap<String, DownloadProgress>) -> Value {
        let mut list: Vec<&DownloadProgress> = downloads.values().collect();
        list.sort_by(|a, b| a.started_at.total_cmp(&b.started_at));
        json!(list)
    }
}

/// Discard one attempt's staging directory along with any partial or
/// post-processing files yt-dlp left in it.
async fn remove_staging_dir(path: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            "Failed to remove download staging directory {}: {e}",
            path.display()
        );
    }
}

/// Publish a staged file into the cache. Hard linking is atomic and fails
/// rather than clobbering an existing cache entry, so a download that finishes
/// alongside an already-cached copy can never truncate what is being served.
async fn link_into_cache(staged_path: &Path, output_path: &Path) -> Result<(), String> {
    tokio::fs::hard_link(staged_path, output_path)
        .await
        .map_err(|e| format!("Failed to publish downloaded audio: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staging_test_dir(label: &str) -> PathBuf {
        let seq = DOWNLOAD_ATTEMPT_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "youtube-multiroom-{label}-{}-{seq}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn publishing_download_never_overwrites_existing_cache() {
        let root = staging_test_dir("publish");
        let attempt = root.join("attempt");
        tokio::fs::create_dir_all(&attempt).await.unwrap();
        let staged = attempt.join("track.m4a");
        let output = root.join("track.m4a");
        tokio::fs::write(&staged, b"new").await.unwrap();
        tokio::fs::write(&output, b"existing").await.unwrap();

        assert!(link_into_cache(&staged, &output).await.is_err());
        assert_eq!(tokio::fs::read(&output).await.unwrap(), b"existing");

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn staging_cleanup_only_removes_the_target_attempt() {
        let root = staging_test_dir("cleanup");
        let attempt = root.join("attempt-a");
        let other = root.join("attempt-b");
        tokio::fs::create_dir_all(&attempt).await.unwrap();
        tokio::fs::create_dir_all(&other).await.unwrap();
        tokio::fs::write(attempt.join("track.m4a.part"), b"partial")
            .await
            .unwrap();
        tokio::fs::write(other.join("keep.m4a.part"), b"other")
            .await
            .unwrap();

        remove_staging_dir(&attempt).await;

        assert!(!attempt.exists());
        assert_eq!(
            tokio::fs::read(other.join("keep.m4a.part")).await.unwrap(),
            b"other"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
