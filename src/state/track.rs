//! The audio library: registering tracks, keeping their order, and choosing
//! what plays next.

use super::model::{AudioTrack, ReorderOutcome};
use super::url::is_video_id;
use super::warn_redis;
use super::ytdlp::fetch_metadata;
use super::{
    AUDIO_EXT, AppState, PendingCommand, REDIS_KEY_TRACKS, REDIS_KEY_TRACKS_ORDER,
    REDIS_PENDING_PREFIX, now_f64, playlist_key, queue_key, token_track_id,
};
use redis::AsyncCommands;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

impl AppState {
    pub async fn get_track(&self, id: &str) -> Option<AudioTrack> {
        self.try_get_track(id).await.ok().flatten()
    }

    /// Get a track, distinguishing Redis errors (Err) from not-found (Ok(None)).
    /// Callers that need to decide "safe to delete if missing" should use this.
    /// Unparsable entries are treated as not-found.
    pub(crate) async fn try_get_track(&self, id: &str) -> redis::RedisResult<Option<AudioTrack>> {
        let mut conn = self.redis.clone();
        let json_str: Option<String> = conn.hget(REDIS_KEY_TRACKS, id).await?;
        Ok(json_str.and_then(|s| AudioTrack::from_redis_json(&s)))
    }

    pub async fn remove_track(&self, id: &str) -> Option<AudioTrack> {
        let track = self.get_track(id).await?;

        // Delete the file first. If removing the last track causes the tracks key
        // to vanish, restore_tracks_if_missing may run — and if the file still
        // exists, the deleted track would be resurrected.
        if !track.file_path.is_empty() {
            let _ = tokio::fs::remove_file(&track.file_path).await;
        }

        let mut conn = self.redis.clone();
        warn_redis!("deleting track {id}", conn.hdel(REDIS_KEY_TRACKS, id).await);
        self.unlink_track_from_order_and_playlists(id).await;
        self.clear_pending_referencing(id).await;
        self.detach_track_from_devices(id).await;

        Some(track)
    }

    /// Drop a deleted track from the library order and from every playlist that
    /// listed it.
    async fn unlink_track_from_order_and_playlists(&self, id: &str) {
        // Serialize with reorder's read-then-replace to prevent a deletion
        // from being silently undone by a concurrent reorder write-back.
        let _guard = self.order_lock.lock().await;
        let mut conn = self.redis.clone();
        warn_redis!(
            "removing {id} from track order",
            conn.lrem(REDIS_KEY_TRACKS_ORDER, 0, id).await
        );

        // Remove from all playlist track lists in a single pipeline round-trip
        let playlists = self.playlists().await;
        if playlists.is_empty() {
            return;
        }
        let mut pipe = redis::pipe();
        for playlist in &playlists {
            pipe.lrem(playlist_key(&playlist.id), 0, id).ignore();
        }
        if let Err(e) = pipe.query_async::<()>(&mut conn).await {
            tracing::warn!("Redis error removing track {id} from playlists: {e}");
        }
    }

