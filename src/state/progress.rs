//! What clients are told about the yt-dlp-backed work in flight, and the
//! cancellation generation that stops it.
//!
//! One entry per video, broadcast to every client as `downloads_update`. Both
//! kinds of per-video work report through here — a download walks an entry from
//! the metadata stage to a percentage, a metadata refresh only opens and closes
//! one — which is why nothing below knows what the work actually does. Kept
//! in-process: a restart drops the map, and clients re-sync from the init
//! snapshot.

use super::model::{AudioTrack, DownloadProgress, DownloadStatus};
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

impl AppState {
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
    pub(super) async fn begin_progress(&self, video_id: &str, title: &str) {
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
    pub(super) async fn settle_progress(
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
