//! Opening the skill, and picking playback back up.
//!
//! A launch and an explicit Resume come down to the same question — is there
//! anything to start? — so they share the answer to it. What differs is only
//! what is said when there is nothing.

use super::ReqCtx;
use super::next_up::NextUp;
use super::response::{no_track_response, play_directive, speech};
use crate::state::{AudioTrack, DeviceUpdate};
use rust_i18n::t;
use serde_json::Value;

pub(super) async fn on_launch(ctx: &ReqCtx<'_>) -> Value {
    if let Some(resp) = start_or_resume(ctx).await {
        return resp;
    }

    // Nothing to play, so the device the launch interrupted is now idle.
    // handle_alexa registered it, so the update lands.
    ctx.state
        .update_device(&ctx.device_id, DeviceUpdate::new().status("idle"))
        .await;

    speech(&t!("alexa_connected", locale = &ctx.locale), false)
}

/// Start whatever opening the skill should start: a pending web command, the
/// head of the Up Next queue, or — since the launch interrupted it — the track
/// the device was part-way through. None when there is nothing to play.
///
/// LaunchRequest and PlayFromWebIntent share this because Alexa routes
/// skill-opening phrases to either one; only the empty-handed reply differs.
pub(super) async fn start_or_resume(ctx: &ReqCtx<'_>) -> Option<Value> {
    if let Some(resp) = start_pending_or_queue(ctx).await {
        return Some(resp);
    }

    // Only an interrupted track is resumed. A finished one leaves the device
    // idle while still remembering it, and replaying it from the top is not
    // what opening the skill asked for.
    let dev = ctx.state.get_device(&ctx.device_id).await?;
    if !dev.playback_in_progress() {
        return None;
    }
    Some(resume_at(ctx, dev.current_track?, dev.position_ms).await)
}

/// Start playback from a pending command or the front of the "next up" queue,
/// or None if neither is available.
///
/// The counterpart to [`super::next_up::pending_or_queue_next`], which only
/// *chooses* the next track for a skip or an auto-continuation. This one starts
/// playback as the response to a launch or resume, so it consumes pending
/// rather than peeking at it and leaves the queue alone while a track is in
/// progress (a seek reload would otherwise discard one).
async fn start_pending_or_queue(ctx: &ReqCtx<'_>) -> Option<Value> {
    if let Some(cmd) = ctx.state.take_pending(&ctx.device_id).await
        && cmd.action == "play"
    {
        tracing::info!("Auto-playing queued track on {}", ctx.log_id());
        let next = NextUp::at(cmd.track, cmd.offset_ms);
        return Some(play_directive(ctx, &next).await);
    }

    let in_progress = ctx
        .state
        .get_device(&ctx.device_id)
        .await
        .is_some_and(|d| d.playback_in_progress());
    if !in_progress && let Some((entry, track)) = ctx.state.peek_queue(&ctx.device_id).await {
        tracing::info!("Starting next-up track on {}", ctx.log_id());
        return Some(play_directive(ctx, &NextUp::from_queue(entry, track)).await);
    }
    None
}

/// Resume the current track from its estimated position (Resume intent / play
/// button). If a web-queued command or "next up" entry is waiting, start that
/// instead.
pub(super) async fn resume_playback(ctx: &ReqCtx<'_>) -> Value {
    if let Some(resp) = start_pending_or_queue(ctx).await {
        return resp;
    }
    if let Some(dev) = ctx.state.get_device(&ctx.device_id).await
        && let Some(track) = dev.current_track
    {
        return resume_at(ctx, track, dev.position_ms).await;
    }
    no_track_response(ctx.can_speak, &t!("alexa_no_track", locale = &ctx.locale))
}

/// Pick `track` up again from `offset_ms`: what an explicit Resume and a launch
/// that interrupted playback both come down to.
async fn resume_at(ctx: &ReqCtx<'_>, track: AudioTrack, offset_ms: u64) -> Value {
    // Carrying on is a fresh chance for the track, so the failure counter is
    // cleared — otherwise one left in the error state errors again at once.
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    play_directive(ctx, &NextUp::at(track, offset_ms)).await
}
