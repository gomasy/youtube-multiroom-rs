//! Reconciling the audio cache directory with the library: tracks whose file
//! is gone, and files no track claims. The filesystem work runs on a blocking
//! thread.

use super::model::{AudioTrack, CacheReport, OrphanFile, TrackJson};
use super::track::{cached_video_ids, mtime_f64};
use super::{AUDIO_EXT, AppState, now_f64};
use std::collections::HashSet;
use std::path::PathBuf;

/// Minimum age before an unclaimed cache file counts as garbage: a download
/// publishes its file moments before it registers the track.
const ORPHAN_MIN_AGE_SECS: f64 = 600.0;

/// One `{video_id}.m4a` in the cache directory.
struct CacheFile {
    id: String,
    bytes: u64,
    /// Seconds since it was last written. Unknown reads as zero — the value
    /// that keeps a file out of the orphan list rather than into it.
    age_secs: f64,
}

impl AppState {
    /// What the cache holds and how it differs from what the library expects:
    /// files claimed by nothing, and tracks whose file is gone.
    pub async fn cache_report(&self) -> CacheReport {
        let (tracks, files) = tokio::join!(self.list_tracks(), self.cache_files());

        let registered: HashSet<&str> = tracks.iter().map(|t| t.id.as_str()).collect();
        let cached: HashSet<&str> = files.iter().map(|f| f.id.as_str()).collect();

        let total_bytes = files.iter().map(|f| f.bytes).sum();
        let mut orphans: Vec<OrphanFile> = files
            .iter()
            .filter(|f| !registered.contains(f.id.as_str()) && f.age_secs > ORPHAN_MIN_AGE_SECS)
            .map(|f| OrphanFile {
                id: f.id.clone(),
                bytes: f.bytes,
            })
            .collect();
        // Largest first: the space a cleanup would reclaim is the reason to look.
        orphans.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));

        let missing = tracks
            .iter()
            .filter(|t| t.keeps_file() && !cached.contains(t.id.as_str()))
            .cloned()
            .collect();

        CacheReport {
            total_bytes,
            file_count: files.len(),
            orphans,
            missing,
        }
    }

    /// Delete the cache files nothing in the library claims, returning how many
    /// and how many bytes. Recomputed here: the report a client acts on may be
    /// stale.
    pub async fn remove_orphans(&self) -> (usize, u64) {
        let orphans = self.cache_report().await.orphans;
        let mut removed = 0;
        let mut freed = 0;
        for orphan in orphans {
            let path = self.cache_dir.join(format!("{}.{AUDIO_EXT}", orphan.id));
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    removed += 1;
                    freed += orphan.bytes;
                }
                Err(e) => tracing::warn!(
                    "Failed to remove the orphaned cache file {}: {e}",
                    path.display()
                ),
            }
        }
        if removed > 0 {
            tracing::info!("Removed {removed} orphaned cache file(s), freeing {freed} byte(s)");
        }
        (removed, freed)
    }

    /// Flag the tracks of a page whose cache file is gone. Only the page is
    /// stat'ed.
    pub async fn with_file_status(&self, tracks: Vec<AudioTrack>) -> Vec<TrackJson> {
        let missing = self.missing_files(&tracks).await;
        tracks
            .into_iter()
            .map(|track| TrackJson {
                file_missing: missing.contains(&track.id),
                track,
            })
            .collect()
    }

    /// The IDs of the given tracks whose `file_path` is not there.
    async fn missing_files(&self, tracks: &[AudioTrack]) -> HashSet<String> {
        let expected: Vec<(String, PathBuf)> = tracks
            .iter()
            .filter(|t| t.keeps_file())
            .map(|t| (t.id.clone(), PathBuf::from(&t.file_path)))
            .collect();
        if expected.is_empty() {
            return HashSet::new();
        }

        tokio::task::spawn_blocking(move || {
            expected
                .into_iter()
                // An empty path is a track registered without one, which is as
                // unplayable as a path pointing at nothing.
                .filter(|(_, path)| path.as_os_str().is_empty() || !path.exists())
                .map(|(id, _)| id)
                .collect()
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Cache check failed: {e}");
            HashSet::new()
        })
    }

    /// Every cached audio file, with its size and age.
    async fn cache_files(&self) -> Vec<CacheFile> {
        let cache_dir = self.cache_dir.clone();
        tokio::task::spawn_blocking(move || {
            let now = now_f64();
            cached_video_ids(&cache_dir)
                .into_iter()
                .filter_map(|(id, path)| {
                    let meta = std::fs::metadata(&path).ok()?;
                    let modified = mtime_f64(&meta).unwrap_or(now);
                    Some(CacheFile {
                        id,
                        bytes: meta.len(),
                        age_secs: (now - modified).max(0.0),
                    })
                })
                .collect()
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Cache scan failed: {e}");
            Vec::new()
        })
    }
}
