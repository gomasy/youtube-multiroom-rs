//! What clients are told about the yt-dlp-backed work in flight, and the
//! cancellation generation that stops it.
//!
//! One entry per job, broadcast to every client as `downloads_update`. Nothing
//! here knows what the work actually is: a download walks its entry from the
//! metadata stage to a percentage, a metadata refresh only opens and closes
//! one, a playlist import holds one until it has expanded into per-video jobs.
//! An entry opens the moment the job is accepted rather than at its first
//! yt-dlp call, so what the user asked for is visible while it queues. Kept
//! in-process: a restart drops the map and clients re-sync from `init`.

use super::model::{DownloadKind, DownloadProgress, DownloadStatus};
use super::ytdlp::DownloadError;
use super::{AppState, now_f64};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time;
use tokio_util::sync::CancellationToken;

/// Keep failed download progress visible for a while so reloading clients can
/// still see the error.
const DOWNLOAD_ERROR_TTL_SECS: u64 = 60;

/// Keeps a playlist import that is still resolving its video list out of the
/// video ID keyspace. Internal to the key: clients learn what an entry tracks
/// from `DownloadKind`, not by parsing a prefix off.
const PLAYLIST_PROGRESS_PREFIX: &str = "list:";

/// The progress key for a playlist import, the one job not named by a video ID.
pub(super) fn playlist_progress_key(list_id: &str) -> String {
    format!("{PLAYLIST_PROGRESS_PREFIX}{list_id}")
}

impl AppState {
    /// Mutate the download progress map and broadcast if `f` reports a
    /// notification-worthy change; that verdict is passed back for callers with
    /// their own bookkeeping to do. The payload is built under the lock so the
    /// broadcast cannot drift from the state.
    async fn update_downloads(
        &self,
        f: impl FnOnce(&mut HashMap<String, DownloadProgress>) -> bool,
    ) -> bool {
        let payload = {
            let mut downloads = self.downloads.lock().await;
            if !f(&mut downloads) {
                return false;
            }
            Self::downloads_payload(&downloads)
        };
        self.broadcast(json!({ "type": "downloads_update", "downloads": payload }));
        true
    }

    /// Mutate a single download entry (no-op if not found).
    /// `f` returns whether a notification-worthy change was made.
    async fn update_download(&self, video_id: &str, f: impl FnOnce(&mut DownloadProgress) -> bool) {
        self.update_downloads(|downloads| downloads.get_mut(video_id).is_some_and(f))
            .await;
    }

    /// Put a job on display and return the stamp identifying its entry, which
    /// `settle_progress` needs to close it out. Jobs start at the metadata
    /// stage showing `title` — the video ID for a download until
    /// set_download_meta lands the real one, the stored title for a metadata
    /// refresh, the playlist ID for an import.
    ///
    /// Called as early as the job is known, before the slot wait and the cache
    /// lookup, so a request is on display for all of it. Living here rather
    /// than in the client that asked is what carries it across a reload.
    ///
    /// None when the key already belongs to a job on display: that entry is the
    /// other job's to settle, and a second request must not reset it.
    pub(super) async fn begin_progress(&self, key: &str, title: &str) -> Option<f64> {
        let started_at = now_f64();
        self.update_downloads(|downloads| insert_entry(downloads, key, title, started_at))
            .await
            .then_some(started_at)
    }

    /// Fold a finished job into the display: cleared once it completed or was
    /// stopped — a cancellation is not a failure to report — and kept on
    /// display as an error for a while otherwise. Without a stamp from
    /// `begin_progress` this job owns no entry and has nothing to close out.
    pub(super) async fn settle_progress<T>(
        self: &Arc<Self>,
        key: &str,
        stamp: Option<f64>,
        result: &Result<T, DownloadError>,
    ) {
        let Some(started_at) = stamp else {
            return;
        };
        match result {
            Ok(_) | Err(DownloadError::Cancelled) => self.finish_download(key, started_at).await,
            Err(e) => self.fail_download(key, started_at, &e.to_string()).await,
        }
    }

    /// Set the title after the metadata fetch. Live streams are never
    /// downloaded, so only a non-live job advances past the metadata stage.
    pub(super) async fn set_download_meta(&self, video_id: &str, title: &str, is_live: bool) {
        self.update_download(video_id, |d| {
            d.title = title.to_string();
            if !is_live {
                d.status = DownloadStatus::Downloading;
            }
            true
        })
        .await;
    }

