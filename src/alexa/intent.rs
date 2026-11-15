//! The commands a person gives: spoken intents, and the touch controls on an
//! Echo Show or the buttons on a remote, which arrive as a different request
//! type but mean the same things.

use super::ReqCtx;
use super::next_up::{NextUp, pending_or_queue_next, playing_context_token};
use super::response::{
    alexa_response, no_track_response, play_directive, play_explicit, speech, stop_directive,
    with_speech,
};
use super::session::{resume_playback, start_or_resume};
use crate::state::token_track_id;
use rust_i18n::t;
use serde_json::{Value, json};

pub(super) async fn on_intent(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    let intent = body["request"]["intent"]["name"].as_str().unwrap_or("");
    let locale = &ctx.locale;

    match intent {
        "PlayFromWebIntent" => match start_or_resume(ctx).await {
            Some(resp) => resp,
            None => speech(&t!("alexa_no_queued_track", locale = locale), true),
        },

        "PlayTrackIntent" => play_named_track(ctx, slot_value(body, "query")).await,

        "PlayPlaylistIntent" => play_named_playlist(ctx, slot_value(body, "name")).await,

        "AMAZON.PauseIntent" => stop_directive(ctx, "paused").await,

        "AMAZON.StopIntent" | "AMAZON.CancelIntent" => stop_directive(ctx, "stopped").await,

        "AMAZON.ResumeIntent" => resume_playback(ctx).await,

        "AMAZON.NextIntent" => skip_next(ctx, body).await,

        "AMAZON.PreviousIntent" => skip_prev(ctx, body).await,

        "AMAZON.StartOverIntent" => start_over(ctx, body).await,

        "AMAZON.ShuffleOnIntent" => set_mode(ctx, "shuffle").await,

        "AMAZON.LoopOnIntent" => set_mode(ctx, "loop").await,

        "AMAZON.RepeatIntent" => set_mode(ctx, "repeat_one").await,

        // Each "off" only leaves the modes it names; shuffle falls back to
        // playing in order rather than to silence.
        "AMAZON.ShuffleOffIntent" => leave_mode(ctx, &["shuffle"], "loop").await,

        "AMAZON.LoopOffIntent" => leave_mode(ctx, &["loop", "repeat_one"], "off").await,

        "AMAZON.HelpIntent" => speech(&t!("alexa_help", locale = locale), false),

        _ => speech(&t!("alexa_use_web", locale = locale), false),
    }
}

/// Play the library track a spoken phrase names, on the device that heard it.
/// Nothing close enough plays nothing at all.
async fn play_named_track(ctx: &ReqCtx<'_>, query: &str) -> Value {
    let locale = &ctx.locale;
    if query.is_empty() {
        return speech(&t!("alexa_not_understood", locale = locale), true);
    }
    let Some(track) = ctx.state.find_track(query).await else {
        return speech(
            &t!("alexa_track_not_found", locale = locale, query = query),
            true,
        );
    };

    let title = track.title.clone();
    let resp = play_explicit(ctx, track, 0).await;
    with_speech(resp, &t!("alexa_playing", locale = locale, title = title))
}

/// Switch the selection scope to the playlist a spoken phrase names and start
/// it from the top, so what follows the first track comes from that playlist.
async fn play_named_playlist(ctx: &ReqCtx<'_>, name: &str) -> Value {
    let locale = &ctx.locale;
    if name.is_empty() {
        return speech(&t!("alexa_not_understood", locale = locale), true);
    }
    let Some(playlist) = ctx.state.find_playlist(name).await else {
        return speech(
            &t!("alexa_playlist_not_found", locale = locale, name = name),
            true,
        );
    };
    let Some(track) = ctx.state.first_playlist_track(&playlist.id).await else {
        return speech(
            &t!(
                "alexa_playlist_empty",
                locale = locale,
                name = playlist.name
            ),
            true,
        );
    };

    if ctx.state.set_active_playlist(Some(&playlist.id)).await {
        ctx.state.broadcast_active_playlist().await;
    }
    let resp = play_explicit(ctx, track, 0).await;
    with_speech(
        resp,
        &t!(
            "alexa_playing_playlist",
            locale = locale,
            name = playlist.name
        ),
    )
}

