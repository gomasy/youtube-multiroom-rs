//! Named playlists, the active playback scope, and importing a YouTube
//! playlist into a local one.

use super::job::{VideoJob, Visited};
use super::model::{AudioTrack, Playlist, PlaylistImportInfo, PlaylistJson};
use super::url::{is_video_id, watch_url};
use super::warn_redis;
use super::ytdlp::{DownloadError, run_yt_dlp_cancellable};
use super::{
    AppState, REDIS_KEY_ACTIVE_PLAYLIST, REDIS_KEY_PLAYLISTS, now_f64, playlist_key, since_epoch,
};
use redis::AsyncCommands;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use tokio::time;
use tokio_util::sync::CancellationToken;

const PLAYLIST_NAME_MAX_CHARS: usize = 100;
/// Cap for playlist import to avoid expanding effectively-infinite mix lists.
const PLAYLIST_IMPORT_MAX: usize = 100;
const PLAYLIST_FLAT_TIMEOUT_SECS: u64 = 60;

/// Append a track only while the playlist is still registered. The list key is
/// created implicitly by RPUSH, so an unguarded append would resurrect a
/// playlist's track list after the playlist itself was deleted.
/// KEYS: playlists hash, playlist track list. ARGV: playlist id, track id.
/// Returns 1 when appended, 0 when the playlist is gone.
static ADD_PLAYLIST_TRACK_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then
            return 0
        end
        redis.call('LREM', KEYS[2], 0, ARGV[2])
        redis.call('RPUSH', KEYS[2], ARGV[2])
        return 1
        ",
    )
});

impl AppState {
    /// Create a playlist. Name is trimmed and must be 1–PLAYLIST_NAME_MAX_CHARS
    /// chars. Returns None for invalid names or Redis errors.
    pub async fn create_playlist(&self, name: &str) -> Option<Playlist> {
        let name = valid_playlist_name(name)?;
        let playlist = Playlist {
            id: new_playlist_id(),
            name: name.to_string(),
            created_at: now_f64(),
        };
        let json_str = serialize_playlist(&playlist)?;
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

    /// Rename a playlist. Returns false if the name is invalid, the playlist
    /// doesn't exist, or Redis errors occur.
    pub async fn rename_playlist(&self, playlist_id: &str, name: &str) -> bool {
        let Some(name) = valid_playlist_name(name) else {
            return false;
        };
        let Some(mut playlist) = self.get_playlist(playlist_id).await else {
            return false;
        };
        playlist.name = name.to_string();
        let Some(json_str) = serialize_playlist(&playlist) else {
            return false;
        };
        let mut conn = self.redis.clone();
        match conn
            .hset::<_, _, _, ()>(REDIS_KEY_PLAYLISTS, playlist_id, json_str)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error renaming playlist {playlist_id}: {e}");
                false
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

    /// Whether a playlist is registered, without paying to deserialize it.
    /// Mirrors device_exists; a Redis error reports "gone" so callers 404 rather
    /// than acting on a playlist they could not confirm.
    pub async fn playlist_exists(&self, playlist_id: &str) -> bool {
        let mut conn = self.redis.clone();
        conn.hexists(REDIS_KEY_PLAYLISTS, playlist_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error checking playlist {playlist_id}: {e}");
                false
            })
    }

    /// Return all playlists sorted by creation time.
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

    /// Delete a playlist. Also removes its track list and clears the active playlist
    /// setting if it pointed here (tracks themselves are not deleted). Returns false if not found.
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
        warn_redis!(
            "deleting playlist tracks for {playlist_id}",
            conn.del(playlist_key(playlist_id)).await
        );
        // If this was the active playlist, revert to the full library
        if self.raw_active_playlist().await.as_deref() == Some(playlist_id) {
            warn_redis!(
                "clearing active playlist",
                conn.del(REDIS_KEY_ACTIVE_PLAYLIST).await
            );
        }
        true
    }

