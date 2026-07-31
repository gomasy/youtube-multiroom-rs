//! Per-device state: what each Echo is playing, the command waiting for it to
//! connect, and its Up Next queue.

use super::model::{AudioTrack, DeviceJson, DeviceState, DeviceUpdate, PendingCommand, QueueItem};
use super::warn_redis;
use super::{
    AppState, REDIS_KEY_DEVICES, new_token, now_f64, pending_key, queue_key, token_track_id,
};
use redis::AsyncCommands;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Pending command TTL in seconds (expired by Redis key TTL).
const PENDING_TTL_SECS: u64 = 600;

/// Store a pending command only while the device is still registered.
/// KEYS: devices hash, pending key. ARGV: device id, command JSON, TTL.
/// Returns 1 when stored, 0 when the device is unknown.
static QUEUE_PLAY_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then
            return 0
        end
        redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
        return 1
        ",
    )
});

/// Append to an Up Next queue only while the device is still registered. RPUSH
/// creates the list key implicitly, so an unguarded append would leave an
/// orphaned queue behind for a device that no longer exists — the same reason
/// QUEUE_PLAY_SCRIPT guards the pending key.
/// KEYS: devices hash, queue key. ARGV: device id, queue entry.
/// Returns 1 when appended, 0 when the device is unknown.
static PUSH_QUEUE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then
            return 0
        end
        redis.call('RPUSH', KEYS[2], ARGV[2])
        return 1
        ",
    )
});

