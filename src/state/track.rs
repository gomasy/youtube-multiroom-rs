//! The audio library: registering tracks, keeping their order, and choosing
//! what plays next.

use super::download::ExtractSlot;
use super::matching::{EXACT, best_scored, fold, match_score};
use super::model::{AudioTrack, ReorderOutcome};
use super::url::{is_video_id, watch_url};
use super::ytdlp::fetch_metadata;
use super::{
    AUDIO_EXT, AppState, PendingCommand, REDIS_KEY_TRACKS, REDIS_KEY_TRACKS_ORDER,
    REDIS_PENDING_PREFIX, now_f64, playlist_key, queue_key, since_epoch, token_track_id,
};
use super::{redis_or, warn_redis};
use redis::AsyncCommands;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

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
        let (track, slot) = self.unregister_track(id).await?;
        self.forget_references(&HashSet::from([id])).await;
        self.release_extract_slot(id, &slot).await;
        Some(track)
    }

    /// Delete every registered track named, and return how many that was.
    ///
    /// The per-video work — claiming the extract slot, removing the cache file,
    /// dropping the registration — is still one track at a time. What follows
    /// is not: each of the sweeps that clears the references to a deleted track
    /// (the library order, every playlist, the queued play commands, the device
    /// queues) costs the same whether it clears one id or a hundred, so they
    /// run once for the whole set. Deleting a page of tracks one at a time
    /// meant a SCAN of the pending commands, a read of every playlist and a
    /// read of every device queue *per track*.
    ///
    /// Ids naming nothing are skipped, and a repeat is deleted once.
    pub async fn remove_tracks(&self, ids: &[String]) -> usize {
        // A repeat finds nothing registered the second time round, so it claims
        // no second slot and cannot appear here twice.
        let mut claimed: Vec<(&str, Arc<ExtractSlot>)> = Vec::new();
        for id in ids {
            if let Some((_, slot)) = self.unregister_track(id).await {
                claimed.push((id.as_str(), slot));
            }
        }
        if claimed.is_empty() {
            return 0;
        }

        let removed: HashSet<&str> = claimed.iter().map(|(id, _)| *id).collect();
        self.forget_references(&removed).await;
        for (id, slot) in &claimed {
            self.release_extract_slot(id, slot).await;
        }
        removed.len()
    }

    /// Delete one track's cache file and its registration, leaving what still
    /// refers to it for [`Self::forget_references`]. None when nothing was
    /// registered under this id — a download still in flight for the video
    /// registers a fresh track rather than resurrecting this one.
    ///
    /// The video's extract slot comes back with the track, still held: it is
    /// what stops a re-download of the same video from registering underneath
    /// the sweep that has yet to run, which would strip the fresh track of the
    /// order position and playlist memberships it just earned. The caller
    /// releases it once that sweep is done.
    async fn unregister_track(&self, id: &str) -> Option<(AudioTrack, Arc<ExtractSlot>)> {
        self.get_track(id).await?;

        // Claim the video's extract slot for the deletion. A download that
        // would rewrite this entry when it commits is told to discard its own
        // registration instead, so the deletion never waits it out.
        let slot = self.extract_slot(id).await;
        slot.mark_deleted();
        match self.unregister_track_locked(id).await {
            Some(track) => Some((track, slot)),
            None => {
                self.release_extract_slot(id, &slot).await;
                None
            }
        }
    }

    /// The half of a deletion that runs under the video's extract slot.
    async fn unregister_track_locked(&self, id: &str) -> Option<AudioTrack> {
        let track = self.get_track(id).await?;

        // File first: removing the last track makes the tracks key vanish, and
        // restore_tracks_if_missing would then rebuild this track from a
        // surviving file.
        if !track.file_path.is_empty() {
            let _ = tokio::fs::remove_file(&track.file_path).await;
        }

        let mut conn = self.redis.clone();
        warn_redis!("deleting track {id}", conn.hdel(REDIS_KEY_TRACKS, id).await);
        Some(track)
    }

    /// Drop every reference to a set of deleted tracks, so nothing left behind
    /// can play one or put it back.
    ///
    /// Each sweep is safe to run late: a listing skips ids no track answers to,
    /// and `peek_queue` clears a queue entry whose track is gone.
    async fn forget_references(&self, ids: &HashSet<&str>) {
        self.unlink_from_order_and_playlists(ids).await;
        self.clear_pending_referencing(ids).await;
        self.detach_from_devices(ids).await;
    }

    /// Drop deleted tracks from the library order and from every playlist that
    /// listed them, in a single pipeline round-trip.
    async fn unlink_from_order_and_playlists(&self, ids: &HashSet<&str>) {
        // Serialize with reorder's read-then-replace to prevent a deletion
        // from being silently undone by a concurrent reorder write-back.
        let _guard = self.order_lock.lock().await;
        let playlists = self.playlists().await;

        let mut pipe = redis::pipe();
        for id in ids {
            pipe.lrem(REDIS_KEY_TRACKS_ORDER, 0, *id).ignore();
            for playlist in &playlists {
                pipe.lrem(playlist_key(&playlist.id), 0, *id).ignore();
            }
        }
        let mut conn = self.redis.clone();
        if let Err(e) = pipe.query_async::<()>(&mut conn).await {
            // The whole pipeline failed or none of it did, so the count is what
            // there is to report — naming one of the ids would suggest the rest
            // succeeded.
            tracing::warn!("Redis error unlinking {} deleted track(s): {e}", ids.len());
        }
    }

    /// Discard queued play commands that would resurrect a deleted track.
    async fn clear_pending_referencing(&self, ids: &HashSet<&str>) {
        let keys = self.pending_keys().await;
        if keys.is_empty() {
            return;
        }

        let mut conn = self.redis.clone();
        // An empty read deletes nothing: a command that could not be examined
        // is no basis for clearing it.
        let commands: Vec<Option<String>> = redis_or!(
            "reading pending commands",
            conn.mget(&keys).await,
            Vec::new()
        );

        let mut pipe = redis::pipe();
        let mut cleared = 0;
        for (key, json_str) in keys.iter().zip(commands) {
            if json_str
                .and_then(|s| serde_json::from_str::<PendingCommand>(&s).ok())
                .is_some_and(|cmd| ids.contains(cmd.track.id.as_str()))
            {
                pipe.del(key).ignore();
                cleared += 1;
            }
        }
        if cleared > 0
            && let Err(e) = pipe.query_async::<()>(&mut conn).await
        {
            tracing::warn!("Redis error clearing {cleared} pending command(s): {e}");
        }
    }

    /// Every pending-command key currently in Redis. A partial scan is returned
    /// as-is: callers only delete what they find, so a short result leaves
    /// stale entries behind rather than removing the wrong ones.
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

    /// Remove deleted tracks from every device's Up Next queue, and clear them
    /// from any device that was pointing at one.
    async fn detach_from_devices(&self, ids: &HashSet<&str>) {
        let devices = self.all_devices().await;
        if devices.is_empty() {
            return;
        }
        let queues = self.queues_for(devices.keys()).await;

        // Entries are unique, so removing by value cannot hit the wrong one
        // when a device consumes an entry concurrently.
        let mut pipe = redis::pipe();
        let mut stale = 0;
        for (device_id, entries) in &queues {
            for entry in entries.iter().filter(|e| ids.contains(token_track_id(e))) {
                pipe.lrem(queue_key(device_id), 0, entry).ignore();
                stale += 1;
            }
        }
        if stale > 0 {
            let mut conn = self.redis.clone();
            if let Err(e) = pipe.query_async::<()>(&mut conn).await {
                tracing::warn!("Redis error removing {stale} stale queue entry/entries: {e}");
            }
        }

        for mut dev in devices.into_values() {
            if dev
                .current_track
                .as_ref()
                .is_some_and(|t| ids.contains(t.id.as_str()))
            {
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
        let all: HashMap<String, String> = redis_or!(
            "reading tracks",
            conn.hgetall(REDIS_KEY_TRACKS).await,
            HashMap::new()
        );
        let mut by_id: HashMap<String, AudioTrack> = all
            .values()
            .filter_map(|s| AudioTrack::from_redis_json(s))
            .map(|t| (t.id.clone(), t))
            .collect();

        let order: Vec<String> = redis_or!(
            "reading track order",
            conn.lrange(REDIS_KEY_TRACKS_ORDER, 0, -1).await,
            Vec::new()
        );
        let mut tracks: Vec<AudioTrack> = order.iter().filter_map(|id| by_id.remove(id)).collect();

        let mut rest: Vec<AudioTrack> = by_id.into_values().collect();
        // total_cmp rather than partial_cmp: a total order over every f64
        // leaves no incomparable case to decide about, and playlists() sorts
        // the same field the same way.
        rest.sort_by(|a, b| {
            b.created_at
                .total_cmp(&a.created_at)
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

        if self.write_order_list(&key, &ids).await {
            ReorderOutcome::Moved
        } else {
            ReorderOutcome::Failed
        }
    }

    /// Replace an order list — the library's or a playlist's — wholesale. At
    /// most a few hundred ids, so a full replace is cheaper to reason about
    /// than a diff, and it is atomic so no reader sees the list half-written.
    ///
    /// Callers hold `order_lock`: the read they based `ids` on and this write
    /// have to be one step, or a concurrent edit lands between them and is then
    /// overwritten. Returns whether the list was written.
    async fn write_order_list(&self, key: &str, ids: &[String]) -> bool {
        // RPUSH with no values is an error, so an empty list is deleted rather
        // than written. DEL alone also leaves no stale entries behind.
        let mut pipe = redis::pipe();
        pipe.atomic().del(key);
        if !ids.is_empty() {
            pipe.rpush(key, ids);
        }
        let mut conn = self.redis.clone();
        match pipe.query_async::<()>(&mut conn).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error writing the order list {key}: {e}");
                false
            }
        }
    }

    /// Put `video_id` back where `snapshot` had it in the order list.
    ///
    /// A download prepends, which is right for a track being added and wrong
    /// for one being recovered: a repair or an import restores files the
    /// library already has a place for, and moving them all to the top would
    /// scramble an order nobody asked to change.
    ///
    /// The position is taken from the neighbours rather than from an index, so
    /// what the rest of the list did in the meantime is preserved: the track
    /// lands just after the last id that precedes it in the snapshot and is
    /// still there.
    pub(crate) async fn restore_order_position(&self, video_id: &str, snapshot: &[String]) {
        let _guard = self.order_lock.lock().await;
        let Ok(mut current) = self.try_track_order().await else {
            return;
        };
        current.retain(|id| id != video_id);
        current.insert(
            anchored_index(&current, snapshot, video_id),
            video_id.to_string(),
        );
        self.write_order_list(REDIS_KEY_TRACKS_ORDER, &current)
            .await;
    }

    /// Replace the library order with `edit`'s verdict on what it currently
    /// holds. The read and the write are one step under `order_lock`, and a
    /// read that fails aborts rather than writing back a list that every id
    /// Redis failed to report is missing from.
    pub(crate) async fn rewrite_track_order(
        &self,
        edit: impl FnOnce(Vec<String>) -> Vec<String>,
    ) -> Option<usize> {
        let _guard = self.order_lock.lock().await;
        let current = self.try_track_order().await.ok()?;
        let ids = edit(current);
        self.write_order_list(REDIS_KEY_TRACKS_ORDER, &ids)
            .await
            .then_some(ids.len())
    }

    /// The raw order list, including ids no registered track answers to. Those
    /// are what an import leaves as placeholders for videos still to arrive, so
    /// a recovery job has to read the list as stored rather than as listed.
    ///
    /// A read that failed reads as empty here, which is what the one caller
    /// wants: an absent snapshot leaves recovered tracks where the download put
    /// them rather than moving them somewhere guessed at. Callers that write
    /// the list back must use `try_track_order` instead.
    pub(crate) async fn track_order(&self) -> Vec<String> {
        self.try_track_order().await.unwrap_or_default()
    }

    async fn try_track_order(&self) -> redis::RedisResult<Vec<String>> {
        let mut conn = self.redis.clone();
        conn.lrange(REDIS_KEY_TRACKS_ORDER, 0, -1)
            .await
            .inspect_err(|e| tracing::warn!("Redis error reading track order: {e}"))
    }

    /// The library track a spoken phrase names, or None if none is close
    /// enough. Searched over the whole library rather than the playback scope:
    /// asking for something by name is an escape from whatever scope is set.
    pub async fn find_track(&self, query: &str) -> Option<AudioTrack> {
        let folded = fold(query);
        best_scored(self.list_tracks().await, |track| {
            track_score(track, &folded)
        })
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
            // Cleared on drop, so a panic or early return cannot latch the flag
            // and disable restore for the rest of the process.
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
            state.broadcast_tracks();
            tracing::info!("Track restore finished");
        });
    }

    /// Re-fetch metadata only via yt-dlp. If the video is deleted or unavailable,
    /// the file can still be played, so return minimal info with the ID as title.
    async fn refetch_track_metadata(&self, video_id: &str, path: &Path) -> AudioTrack {
        let meta = match fetch_metadata(&watch_url(video_id), None).await {
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

    /// Resolve a batch of track references in a single HMGET, keyed by track ID.
    ///
    /// A reference is either a queue entry ("{id}#{millis}") or a bare track ID;
    /// [`token_track_id`] reduces both to the same thing, so a list of any
    /// length costs one round-trip rather than one per ID. References naming no
    /// track are absent from the result, which makes it a membership test too.
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
    let nanos = since_epoch().subsec_nanos() as usize;
    Some(tracks.swap_remove(nanos % tracks.len()))
}

/// Where `video_id` belongs in `current` (which no longer holds it) to keep the
/// order `snapshot` gives it: just after the last id preceding it there that is
/// still present. Nothing preceding it survived, so the front.
fn anchored_index(current: &[String], snapshot: &[String], video_id: &str) -> usize {
    snapshot
        .iter()
        .take_while(|id| *id != video_id)
        .filter_map(|id| current.iter().position(|c| c == id))
        .max()
        .map_or(0, |i| i + 1)
}

/// How well a track answers a spoken query, already folded by the caller. The
/// title is scored a whole tier above the channel, so a weak title match still
/// outranks a perfect channel one rather than being decided by which happens to
/// score higher — asking for a song by name is far more common than asking for
/// everything by an uploader.
fn track_score(track: &AudioTrack, folded_query: &str) -> Option<u32> {
    let by_title = match_score(&track.title, folded_query).map(|score| score + TITLE_TIER);
    // A title that answers the query outright leaves the channel nothing to
    // add, and reading it costs another fold of the field.
    if by_title == Some(EXACT + TITLE_TIER) {
        return by_title;
    }
    by_title.max(match_score(&track.channel, folded_query))
}

/// Added to every title score, putting it out of reach of any channel score.
const TITLE_TIER: u32 = 1000;

/// List {video_id}.m4a files in audio_cache as (video_id, path) pairs.
pub(super) fn cached_video_ids(cache_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
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

/// A file's mtime as UNIX seconds, or None if the filesystem cannot say.
/// Takes the metadata rather than the path so a caller that has already read it
/// — for the size, say — does not pay for a second stat.
pub(super) fn mtime_f64(meta: &std::fs::Metadata) -> Option<f64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

/// Return a file's mtime as UNIX seconds (falls back to current time).
fn file_mtime_f64(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| mtime_f64(&meta))
        .unwrap_or_else(now_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

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

    /// Track with a title and channel of its own, for query matching.
    fn named(id: &str, title: &str, channel: &str) -> AudioTrack {
        AudioTrack {
            title: title.into(),
            channel: channel.into(),
            ..track(id)
        }
    }

    /// The track a spoken query resolves to, as `find_track` picks it.
    fn best_match<'a>(tracks: &'a [AudioTrack], query: &str) -> Option<&'a AudioTrack> {
        let folded = fold(query);
        best_scored(tracks, |track| track_score(track, &folded))
    }

    #[test]
    fn a_query_picks_the_best_titled_track() {
        let tracks = vec![
            named("aaaaaaaaaa1", "Live at the Park", "Never Gonna Records"),
            named("aaaaaaaaaa2", "Never Gonna Give You Up", "Rick Astley"),
            named("aaaaaaaaaa3", "Never Gonna Give You Up (Remix)", "Someone"),
        ];

        // A whole title beats a title it is only the start of
        assert_eq!(
            best_match(&tracks, "never gonna give you up").unwrap().id,
            "aaaaaaaaaa2"
        );
        // Channel names are searched too, but never at a title's expense: this
        // phrase is the channel of track 1 and inside the title of tracks 2–3
        assert_eq!(
            best_match(&tracks, "never gonna").unwrap().id,
            "aaaaaaaaaa2"
        );
        assert_eq!(
            best_match(&tracks, "rick astley").unwrap().id,
            "aaaaaaaaaa2"
        );
        // Nothing close enough plays nothing at all
        assert!(best_match(&tracks, "bohemian rhapsody").is_none());
        assert!(best_match(&[], "anything").is_none());
    }

    #[test]
    fn equally_good_matches_resolve_to_library_order() {
        let tracks = vec![
            named("aaaaaaaaaa1", "Mix", "A"),
            named("aaaaaaaaaa2", "Mix", "B"),
        ];
        assert_eq!(best_match(&tracks, "mix").unwrap().id, "aaaaaaaaaa1");
    }

    #[test]
    fn a_restored_track_lands_after_its_surviving_predecessor() {
        let snapshot = ids(&["a", "b", "c", "d"]);
        // A download moved "c" to the front; it belongs after "b"
        assert_eq!(anchored_index(&ids(&["a", "b", "d"]), &snapshot, "c"), 2);
        // Its predecessors are gone, so the front is where it belongs
        assert_eq!(anchored_index(&ids(&["d"]), &snapshot, "a"), 0);
        assert_eq!(anchored_index(&ids(&["d"]), &snapshot, "c"), 0);
        // Tracks added since are left where they are, ahead or behind
        assert_eq!(anchored_index(&ids(&["new", "a", "b"]), &snapshot, "c"), 3);
        // An id the snapshot has no opinion about goes to the end, the only
        // place that displaces nothing
        assert_eq!(anchored_index(&ids(&["a", "b"]), &snapshot, "zz"), 2);
    }
}