    /// Append a track to the end of an existing playlist (moves to end if
    /// already present). Returns false if the playlist no longer exists: the
    /// existence check and the list mutation are atomic, so a slow background
    /// import cannot recreate a playlist that was deleted meanwhile.
    pub async fn add_playlist_track(
        &self,
        playlist_id: &str,
        track_id: &str,
    ) -> redis::RedisResult<bool> {
        // Serialized with reorder's read-then-replace to prevent interleaving.
        let _guard = self.order_lock.lock().await;
        let mut conn = self.redis.clone();
        ADD_PLAYLIST_TRACK_SCRIPT
            .key(REDIS_KEY_PLAYLISTS)
            .key(playlist_key(playlist_id))
            .arg(playlist_id)
            .arg(track_id)
            .invoke_async::<i64>(&mut conn)
            .await
            .map(|added| added == 1)
            .inspect_err(|e| {
                tracing::warn!("Redis error adding track to playlist {playlist_id}: {e}");
            })
    }

    /// Remove a track from a playlist. Returns false if not found.
    pub async fn remove_playlist_track(&self, playlist_id: &str, track_id: &str) -> bool {
        // Serialize with reorder's read-then-replace to prevent the removed
        // track from being restored by a concurrent write-back.
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

    /// Remove multiple tracks from one playlist without touching the library,
    /// cache files, queues, or any other playlist.
    pub async fn remove_playlist_tracks(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> redis::RedisResult<u64> {
        let unique_ids: HashSet<&str> = track_ids.iter().map(String::as_str).collect();
        if unique_ids.is_empty() {
            return Ok(0);
        }

        // Serialize with reorder's read-then-replace so a concurrent write-back
        // cannot restore an entry removed by this operation.
        let _guard = self.order_lock.lock().await;
        let key = playlist_key(playlist_id);
        let mut pipe = redis::pipe();
        pipe.atomic();
        for track_id in unique_ids {
            pipe.lrem(&key, 0, track_id);
        }
        let mut conn = self.redis.clone();
        let removed: Vec<u64> = pipe.query_async(&mut conn).await?;
        Ok(removed.into_iter().sum())
    }

    pub(crate) async fn playlist_track_ids(&self, playlist_id: &str) -> Vec<String> {
        let mut conn = self.redis.clone();
        conn.lrange(playlist_key(playlist_id), 0, -1)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error reading playlist {playlist_id}: {e}");
                Vec::new()
            })
    }

    /// Return playlist tracks in playlist order (deleted tracks are skipped).
    pub async fn list_playlist_tracks(&self, playlist_id: &str) -> Vec<AudioTrack> {
        let ids = self.playlist_track_ids(playlist_id).await;
        let mut by_id = self.fetch_tracks_for(ids.iter()).await;
        ids.iter().filter_map(|id| by_id.remove(id)).collect()
    }

