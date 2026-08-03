//! What clients are told about the yt-dlp-backed work in flight, and the
//! cancellation generation that stops it.
//!
//! One entry per job, broadcast to every client as `downloads_update`. Every
//! kind of job reports through here — a download walks an entry from the
//! metadata stage to a percentage, a metadata refresh only opens and closes
//! one, a playlist import holds one until it has expanded into per-video
//! downloads — which is why nothing below knows what the work actually does.
//! A job is on display from the moment it is accepted rather than from its
//! first yt-dlp call, so what the user asked for is visible while it queues.
//! Kept in-process: a restart drops the map, and clients re-sync from the init
//! snapshot.

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
/// video ID keyspace. An implementation detail of the key: what the entry is
/// tracking reaches clients as `DownloadKind`, not as a prefix to parse off.
const PLAYLIST_PROGRESS_PREFIX: &str = "list:";

/// The progress key for a playlist import, the one job not named by a video ID.
pub(super) fn playlist_progress_key(list_id: &str) -> String {
    format!("{PLAYLIST_PROGRESS_PREFIX}{list_id}")
}

impl AppState {
    /// Mutate the download progress map and broadcast if changed.
    /// `f` returns whether a notification-worthy change was made, which is
    /// passed back for callers with their own bookkeeping to do only when the
    /// map really changed. The payload is built while holding the lock to avoid
    /// drift between state and broadcast.
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
    /// stage, and `title` is what the user sees until one of them moves on: a
    /// download has no title yet, so it shows the video ID until
    /// set_download_meta lands the real one, a metadata refresh has the stored
    /// title to show and never leaves this stage at all, and a playlist import
    /// shows the playlist ID until it has expanded into per-video jobs.
    ///
    /// Called as early as the job is known — before waiting for the video's
    /// slot, before the cache lookup — so a request the user has sent is on
    /// display for all of it. That the display lives here rather than in the
    /// client that asked is also what carries it across a reload: it comes back
    /// with the init snapshot, to every client.
    ///
    /// Yields None when the key already belongs to a job on display, which a
    /// second request for the same video must not reset. The entry is that
    /// job's to settle, so this one has nothing to close out.
    pub(super) async fn begin_progress(&self, key: &str, title: &str) -> Option<f64> {
        let started_at = now_f64();
        self.update_downloads(|downloads| insert_entry(downloads, key, title, started_at))
            .await
            .then_some(started_at)
    }

    /// Fold a finished job into the progress display: cleared once it completed
    /// or was stopped — a cancellation is not a failure to report — and kept as
    /// an error display for a while otherwise. Without a stamp from
    /// `begin_progress` there is no entry of this job's own, so nothing here is
    /// this job's to close out.
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

    /// Set the title after metadata fetch. Live streams are not downloaded
    /// (they leave the list immediately on registration), so advance status only for non-live.
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

    /// Update download percentage. yt-dlp emits progress lines at high frequency,
    /// so only broadcast when the integer part changes. At 100%, mark as processing
    /// (yt-dlp post-processing).
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

/// Start an entry for `key`, unless the key already belongs to a job that is
/// still running. What is left of a job that failed is a display, not an owner:
/// a retry takes that entry over, so its progress is the one shown and the
/// expiry the failure scheduled no longer applies to it. Whether the entry was
/// inserted.
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
            // The prefix that keeps a playlist import out of the video ID
            // keyspace is this module's business, so what the entry tracks is
            // published as a field the client can switch on.
            kind: match key.starts_with(PLAYLIST_PROGRESS_PREFIX) {
                true => DownloadKind::Playlist,
                false => DownloadKind::Video,
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
/// An entry belongs to the job that started it: once that job has settled and
/// another has taken the key, every write the first one still owes — the
/// outcome it is about to report, the error it will expire — is aimed at an
/// entry that is no longer its own.
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
