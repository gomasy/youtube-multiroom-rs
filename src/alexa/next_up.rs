//! Choosing what plays next, and the token that identifies it.
//!
//! Three sources answer that question, in order: a play command the web UI
//! parked in Redis, the device's own Up Next queue, and — only when neither
//! does — whatever the playback mode selects. Every caller that needs a next
//! track walks the same order, which is what keeps an explicit choice ahead of
//! an automatic one.

use super::ReqCtx;
use crate::state::{AudioTrack, auto_token, new_token, token_track_id};
use serde_json::Value;

/// A track chosen to play next, with everything its directive needs.
///
/// The token is not derivable from the track, which is why it travels along:
/// a queue-sourced play reuses the queue entry so PlaybackStarted can consume
/// it by value, and auto-continuation carries a marker PlaybackStarted checks
/// the playback mode against.
pub(super) struct NextUp {
    pub(super) track: AudioTrack,
    pub(super) offset_ms: u64,
    pub(super) token: String,
}

impl NextUp {
    /// A play from `offset_ms` under a freshly minted token: every path that is
    /// neither consuming a queue entry nor auto-continuing.
    pub(super) fn at(track: AudioTrack, offset_ms: u64) -> Self {
        Self {
            token: new_token(&track.id),
            track,
            offset_ms,
        }
    }

    /// The same, from the start of the track.
    pub(super) fn fresh(track: AudioTrack) -> Self {
        Self::at(track, 0)
    }

    /// A play of a queue entry, keyed by the entry itself so PlaybackStarted
    /// and PlaybackFailed can consume it by value match.
    pub(super) fn from_queue(entry: String, track: AudioTrack) -> Self {
        Self {
            track,
            offset_ms: 0,
            token: entry,
        }
    }

    /// A play the playback mode chose, marked so PlaybackStarted can stop it if
    /// the mode was switched off after the ENQUEUE went out.
    pub(super) fn auto(track: AudioTrack) -> Self {
        Self {
            token: auto_token(&track.id),
            track,
            offset_ms: 0,
        }
    }
}

/// The next track: pending command → "next up" queue → playback-mode
/// auto-selection. With `allow_live_auto` false, only the auto-selected track
/// is filtered for liveness — pending and queue items are explicit choices.
pub(super) async fn queued_or_auto_next(
    ctx: &ReqCtx<'_>,
    current_token: &str,
    allow_live_auto: bool,
) -> Option<NextUp> {
    match pending_or_queue_next(ctx, current_token).await {
        Ok(Some(next)) => return Some(next),
        Ok(None) => {}
        // Queue state unconfirmed; skip auto-selection too, for safety
        Err(()) => return None,
    }

    ctx.state
        .auto_next_track(token_track_id(current_token))
        .await
        .filter(|t| allow_live_auto || !t.is_live)
        .map(NextUp::auto)
}

/// The next candidate from pending → "next up" queue, without the playback-mode
/// auto-selection. `Ok(None)` if there is none, `Err` if a Redis error left the
/// queue state unconfirmed. Pending is peeked, not consumed: both the
/// REPLACE_ALL and ENQUEUE paths rely on PlaybackStarted to clear it.
pub(super) async fn pending_or_queue_next(
    ctx: &ReqCtx<'_>,
    current_token: &str,
) -> Result<Option<NextUp>, ()> {
    if let Some(cmd) = ctx.state.peek_pending(&ctx.device_id).await
        && cmd.action == "play"
    {
        return Ok(Some(NextUp::at(cmd.track, cmd.offset_ms)));
    }

    // The playing track's own entry can still be at the head if PlaybackStarted
    // lost its consumption to a Redis error; drop it before looking further.
    while let Some((entry, track)) = ctx.state.peek_queue(&ctx.device_id).await {
        if entry == current_token {
            if !ctx.state.remove_queue_entry(&ctx.device_id, &entry).await {
                return Err(());
            }
            continue;
        }
        return Ok(Some(NextUp::from_queue(entry, track)));
    }
    Ok(None)
}

/// The token identifying the current playback context. The request's
/// AudioPlayer context is preferred — it survives a pause or stop — with the
/// device's current track ID as the fallback.
pub(super) async fn playing_context_token(ctx: &ReqCtx<'_>, body: &Value) -> String {
    if let Some(token) = body["context"]["AudioPlayer"]["token"]
        .as_str()
        .filter(|t| !t.is_empty())
    {
        return token.to_string();
    }
    ctx.state
        .get_device(&ctx.device_id)
        .await
        .and_then(|d| d.current_track)
        .map(|t| t.id)
        .unwrap_or_default()
}