/// Set the global playback mode and confirm it by speech alone, so the track
/// carries on.
async fn set_mode(ctx: &ReqCtx<'_>, mode: &str) -> Value {
    let locale = &ctx.locale;
    let said = match mode {
        "shuffle" => t!("alexa_mode_shuffle", locale = locale),
        "loop" => t!("alexa_mode_loop", locale = locale),
        "repeat_one" => t!("alexa_mode_repeat_one", locale = locale),
        "off" => t!("alexa_mode_off", locale = locale),
        _ => return speech(&t!("alexa_mode_failed", locale = locale), true),
    };
    if !ctx.state.set_playback_mode(mode).await {
        return speech(&t!("alexa_mode_failed", locale = locale), true);
    }
    ctx.state.broadcast_playback_mode(mode);
    speech(&said, true)
}

/// Switch to `fallback` if the current mode is one of `leaving`; otherwise the
/// mode stands and is only confirmed.
async fn leave_mode(ctx: &ReqCtx<'_>, leaving: &[&str], fallback: &str) -> Value {
    let current = ctx.state.playback_mode().await;
    let mode = if leaving.contains(&current.as_str()) {
        fallback
    } else {
        current.as_str()
    };
    set_mode(ctx, mode).await
}

async fn start_over(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    let current_token = playing_context_token(ctx, body).await;
    let Some(track) = ctx.state.get_track(token_track_id(&current_token)).await else {
        return no_track_response(ctx.can_speak, &t!("alexa_no_track", locale = &ctx.locale));
    };
    play_explicit(ctx, track, 0).await
}

/// What Alexa filled a slot with, trimmed; empty when it heard nothing usable.
fn slot_value<'a>(body: &'a Value, name: &str) -> &'a str {
    body["request"]["intent"]["slots"][name]["value"]
        .as_str()
        .unwrap_or("")
        .trim()
}

/// Explicit "next track" skip. Priority: pending → "next up" queue → playback
/// scope order (random for shuffle). Advances even when playback mode is "off"
/// since this is an explicit user command.
async fn skip_next(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    let current_token = playing_context_token(ctx, body).await;

    let next = match pending_or_queue_next(ctx, &current_token).await {
        Ok(Some(next)) => Some(next),
        Ok(None) => ctx
            .state
            .skip_next_track(token_track_id(&current_token))
            .await
            .map(NextUp::fresh),
        // Queue state unconfirmed; stay on the current track to be safe
        Err(()) => None,
    };
    match next {
        Some(next) => play_directive(ctx, &next).await,
        None => no_track_response(ctx.can_speak, &t!("alexa_no_next", locale = &ctx.locale)),
    }
}

/// Explicit "previous track" skip (previous in scope order; wraps from first
/// to last).
async fn skip_prev(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    let current_token = playing_context_token(ctx, body).await;
    match ctx
        .state
        .skip_prev_track(token_track_id(&current_token))
        .await
    {
        Some(track) => play_directive(ctx, &NextUp::fresh(track)).await,
        None => no_track_response(ctx.can_speak, &t!("alexa_no_prev", locale = &ctx.locale)),
    }
}

/// Touch controls on Echo Show or physical remote buttons. The response cannot
/// include speech — only AudioPlayer directives are valid.
pub(super) async fn on_playback_controller(
    ctx: &ReqCtx<'_>,
    event_type: &str,
    body: &Value,
) -> Value {
    match event_type {
        "PlaybackController.PlayCommandIssued" => resume_playback(ctx).await,
        "PlaybackController.PauseCommandIssued" => stop_directive(ctx, "paused").await,
        "PlaybackController.NextCommandIssued" => skip_next(ctx, body).await,
        "PlaybackController.PreviousCommandIssued" => skip_prev(ctx, body).await,
        _ => alexa_response(json!({})),
    }
}