    /// Discard queued play commands that would resurrect a deleted track.
    async fn clear_pending_referencing(&self, id: &str) {
        let mut conn = self.redis.clone();
        for key in self.pending_keys().await {
            let json_str: Option<String> = match conn.get(&key).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Redis error reading pending command {key}: {e}");
                    continue;
                }
            };
            if json_str
                .and_then(|s| serde_json::from_str::<PendingCommand>(&s).ok())
                .is_some_and(|cmd| cmd.track.id == id)
            {
                warn_redis!("clearing pending command {key}", conn.del(&key).await);
            }
        }
    }

    /// Every pending-command key currently in Redis. A partial scan is returned
    /// as-is: the caller only ever deletes what it finds, so seeing fewer keys
    /// leaves stale entries behind rather than removing the wrong ones.
    async fn pending_keys(&self) -> Vec<String> {
        let mut conn = self.redis.clone();
        let pattern = format!("{REDIS_PENDING_PREFIX}:*");
        let mut iter = match conn.scan_match::<_, String>(&pattern).await {
            Ok(iter) => iter,
            Err(e) => {
                tracing::warn!("Redis error scanning pending commands: {e}");
                return Vec::new();
            }
        };
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

    /// Remove a deleted track from every device's Up Next queue, and clear it
    /// from any device that was pointing at it.
    async fn detach_track_from_devices(&self, id: &str) {
        let mut conn = self.redis.clone();
        for mut dev in self.all_devices().await.into_values() {
            let key = queue_key(&dev.device_id);
            let entries: Vec<String> = conn.lrange(&key, 0, -1).await.unwrap_or_else(|e| {
                tracing::warn!("Redis error reading queue for {}: {e}", dev.device_id);
                Vec::new()
            });
            for entry in entries.iter().filter(|e| token_track_id(e) == id) {
                warn_redis!(
                    "removing queue entry {entry}",
                    conn.lrem(&key, 0, entry).await
                );
            }
            if dev.current_track.as_ref().is_some_and(|t| t.id == id) {
                dev.current_track = None;
                dev.status = "idle".to_string();
                self.write_device(&dev).await;
            }
        }
    }

    /// Return all tracks in saved order. Tracks not in the order list
    /// (pre-reorder data or freshly restored) are appended newest-first.
    pub async fn list_tracks(&self) -> Vec<AudioTrack> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> =
            conn.hgetall(REDIS_KEY_TRACKS).await.unwrap_or_else(|e| {
                tracing::warn!("Redis error reading tracks: {e}");
                HashMap::new()
            });
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

    /// Move a track to new_index (0-based) and persist. If playlist_id is given,
    /// reorder within that playlist; otherwise reorder the entire library.
    pub async fn reorder_track(
        &self,
        playlist_id: Option<&str>,
        track_id: &str,
        new_index: usize,
    ) -> ReorderOutcome {
        // Serialize to prevent interleaving changes between read and full replace
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

        // Rewrite the entire order list (at most a few hundred items, so full replace is fine)
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

    /// If the youtube:tracks key is missing in Redis (e.g. after a fresh init),
    /// re-fetch metadata from audio_cache m4a filenames and register them.
    /// Since yt-dlp takes time per track, restoration runs in the background
    /// and broadcasts tracks_update on completion so clients refresh.
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
            // Clear the flag on drop so a panic or early return cannot leave it
            // latched, which would disable restore for the rest of the process.
            let _guard = RestoreGuard(&state);
            tracing::info!(
                "Tracks key missing: restoring {} track(s) from audio_cache",
                cached.len()
            );
            for (video_id, path) in cached {
                let track = state.refetch_track_metadata(&video_id, &path).await;
                let json_str = match track.to_redis_json() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Failed to serialize restored track {video_id}: {e}");
                        continue;
                    }
                };
                let mut conn = state.redis.clone();
                if let Err(e) = conn
                    .hset::<_, _, _, ()>(REDIS_KEY_TRACKS, &video_id, json_str)
                    .await
                {
                    tracing::warn!("Redis error restoring track {video_id}: {e}");
                }
            }
            state.broadcast_tracks().await;
            tracing::info!("Track restore finished");
        });
    }

    /// Re-fetch metadata only via yt-dlp. If the video is deleted or unavailable,
    /// the file can still be played, so return minimal info with the ID as title.
    async fn refetch_track_metadata(&self, video_id: &str, path: &Path) -> AudioTrack {
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let meta = match fetch_metadata(&url, None).await {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!("Metadata refetch failed for {video_id}: {e}");
                Value::Null
            }
        };

        // Use the original file's mtime as created_at to preserve registration order
        AudioTrack::from_meta(
            video_id,
            &meta,
            file_mtime_f64(path),
            path.to_string_lossy().to_string(),
        )
    }

    /// Return tracks for the given scope. If a playlist is specified, return its
    /// track order; otherwise return the full library order.
    pub async fn scoped_tracks(&self, playlist_id: Option<&str>) -> Vec<AudioTrack> {
        match playlist_id {
            Some(pid) => self.list_playlist_tracks(pid).await,
            None => self.list_tracks().await,
        }
    }

    /// Return tracks in the active scope (active playlist, or entire library if none).
    async fn active_scope_tracks(&self) -> Vec<AudioTrack> {
        let scope = self.active_playlist().await;
        self.scoped_tracks(scope.as_deref()).await
    }

    /// Return the next track to play after the current one ends, based on the
    /// playback mode ("off" returns None). Selection uses the active playlist scope.
    pub async fn auto_next_track(&self, current_id: &str) -> Option<AudioTrack> {
        match self.playback_mode().await.as_str() {
            "loop" => neighbor_track(&self.active_scope_tracks().await, current_id, 1),
            "shuffle" => random_track_from(self.active_scope_tracks().await, current_id),
            _ => None, // "off": no auto-play
        }
    }

    /// Return the track to play on an explicit "next" command. Shuffle picks
    /// randomly; otherwise advance in order (even when mode is "off").
    pub async fn skip_next_track(&self, current_id: &str) -> Option<AudioTrack> {
        if self.playback_mode().await == "shuffle" {
            random_track_from(self.active_scope_tracks().await, current_id)
        } else {
            neighbor_track(&self.active_scope_tracks().await, current_id, 1)
        }
    }

    /// Return the track to play on an explicit "previous" command (wraps from first to last).
    pub async fn skip_prev_track(&self, current_id: &str) -> Option<AudioTrack> {
        neighbor_track(&self.active_scope_tracks().await, current_id, -1)
    }

    /// Return tracks for the given page (1-based) and total count.
    /// If playlist_id is given, return in that playlist's track order.
    /// If filter is given, only include tracks whose title or channel
    /// contains the substring (case-insensitive).
    pub async fn list_tracks_page(
        &self,
        playlist_id: Option<&str>,
        page: usize,
        per_page: usize,
        filter: Option<&str>,
    ) -> (Vec<AudioTrack>, usize) {
        let tracks = self.scoped_tracks(playlist_id).await;
        let tracks = match filter {
            Some(q) => {
                let q = q.to_lowercase();
                tracks
                    .into_iter()
                    .filter(|t| {
                        t.title.to_lowercase().contains(&q) || t.channel.to_lowercase().contains(&q)
                    })
                    .collect()
            }
            None => tracks,
        };
        let total = tracks.len();
        let start = page.saturating_sub(1).saturating_mul(per_page);
        let items = tracks.into_iter().skip(start).take(per_page).collect();
        (items, total)
    }

    /// Fetch tracks referenced by queue entries in a single HMGET.
    pub(crate) async fn fetch_tracks_for(
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
}