    /// Return all playlists with metadata and track counts in creation order (API/WS wire format).
    pub async fn playlists_json(&self) -> Value {
        let playlists = self.playlists().await;
        if playlists.is_empty() {
            return json!([]);
        }

        // Fetch track counts in a single pipeline round-trip
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

    /// Return the saved active playlist ID (no existence check).
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

    /// Active playlist ID for loop/shuffle scope. Returns None (= full library)
    /// if unset or if the referenced playlist has been deleted.
    pub async fn active_playlist(&self) -> Option<String> {
        let id = self.raw_active_playlist().await?;
        self.playlist_exists(&id).await.then_some(id)
    }

    /// Set the active playlist (None reverts to full library).
    /// Returns false for nonexistent playlists or Redis errors.
    pub async fn set_active_playlist(&self, playlist_id: Option<&str>) -> bool {
        let mut conn = self.redis.clone();
        let result = match playlist_id {
            Some(pid) => {
                if !self.playlist_exists(pid).await {
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

    /// Flat-expand a YouTube playlist and start importing into a local playlist
    /// with the same name (created if absent) in the background. Returns the
    /// playlist name and item count at start. Per-video download progress is
    /// broadcast via the same downloads_update as extract_audio.
    pub async fn import_playlist(
        self: &Arc<Self>,
        list_id: &str,
        cancel: &CancellationToken,
    ) -> Result<PlaylistImportInfo, DownloadError> {
        let url = format!("https://www.youtube.com/playlist?list={list_id}");
        let items = format!("1:{PLAYLIST_IMPORT_MAX}");
        let stdout = run_yt_dlp_cancellable(
            &[
                "--dump-single-json",
                "--flat-playlist",
                "--playlist-items",
                &items,
                &url,
            ],
            time::Duration::from_secs(PLAYLIST_FLAT_TIMEOUT_SECS),
            Some(cancel),
        )
        .await
        .map_err(|e| e.context("Failed to expand playlist"))?;
        let meta: Value = serde_json::from_slice(&stdout)
            .map_err(|e| format!("Failed to parse playlist metadata: {e}"))?;

        // Exclude non-video entries and duplicates (same video appearing multiple times)
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
            return Err("Playlist has no importable videos".into());
        }
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        // Truncate playlist name to the creation limit (fall back to list ID if title is unknown)
        let name: String = meta["title"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(list_id)
            .chars()
            .take(PLAYLIST_NAME_MAX_CHARS)
            .collect();

        // Append to an existing playlist with the same name (avoid duplicates on re-import)
        let playlists = self.playlists().await;
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let playlist = match playlists.into_iter().find(|p| p.name == name) {
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

        // Spawning the worker is the import commit point. The potentially slow
        // Redis work above stays outside this lock so Stop all is never blocked
        // from terminating unrelated active processes.
        let _commit_guard = self.download_cancel.lock().await;
        if cancel.is_cancelled() {
            // A newly created playlist may already be visible to another client;
            // leave it empty rather than risk deleting concurrent user changes.
            return Err(DownloadError::Cancelled);
        }
        let total = video_ids.len();
        let state = self.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let job = VideoJob::new(
                format!("Playlist import '{}'", playlist.name),
                "imported",
                "deleted while downloading",
            );
            let (state, cancel, playlist) = (&state, &cancel, &playlist);
            job.run(&video_ids, cancel, |video_id| async move {
                let track = state.extract_audio(&watch_url(&video_id), cancel).await?;
                match state.add_playlist_track(&playlist.id, &track.id).await {
                    Ok(true) => {
                        // Reflect each successfully imported track in the lists
                        state.broadcast_tracks();
                        state.broadcast_playlists().await;
                        Ok(Visited::Done)
                    }
                    // The playlist this import exists to fill is gone, so
                    // there is nowhere left to put the remaining videos.
                    Ok(false) => Ok(Visited::Stop("the playlist was deleted".to_string())),
                    Err(e) => {
                        tracing::warn!(
                            "Playlist import '{}': failed to add {video_id}: {e}",
                            playlist.name
                        );
                        Ok(Visited::Skipped)
                    }
                }
            })
            .await;
        });

        Ok(PlaylistImportInfo { name, total })
    }
}

/// Trim a user-supplied playlist name and accept it only if it is non-empty and
/// within the length cap. Shared by create and rename so the two cannot drift.
fn valid_playlist_name(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= PLAYLIST_NAME_MAX_CHARS).then_some(name)
}

/// Serialize a playlist for Redis, reporting rather than panicking on failure.
fn serialize_playlist(playlist: &Playlist) -> Option<String> {
    serde_json::to_string(playlist)
        .inspect_err(|e| tracing::warn!("Failed to serialize playlist {}: {e}", playlist.id))
        .ok()
}

/// Generate a playlist ID ("pl" + time-derived value; unique enough for creation frequency).
fn new_playlist_id() -> String {
    let d = since_epoch();
    format!("pl{:x}{:05x}", d.as_millis(), d.subsec_nanos() & 0xfffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_ids_are_distinct() {
        // No duplicates even when generated consecutively (nanosecond precision)
        let a = new_playlist_id();
        let b = new_playlist_id();
        assert_ne!(a, b);
        assert!(a.starts_with("pl"));
    }

    #[test]
    fn playlist_names_are_trimmed_and_length_capped() {
        assert_eq!(valid_playlist_name("  Mix  "), Some("Mix"));
        assert_eq!(valid_playlist_name(""), None);
        assert_eq!(valid_playlist_name("   "), None);

        // The cap counts characters, not bytes, so a name of multi-byte
        // characters is accepted right up to the same limit as an ASCII one
        let kana = "あ".repeat(PLAYLIST_NAME_MAX_CHARS);
        assert_eq!(valid_playlist_name(&kana).map(str::len), Some(kana.len()));
        assert_eq!(
            valid_playlist_name(&"あ".repeat(PLAYLIST_NAME_MAX_CHARS + 1)),
            None
        );
        assert_eq!(
            valid_playlist_name(&"a".repeat(PLAYLIST_NAME_MAX_CHARS + 1)),
            None
        );
    }
}
