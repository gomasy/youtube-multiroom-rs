//! Playback mode (what happens when a track ends) and the sleep timer.

use super::model::DeviceUpdate;
use super::warn_redis;
use super::{AppState, REDIS_KEY_PLAYBACK_MODE, REDIS_KEY_SLEEP_TIMER, now_f64};
use redis::AsyncCommands;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::time;

const PLAYBACK_MODES: [&str; 3] = ["loop", "shuffle", "off"];
const DEFAULT_PLAYBACK_MODE: &str = "off";

/// Upper bound for the sleep timer. Anything longer is indistinguishable from a
/// bogus value, and `minutes * 60` must not be allowed to overflow u64.
const MAX_SLEEP_TIMER_MINUTES: u64 = 24 * 60;

impl AppState {
    /// Return the saved playback mode (invalid/missing values are normalized to
    /// the default; Redis errors are returned as Err for the caller to decide).
    async fn try_playback_mode(&self) -> redis::RedisResult<String> {
        let mut conn = self.redis.clone();
        let mode: Option<String> = conn.get(REDIS_KEY_PLAYBACK_MODE).await?;
        Ok(mode
            .filter(|m| PLAYBACK_MODES.contains(&m.as_str()))
            .unwrap_or_else(|| DEFAULT_PLAYBACK_MODE.to_string()))
    }

    /// Return the playback mode (end-of-track behavior). Falls back to default on Redis error.
    pub async fn playback_mode(&self) -> String {
        self.try_playback_mode().await.unwrap_or_else(|e| {
            tracing::warn!("Redis error reading playback mode: {e}");
            DEFAULT_PLAYBACK_MODE.to_string()
        })
    }

    /// Return true only when the mode is confirmed to be "off". Used to decide
    /// whether to stop ongoing playback, so on Redis error it falls back to
    /// false (don't stop) — since the default is "off", playback_mode() alone
    /// can't distinguish a transient error from a genuine "off".
    pub async fn playback_mode_is_off(&self) -> bool {
        match self.try_playback_mode().await {
            Ok(mode) => mode == "off",
            Err(e) => {
                tracing::warn!("Redis error reading playback mode: {e}");
                false
            }
        }
    }

    /// Save the playback mode. Returns false for unknown values or Redis errors.
    pub async fn set_playback_mode(&self, mode: &str) -> bool {
        if !PLAYBACK_MODES.contains(&mode) {
            return false;
        }
        let mut conn = self.redis.clone();
        match conn.set::<_, _, ()>(REDIS_KEY_PLAYBACK_MODE, mode).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("Redis error writing playback mode: {e}");
                false
            }
        }
    }

    // ── Sleep timer ──

    /// Return the sleep timer expiry as UNIX seconds, or None if not set / expired.
    pub async fn sleep_timer(&self) -> Option<f64> {
        let mut conn = self.redis.clone();
        let expiry: Option<f64> = match conn.get(REDIS_KEY_SLEEP_TIMER).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Redis error reading sleep timer: {e}");
                return None;
            }
        };
        expiry.filter(|&t| t > now_f64())
    }

    /// Set a sleep timer that fires after `minutes` minutes. Spawns a task that
    /// stops all devices and sets playback mode to "off" when it expires.
    /// Returns the expiry time (UNIX seconds), or None if `minutes` is out of
    /// range.
    ///
    /// The range is owned here rather than at the caller: `minutes` arrives
    /// straight from a WebSocket payload, and `minutes * 60` would overflow u64
    /// on an absurd value. Rejected rather than clamped — a client asking for a
    /// year of sleep timer is not asking for a day — which also leaves a running
    /// timer undisturbed by a nonsense request.
    pub async fn set_sleep_timer(self: &Arc<Self>, minutes: u64) -> Option<f64> {
        if !(1..=MAX_SLEEP_TIMER_MINUTES).contains(&minutes) {
            tracing::warn!("Rejecting out-of-range sleep timer: {minutes} minutes");
            return None;
        }
        let generation = self.sleep_timer_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let expiry = now_f64() + (minutes as f64) * 60.0;

        let mut conn = self.redis.clone();
        let ttl_secs = (minutes * 60) + 10;
        warn_redis!(
            "writing sleep timer",
            conn.set_ex(REDIS_KEY_SLEEP_TIMER, expiry, ttl_secs).await
        );

        self.spawn_sleep_expiry(time::Duration::from_secs(minutes * 60), generation);
        Some(expiry)
    }

    /// Re-spawn the sleep timer task after a server restart. If a timer is
    /// still active in Redis, calculate the remaining time and schedule the
    /// expiry task so the timer actually fires.
    pub async fn restore_sleep_timer(self: &Arc<Self>) {
        let Some(expiry) = self.sleep_timer().await else {
            return;
        };
        let remaining_secs = expiry - now_f64();
        if remaining_secs <= 0.0 {
            self.cancel_sleep_timer().await;
            return;
        }
        let generation = self.sleep_timer_gen.load(Ordering::Relaxed);
        self.spawn_sleep_expiry(time::Duration::from_secs_f64(remaining_secs), generation);
        tracing::info!("Restored sleep timer ({remaining_secs:.0}s remaining)");
    }

    fn spawn_sleep_expiry(self: &Arc<Self>, delay: time::Duration, generation: u64) {
        let state = self.clone();
        tokio::spawn(async move {
            time::sleep(delay).await;
            if state.sleep_timer_gen.load(Ordering::Relaxed) != generation {
                return;
            }
            tracing::info!("Sleep timer expired, stopping all devices");
            let mut conn = state.redis.clone();
            warn_redis!(
                "clearing expired sleep timer",
                conn.del(REDIS_KEY_SLEEP_TIMER).await
            );
            state.set_playback_mode("off").await;
            state.broadcast_playback_mode("off").await;
            let device_ids = state.device_ids().await.unwrap_or_else(|e| {
                tracing::warn!("Redis error listing devices for sleep timer: {e}");
                Vec::new()
            });
            for did in &device_ids {
                state
                    .update_device(did, DeviceUpdate::new().status("stopped"))
                    .await;
            }
            state.broadcast_devices().await;
            state.broadcast_sleep_timer().await;
        });
    }

    /// Cancel the sleep timer.
    pub async fn cancel_sleep_timer(&self) {
        self.sleep_timer_gen.fetch_add(1, Ordering::Relaxed);
        let mut conn = self.redis.clone();
        warn_redis!(
            "cancelling sleep timer",
            conn.del(REDIS_KEY_SLEEP_TIMER).await
        );
    }

    pub async fn broadcast_sleep_timer(&self) {
        self.broadcast(json!({
            "type": "sleep_timer_update",
            "expires_at": self.sleep_timer().await,
        }));
    }

    pub async fn broadcast_playback_mode(&self, mode: &str) {
        self.broadcast(json!({
            "type": "playback_mode_update",
            "mode": mode,
        }));
    }
}