/// Releases `AppState::restoring` when the restore task ends, however it ends.
struct RestoreGuard<'a>(&'a AppState);

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        self.0.restoring.store(false, Ordering::SeqCst);
    }
}

/// Return the next (dir=1) or previous (dir=-1) track relative to current_id.
/// Wraps around at the ends. If current_id is not found (deleted etc.),
/// returns the first (dir=1) or last (dir=-1) track.
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

/// Pick a random track excluding current_id (returns that track if only one exists).
fn random_track_from(mut tracks: Vec<AudioTrack>, current_id: &str) -> Option<AudioTrack> {
    if tracks.len() > 1 {
        tracks.retain(|t| t.id != current_id);
    }
    if tracks.is_empty() {
        return None;
    }
    // Nanoseconds of the current time are random enough for track selection variety
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    Some(tracks.swap_remove(nanos % tracks.len()))
}

/// List {video_id}.m4a files in audio_cache as (video_id, path) pairs.
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

/// Return a file's mtime as UNIX seconds (falls back to current time).
fn file_mtime_f64(path: &Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or_else(now_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal track for tests.
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

        // Forward: advance to next, wrap from last to first
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa1", 1).unwrap().id,
            "aaaaaaaaaa2"
        );
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa3", 1).unwrap().id,
            "aaaaaaaaaa1"
        );
        // Backward: go to previous, wrap from first to last
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa2", -1).unwrap().id,
            "aaaaaaaaaa1"
        );
        assert_eq!(
            neighbor_track(&tracks, "aaaaaaaaaa1", -1).unwrap().id,
            "aaaaaaaaaa3"
        );
        // Not found (deleted etc.): first / last
        assert_eq!(
            neighbor_track(&tracks, "gone", 1).unwrap().id,
            "aaaaaaaaaa1"
        );
        assert_eq!(
            neighbor_track(&tracks, "gone", -1).unwrap().id,
            "aaaaaaaaaa3"
        );
        // Empty returns None
        assert!(neighbor_track(&[], "aaaaaaaaaa1", 1).is_none());
    }

    #[test]
    fn random_track_excludes_current_unless_only_one() {
        // With 2 tracks, the other one is always picked
        let tracks = vec![track("aaaaaaaaaa1"), track("aaaaaaaaaa2")];
        for _ in 0..5 {
            let picked = random_track_from(tracks.clone(), "aaaaaaaaaa1").unwrap();
            assert_eq!(picked.id, "aaaaaaaaaa2");
        }
        // With only 1 track, that track is returned
        let only = vec![track("aaaaaaaaaa1")];
        assert_eq!(
            random_track_from(only, "aaaaaaaaaa1").unwrap().id,
            "aaaaaaaaaa1"
        );
        assert!(random_track_from(Vec::new(), "aaaaaaaaaa1").is_none());
    }
}