    /// Update the percentage. yt-dlp emits progress lines at high frequency, so
    /// only a change in the integer part is broadcast. 100% means the remaining
    /// work is post-processing and the container rebuild.
    pub(super) async fn set_download_percent(&self, video_id: &str, percent: f64) {
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

    /// Remove a finished job from the progress map and broadcast.
    async fn finish_download(&self, key: &str, started_at: f64) {
        self.update_downloads(|downloads| {
            owned_entry(downloads, key, started_at).is_some() && downloads.remove(key).is_some()
        })
        .await;
    }

    /// Switch a failed job's entry to error display, then clean it up after TTL.
    async fn fail_download(self: &Arc<Self>, key: &str, started_at: f64, error: &str) {
        let failed = self
            .update_downloads(|downloads| {
                let Some(entry) = owned_entry(downloads, key, started_at) else {
                    return false;
                };
                entry.status = DownloadStatus::Error;
                entry.error = Some(error.to_string());
                true
            })
            .await;
        if failed {
            self.expire_download_error(key, started_at);
        }
    }

    /// Drop an error entry once it has been on display long enough.
    fn expire_download_error(self: &Arc<Self>, key: &str, started_at: f64) {
        let state = self.clone();
        let key = key.to_string();
        tokio::spawn(async move {
            time::sleep(time::Duration::from_secs(DOWNLOAD_ERROR_TTL_SECS)).await;
            state
                .update_downloads(|downloads| {
                    owned_entry(downloads, &key, started_at).is_some()
                        && downloads.remove(&key).is_some()
                })
                .await;
        });
    }

    /// Token for work starting now. Capture it before spawning so a stop that
    /// arrives immediately afterwards still cancels the request.
    pub async fn download_token(&self) -> CancellationToken {
        self.download_cancel.lock().await.clone()
    }

    /// Cancel all work started under the current token. Each active yt-dlp run
    /// observes it, stops its process group, and removes only its own staging
    /// directory before returning.
    pub async fn cancel_downloads(&self) {
        // Held until the stale progress snapshot is cleared, so a download that
        // already picked up the replacement token keeps its entry.
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

/// Start an entry for `key` unless a still-running job already owns it,
/// reporting whether one was inserted. What a failed job left behind is a
/// display, not an owner: a retry takes the entry over, so its progress is what
/// is shown and the expiry the failure scheduled no longer applies.
fn insert_entry(
    downloads: &mut HashMap<String, DownloadProgress>,
    key: &str,
    title: &str,
    started_at: f64,
) -> bool {
    let running = downloads
        .get(key)
        .is_some_and(|entry| !matches!(entry.status, DownloadStatus::Error));
    if running {
        return false;
    }
    downloads.insert(
        key.to_string(),
        DownloadProgress {
            id: key.to_string(),
            kind: if key.starts_with(PLAYLIST_PROGRESS_PREFIX) {
                DownloadKind::Playlist
            } else {
                DownloadKind::Video
            },
            title: title.to_string(),
            status: DownloadStatus::Metadata,
            percent: 0.0,
            error: None,
            started_at,
        },
    );
    true
}

/// The entry under `key`, while it is still the one `started_at` identifies.
/// An entry belongs to the job that started it: once another job has taken the
/// key, every write the first still owes — the outcome it reports, the error it
/// expires — is aimed at an entry that is no longer its own.
fn owned_entry<'a>(
    downloads: &'a mut HashMap<String, DownloadProgress>,
    key: &str,
    started_at: f64,
) -> Option<&'a mut DownloadProgress> {
    downloads
        .get_mut(key)
        .filter(|entry| entry.started_at == started_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(key: &str, started_at: f64) -> HashMap<String, DownloadProgress> {
        let mut downloads = HashMap::new();
        assert!(insert_entry(&mut downloads, key, key, started_at));
        downloads
    }

    #[test]
    fn a_second_request_does_not_reset_the_job_on_display() {
        let mut downloads = map_with("vid", 100.0);
        let entry = downloads.get_mut("vid").unwrap();
        entry.status = DownloadStatus::Downloading;
        entry.percent = 42.0;

        assert!(!insert_entry(&mut downloads, "vid", "vid", 101.0));
        let entry = &downloads["vid"];
        assert_eq!(entry.percent, 42.0);
        assert_eq!(entry.started_at, 100.0);
    }

    #[test]
    fn a_retry_takes_over_what_a_failure_left_on_display() {
        let mut downloads = map_with("vid", 100.0);
        let entry = downloads.get_mut("vid").unwrap();
        entry.status = DownloadStatus::Error;
        entry.error = Some("boom".to_string());

        assert!(insert_entry(&mut downloads, "vid", "vid", 101.0));
        let entry = &downloads["vid"];
        assert_eq!(entry.started_at, 101.0);
        assert!(entry.error.is_none());
        // The failure's expiry now points at an entry that is not its own.
        assert!(owned_entry(&mut downloads, "vid", 100.0).is_none());
    }

    #[test]
    fn an_entry_belongs_to_the_job_that_started_it() {
        let mut downloads = map_with("vid", 100.0);
        assert!(owned_entry(&mut downloads, "vid", 100.0).is_some());
        // What a retry after this job settled looks like: same key, new stamp.
        assert!(owned_entry(&mut downloads, "vid", 99.0).is_none());
        assert!(owned_entry(&mut downloads, "other", 100.0).is_none());
    }

    #[test]
    fn a_playlist_import_is_told_apart_by_its_key() {
        let downloads = map_with(&playlist_progress_key("PL0123"), 100.0);
        let entry = downloads.values().next().unwrap();
        assert!(matches!(entry.kind, DownloadKind::Playlist));
        assert!(matches!(
            map_with("dQw4w9WgXcQ", 100.0)["dQw4w9WgXcQ"].kind,
            DownloadKind::Video
        ));
    }
}
