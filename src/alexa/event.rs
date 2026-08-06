//! What the AudioPlayer reports back once a directive has gone out: that
//! playback started, finished, stopped, is nearly done, or failed.
//!
//! These are the only requests nobody asked for, and the ones that keep the
//! device state honest — a play is adopted here, the entry that queued it is
//! retired here, and a failure is either retried or given up on here.

use super::ReqCtx;
use super::next_up::{NextUp, queued_or_auto_next};
use super::response::{end_session, play_directive, play_response, stop_directive};
use crate::state::{
    AudioTrack, DeviceUpdate, auto_token, is_auto_token, new_token, token_track_id,
};
use serde_json::Value;

/// Maximum retries for the same track on PlaybackFailed before marking as error.
const MAX_PLAYBACK_RETRIES: u32 = 3;

/// Everything an AudioPlayer event reports about the playback it concerns.
/// Read once in [`on_audio_event`], so no two handlers can read a field
/// differently.
struct AudioEvent<'a> {
    /// The AudioPlayer token, which is also the queue entry for a
    /// queue-sourced play (see [`NextUp`]).
    token: &'a str,
    /// The track the token names, whether or not it is still registered.
    track_id: &'a str,
    /// Playback position the event reports, in milliseconds.
    offset: u64,
}

pub(super) async fn on_audio_event(ctx: &ReqCtx<'_>, event_type: &str, body: &Value) -> Value {
    let token = body["request"]["token"].as_str().unwrap_or("");
    let event = AudioEvent {
        token,
        track_id: token_track_id(token),
        offset: body["request"]["offsetInMilliseconds"]
            .as_u64()
            .unwrap_or(0),
    };

    match event_type {
        "AudioPlayer.PlaybackStarted" => return on_playback_started(ctx, &event).await,
        "AudioPlayer.PlaybackFinished" => {
            ctx.state.clear_playback_failures(&ctx.device_id).await;
            mark_idle(ctx).await;
        }
        "AudioPlayer.PlaybackStopped" => {
            // Also fires for external interruptions (other content starting).
            // "paused" stops the client's estimated position from advancing
            // while playback is actually stopped.
            ctx.state
                .pause_if_playing(&ctx.device_id, event.offset)
                .await;
        }
        "AudioPlayer.PlaybackNearlyFinished" => {
            // Pending is left for PlaybackStarted to consume, so a discarded
            // ENQUEUE loses nothing and a replayed event is idempotent.
            if let Some(next) = queued_or_auto_next(ctx, event.token, true).await {
                tracing::info!(
                    "Enqueueing next track '{}' on {}",
                    next.track.title,
                    ctx.log_id()
                );
                return play_response(ctx, &next, Some(event.token));
            }
        }
        "AudioPlayer.PlaybackFailed" => return on_playback_failed(ctx, &event, body).await,
        _ => {}
    }

    end_session()
}

/// Playback of `event.token` has begun: adopt it as the device's current track
/// and retire whatever queued it.
async fn on_playback_started(ctx: &ReqCtx<'_>, event: &AudioEvent<'_>) -> Value {
    let state = ctx.state;
    let device_id = &ctx.device_id;

    // An auto-continued track (loop/shuffle ENQUEUE) whose mode was switched to
    // "off" in the meantime is stopped here: NearlyFinished often fires right
    // after playback starts, so the check at ENQUEUE time is not enough.
    // playback_mode_is_off errs towards NOT stopping on a Redis error.
    if is_auto_token(event.token) && state.playback_mode_is_off().await {
        tracing::info!(
            "Stopping auto-continued track on {} (playback mode is off)",
            ctx.log_id()
        );
        return stop_directive(ctx, "idle").await;
    }
    tracing::info!("Playback started: {}", ctx.log_id());
    // A seek-based ENQUEUE can start from a non-zero offset, so the position
    // comes from the event rather than being assumed to be zero.
    let mut upd = DeviceUpdate::new().status("playing").position(event.offset);
    if let Some(track) = state.get_track(event.track_id).await {
        upd = upd.track(track);
    }
    state.update_device(device_id, upd).await;
    // Retire the pending command this play satisfied. The offset is compared
    // too, so a newer seek that arrived between the directive and the start is
    // not cleared by it.
    if state
        .peek_pending(device_id)
        .await
        .is_some_and(|cmd| cmd.track.id == event.track_id && cmd.offset_ms == event.offset)
    {
        state.clear_pending(device_id).await;
    }
    // A queue-sourced play is consumed by value match here rather than at
    // ENQUEUE time, so a discarded directive loses no track and a replayed
    // event consumes nothing twice. Pending and auto tokens match no entry.
    state.remove_queue_entry(device_id, event.token).await;

    end_session()
}