impl AppState {
    pub(crate) async fn write_device(&self, dev: &DeviceState) {
        let json_str = match serde_json::to_string(dev) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to serialize device {}: {e}", dev.device_id);
                return;
            }
        };
        let mut conn = self.redis.clone();
        if let Err(e) = conn
            .hset::<_, _, _, ()>(REDIS_KEY_DEVICES, &dev.device_id, json_str)
            .await
        {
            tracing::warn!("Redis error writing device {}: {e}", dev.device_id);
        }
    }

    pub async fn get_device(&self, device_id: &str) -> Option<DeviceState> {
        let mut conn = self.redis.clone();
        match conn
            .hget::<_, _, Option<String>>(REDIS_KEY_DEVICES, device_id)
            .await
        {
            Ok(Some(s)) => serde_json::from_str(&s).ok(),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("Redis error reading device {device_id}: {e}");
                None
            }
        }
    }

    pub(crate) async fn all_devices(&self) -> HashMap<String, DeviceState> {
        let mut conn = self.redis.clone();
        let all: HashMap<String, String> =
            conn.hgetall(REDIS_KEY_DEVICES).await.unwrap_or_else(|e| {
                tracing::warn!("Redis error listing devices: {e}");
                HashMap::new()
            });
        all.into_iter()
            .filter_map(|(k, s)| serde_json::from_str(&s).ok().map(|d| (k, d)))
            .collect()
    }

    pub async fn device_ids(&self) -> redis::RedisResult<Vec<String>> {
        let mut conn = self.redis.clone();
        conn.hkeys(REDIS_KEY_DEVICES).await
    }

    pub async fn register_device(&self, device_id: &str, name: &str) -> DeviceState {
        let now = now_f64();
        let mut dev = self
            .get_device(device_id)
            .await
            .unwrap_or_else(|| DeviceState {
                device_id: device_id.to_string(),
                name: name.to_string(),
                status: "idle".to_string(),
                current_track: None,
                position_ms: 0,
                connected: true,
                last_update: now,
            });
        dev.connected = true;
        dev.apply(DeviceUpdate::new(), now);
        self.write_device(&dev).await;
        dev
    }

    /// Apply an update to a device. Returns false if the device is not
    /// registered, so callers needing a 404 don't have to read it first.
    pub async fn update_device(&self, device_id: &str, upd: DeviceUpdate) -> bool {
        let Some(mut dev) = self.get_device(device_id).await else {
            return false;
        };
        dev.apply(upd, now_f64());
        self.write_device(&dev).await;
        true
    }

    /// Whether a device is registered, without paying to deserialize it.
    pub async fn device_exists(&self, device_id: &str) -> bool {
        let mut conn = self.redis.clone();
        conn.hexists(REDIS_KEY_DEVICES, device_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Redis error checking device {device_id}: {e}");
                false
            })
    }

    /// Record the actual stop position. If still "playing", transition to "paused".
    /// Does not overwrite "paused"/"stopped" already set by a Pause/Stop intent.
    pub async fn pause_if_playing(&self, device_id: &str, offset_ms: u64) {
        let Some(mut dev) = self.get_device(device_id).await else {
            return;
        };
        let mut upd = DeviceUpdate::new().position(offset_ms);
        if dev.status == "playing" {
            upd = upd.status("paused");
        }
        dev.apply(upd, now_f64());
        self.write_device(&dev).await;
    }

    pub async fn remove_device(&self, device_id: &str) -> Option<DeviceState> {
        let device = self.get_device(device_id).await?;
        let mut conn = self.redis.clone();
        warn_redis!(
            "deleting device {device_id}",
            conn.hdel(REDIS_KEY_DEVICES, device_id).await
        );
        self.clear_pending(device_id).await;
        self.clear_queue(device_id).await;
        self.clear_playback_failures(device_id).await;
        Some(device)
    }

    /// Record a playback failure with its position and return the consecutive
    /// failure count for this track (including the current one).
    pub async fn record_playback_failure(
        &self,
        device_id: &str,
        track_id: &str,
        offset_ms: u64,
    ) -> u32 {
        self.playback_failures
            .lock()
            .await
            .entry(device_id.to_string())
            .or_default()
            .entry(track_id.to_string())
            .or_default()
            .record(offset_ms, now_f64())
    }

    /// Discard a device's failure records (next failure starts counting from 1).
    pub async fn clear_playback_failures(&self, device_id: &str) {
        self.playback_failures.lock().await.remove(device_id);
    }

    /// Return device states with their Up Next queues attached. Called from
    /// every Alexa webhook response path and every broadcast, so it is held to
    /// three round-trips regardless of device count: HGETALL for the devices,
    /// one pipeline for all queues, one HMGET for the tracks they reference.
    pub async fn devices_json(&self) -> Value {
        let devices = self.all_devices().await;
        if devices.is_empty() {
            return Value::Object(serde_json::Map::new());
        }

        let mut queues = self.queues_for(devices.keys()).await;
        let tracks = self.fetch_tracks_for(queues.values().flatten()).await;

        let mut map = serde_json::Map::new();
        for (id, dev) in devices {
            let queue: Vec<QueueItem> = queues
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                // Hide entries whose referenced track is gone (peek_queue cleans them up later)
                .filter_map(|entry| {
                    let track = tracks.get(token_track_id(&entry)).cloned()?;
                    Some(QueueItem { entry, track })
                })
                .collect();
            match serde_json::to_value(DeviceJson { device: dev, queue }) {
                Ok(v) => {
                    map.insert(id, v);
                }
                Err(e) => tracing::warn!("Failed to serialize device {id}: {e}"),
            }
        }
        Value::Object(map)
    }

    /// Read every device's queue in one pipeline round-trip. On error each
    /// device gets an empty queue, matching the per-device fallback.
    async fn queues_for(
        &self,
        device_ids: impl Iterator<Item = &String>,
    ) -> HashMap<String, Vec<String>> {
        let ids: Vec<&String> = device_ids.collect();
        let mut pipe = redis::pipe();
        for id in &ids {
            pipe.lrange(queue_key(id), 0, -1);
        }
        let mut conn = self.redis.clone();
        let queues: Vec<Vec<String>> = pipe.query_async(&mut conn).await.unwrap_or_else(|e| {
            tracing::warn!("Redis error reading device queues: {e}");
            vec![Vec::new(); ids.len()]
        });
        ids.into_iter().cloned().zip(queues).collect()
    }

    // ── Pending command ──

    /// Queue playback for a device. `Ok(false)` means the device is not
    /// registered: the existence check and the pending write happen in one
    /// Redis script, so a stale client targeting a deleted device cannot leave
    /// an orphan pending key behind.
    ///
    /// The failure carries no detail because there is nothing a caller could do
    /// with it — what went wrong is logged here, at the point that knows it, the
    /// way every other write in this module reports its own errors.
    pub async fn queue_play(
        &self,
        device_id: &str,
        track: AudioTrack,
        offset_ms: u64,
    ) -> Result<bool, ()> {
        let cmd = PendingCommand {
            action: "play".to_string(),
            track: track.clone(),
            offset_ms,
        };
        let json_str = serde_json::to_string(&cmd).map_err(|e| {
            tracing::warn!("Failed to serialize pending command for {device_id}: {e}");
        })?;
        let mut conn = self.redis.clone();
        let queued: i64 = QUEUE_PLAY_SCRIPT
            .key(REDIS_KEY_DEVICES)
            .key(pending_key(device_id))
            .arg(device_id)
            .arg(json_str)
            .arg(PENDING_TTL_SECS)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| {
                tracing::warn!("Redis error queueing play for {device_id}: {e}");
            })?;
        if queued == 0 {
            return Ok(false);
        }

        // Explicit play command from the web UI — reset consecutive failure
        // records to allow retries.
        self.clear_playback_failures(device_id).await;
        // Align position to the queued start offset (used by Resume and the web UI)
        self.update_device(
            device_id,
            DeviceUpdate::new()
                .status("queued")
                .track(track)
                .position(offset_ms),
        )
        .await;
        Ok(true)
    }

    /// Consume a queued command (atomic get+delete via GETDEL; expiry is handled by Redis TTL).
    pub async fn take_pending(&self, device_id: &str) -> Option<PendingCommand> {
        let mut conn = self.redis.clone();
        let result = conn
            .get_del::<_, Option<String>>(pending_key(device_id))
            .await;
        Self::parse_pending(device_id, result)
    }

    /// Peek at a queued command without consuming it (use take_pending to consume).
    pub async fn peek_pending(&self, device_id: &str) -> Option<PendingCommand> {
        let mut conn = self.redis.clone();
        let result = conn.get::<_, Option<String>>(pending_key(device_id)).await;
        Self::parse_pending(device_id, result)
    }

    /// Discard a queued command.
    pub async fn clear_pending(&self, device_id: &str) {
        let mut conn = self.redis.clone();
        warn_redis!(
            "clearing pending for {device_id}",
            conn.del(pending_key(device_id)).await
        );
    }

    // ── Up Next queue ──

    /// Append a track to the device's Up Next queue. `Ok(false)` means the
    /// device is not registered; the check and the append share one script, so
    /// a stale client cannot leave an orphaned queue behind. The Redis error is
    /// passed through so callers can tell a write failure from a rejected
    /// request.
    pub async fn push_queue(&self, device_id: &str, track_id: &str) -> redis::RedisResult<bool> {
        let mut conn = self.redis.clone();
        PUSH_QUEUE_SCRIPT
            .key(REDIS_KEY_DEVICES)
            .key(queue_key(device_id))
            .arg(device_id)
            .arg(new_token(track_id))
            .invoke_async::<i64>(&mut conn)
            .await
            .map(|pushed| pushed == 1)
            .inspect_err(|e| tracing::warn!("Redis error pushing queue for {device_id}: {e}"))
    }

    /// Peek at the front of the queue as (entry, track) without consuming.
    /// Entries whose referenced track is confirmed deleted are removed and the
    /// next entry is checked. On Redis error, fall back to None (don't remove).
    pub async fn peek_queue(&self, device_id: &str) -> Option<(String, AudioTrack)> {
        let mut conn = self.redis.clone();
        loop {
            let front: Option<String> = match conn.lindex(queue_key(device_id), 0).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Redis error reading queue for {device_id}: {e}");
                    return None;
                }
            };
            let entry = front?;
            match self.try_get_track(token_track_id(&entry)).await {
                Ok(Some(track)) => return Some((entry, track)),
                Ok(None) => {
                    // If removal failed, don't loop — retry on the next peek
                    if !self.remove_queue_entry(device_id, &entry).await {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// Remove one entry by value (LREM). Returns false if not found. Entries are
    /// unique, so no index is needed and concurrent consumption doesn't conflict.
    pub async fn remove_queue_entry(&self, device_id: &str, entry: &str) -> bool {
        let mut conn = self.redis.clone();
        match conn.lrem::<_, _, i64>(queue_key(device_id), 1, entry).await {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("Redis error removing queue entry for {device_id}: {e}");
                false
            }
        }
    }

    /// Clear the entire queue.
    pub async fn clear_queue(&self, device_id: &str) {
        let mut conn = self.redis.clone();
        warn_redis!(
            "clearing queue for {device_id}",
            conn.del(queue_key(device_id)).await
        );
    }

    fn parse_pending(
        device_id: &str,
        result: redis::RedisResult<Option<String>>,
    ) -> Option<PendingCommand> {
        let json_str = match result {
            Ok(Some(s)) => s,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!("Redis error reading pending for {device_id}: {e}");
                return None;
            }
        };
        match serde_json::from_str(&json_str) {
            Ok(cmd) => Some(cmd),
            Err(e) => {
                tracing::warn!("Discarding unparsable pending command for {device_id}: {e}");
                None
            }
        }
    }
}