/// The device could not play `event.token`. A track that has since been deleted
/// leaves the device idle, an ended live stream is normal termination and
/// advances like PlaybackFinished, and anything else is retried a few times
/// before the device is marked as errored.
async fn on_playback_failed(ctx: &ReqCtx<'_>, event: &AudioEvent<'_>, body: &Value) -> Value {
    let state = ctx.state;
    let device_id = &ctx.device_id;
    let err = &body["request"]["error"];

    // Consume a queue-sourced entry so an unplayable item (an ended live
    // stream, say) cannot block the tracks behind it.
    state.remove_queue_entry(device_id, event.token).await;

    // A track confirmed gone explains the failure by itself: it was deleted
    // mid-playback, cache file and all. Nothing is wrong with the device and
    // nothing is left to retry, so it is parked the way a finished track is
    // rather than left reporting an error for something the user chose.
    //
    // Hence try_get_track: get_track reads a Redis error as "missing" too, and
    // an unread track is no basis for that explanation. An unreadable one takes
    // the retry path instead, which declines without a track and marks the
    // device errored — the honest answer when nothing could be confirmed.
    let track = match state.try_get_track(event.track_id).await {
        Ok(Some(track)) => Some(track),
        Ok(None) => {
            tracing::info!(
                "Playback failed on {} for deleted track {}; leaving it idle",
                ctx.log_id(),
                event.track_id
            );
            mark_idle(ctx).await;
            return end_session();
        }
        Err(e) => {
            tracing::warn!(
                "Redis error reading track {} for a playback failure: {e}",
                event.track_id
            );
            None
        }
    };

    // A live stream becomes unresolvable once it ends, which surfaces as
    // PlaybackFailed. That is normal termination, so it advances like
    // PlaybackFinished rather than counting as an error.
    if let Some(track) = track.as_ref().filter(|t| t.is_live) {
        tracing::info!(
            "Live stream '{}' ended on {} ({:?})",
            track.title,
            ctx.log_id(),
            err
        );
        mark_idle(ctx).await;
        // No auto-selecting another live stream: it may have ended too, and a
        // chain of failures would follow. An explicit pending command is still
        // tried once — it survives a failure, and the same-track exclusion
        // stops it from retrying forever.
        if let Some(next) = queued_or_auto_next(ctx, event.token, false)
            .await
            .filter(|next| next.track.id != event.track_id)
        {
            return play_directive(ctx, &next).await;
        }
    } else {
        // Transient failures (network drops) are retried; only an exhausted
        // retry budget marks the device as errored.
        if let Some(resp) = retry_playback(ctx, body, event.token, track, err).await {
            return resp;
        }
        tracing::error!("Playback failed on {}: {:?}", ctx.log_id(), err);
        state
            .update_device(device_id, DeviceUpdate::new().status("error"))
            .await;
    }

    end_session()
}

/// Park the device at the start of nothing: what both a track finishing and a
/// live stream ending leave behind.
async fn mark_idle(ctx: &ReqCtx<'_>) {
    ctx.state
        .update_device(
            &ctx.device_id,
            DeviceUpdate::new().status("idle").position(0),
        )
        .await;
}

/// Build a retry directive for PlaybackFailed. None when the track cannot be
/// resolved, the situation does not warrant a retry, or consecutive failures
/// exceed MAX_PLAYBACK_RETRIES — the caller marks the device errored.
async fn retry_playback(
    ctx: &ReqCtx<'_>,
    body: &Value,
    token: &str,
    track: Option<AudioTrack>,
    err: &Value,
) -> Option<Value> {
    let track = track?;
    let cps = &body["request"]["currentPlaybackState"];
    let failed_current = cps["token"].as_str() == Some(token);

    // An ENQUEUE'd next track that failed while nothing is playing is not
    // retried: the REPLACE_ALL would resume playback the user had stopped.
    if !failed_current && cps["playerActivity"].as_str() != Some("PLAYING") {
        return None;
    }

    // The measured position for the track that was playing; for one that never
    // started, the pending command's offset, so a web seek is preserved and
    // PlaybackStarted still recognizes the pending it satisfies.
    let offset_ms = if failed_current {
        cps["offsetInMilliseconds"].as_u64().unwrap_or(0)
    } else {
        ctx.state
            .peek_pending(&ctx.device_id)
            .await
            .filter(|cmd| cmd.action == "play" && cmd.track.id == track.id)
            .map_or(0, |cmd| cmd.offset_ms)
    };

    let failures = ctx
        .state
        .record_playback_failure(&ctx.device_id, &track.id, offset_ms)
        .await;
    if failures > MAX_PLAYBACK_RETRIES {
        return None;
    }

    tracing::warn!(
        "Playback failed on {} (attempt {failures}/{MAX_PLAYBACK_RETRIES}), \
         retrying '{}' from {offset_ms}ms: {:?}",
        ctx.log_id(),
        track.title,
        err
    );

    // The auto-continuation marker only carries forward while playback has not
    // progressed: resuming a partially played track is not a new
    // auto-continuation and must not be stopped by a mode switched to "off".
    let retry = NextUp {
        token: if is_auto_token(token) && offset_ms == 0 {
            auto_token(&track.id)
        } else {
            new_token(&track.id)
        },
        track,
        offset_ms,
    };

    // Retried as an ENQUEUE when it was the next track that failed, so the one
    // still playing is not interrupted.
    if !failed_current {
        let current = cps["token"].as_str()?;
        return Some(play_response(ctx, &retry, Some(current)));
    }
    Some(play_directive(ctx, &retry).await)
}
