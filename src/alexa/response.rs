//! Building what goes back to Alexa: the response envelope, the speech it may
//! carry, and the AudioPlayer directives that actually move playback.
//!
//! Nothing here decides *what* to play — that is [`super::next_up`]'s job. This
//! is only how a decision is worded once it has been made, which is what keeps
//! the directive shape to one definition across every path that sends one.

use super::ReqCtx;
use super::next_up::NextUp;
use crate::state::DeviceUpdate;
use serde_json::{Value, json};

/// Wrap a response body in the standard Alexa response envelope.
pub(super) fn alexa_response(response: Value) -> Value {
    json!({ "version": "1.0", "response": response })
}

/// The bare acknowledgement an event handler returns when it has no directive
/// to send.
pub(super) fn end_session() -> Value {
    alexa_response(json!({ "shouldEndSession": true }))
}

pub(super) fn speech(text: &str, end_session: bool) -> Value {
    alexa_response(json!({
        "outputSpeech": {
            "type": "PlainText",
            "text": text
        },
        "shouldEndSession": end_session
    }))
}

/// Add spoken confirmation to a response that already carries a directive. An
/// AudioPlayer.Play may be announced — Alexa speaks first and starts the audio
/// after — which is what lets "playing X" answer a request that names X.
pub(super) fn with_speech(mut resp: Value, text: &str) -> Value {
    resp["response"]["outputSpeech"] = json!({ "type": "PlainText", "text": text });
    resp
}

/// Response when playback cannot be switched. Requests that cannot carry speech
/// (see ReqCtx::can_speak) get an empty response instead.
pub(super) fn no_track_response(can_speak: bool, text: &str) -> Value {
    if can_speak {
        speech(text, true)
    } else {
        alexa_response(json!({}))
    }
}

/// Update device status and return an AudioPlayer.Stop directive.
pub(super) async fn stop_directive(ctx: &ReqCtx<'_>, status: &str) -> Value {
    ctx.state
        .update_device(&ctx.device_id, DeviceUpdate::new().status(status))
        .await;
    alexa_response(json!({
        "directives": [{ "type": "AudioPlayer.Stop" }],
        "shouldEndSession": true
    }))
}

/// Start `next` now, replacing whatever is playing, and reflect it in the
/// device state right away rather than waiting for PlaybackStarted.
pub(super) async fn play_directive(ctx: &ReqCtx<'_>, next: &NextUp) -> Value {
    ctx.state
        .update_device(
            &ctx.device_id,
            DeviceUpdate::new()
                .status("playing")
                .track(next.track.clone())
                .position(next.offset_ms),
        )
        .await;

    play_response(ctx, next, None)
}

/// Build an AudioPlayer.Play response: ENQUEUE after `enqueue_after`, or
/// REPLACE_ALL without it. An ENQUEUE leaves the device state alone —
/// PlaybackStarted reflects it once playback actually begins.
pub(super) fn play_response(ctx: &ReqCtx<'_>, next: &NextUp, enqueue_after: Option<&str>) -> Value {
    let NextUp {
        track,
        offset_ms,
        token,
    } = next;
    let stream_url = format!(
        "{}{}",
        ctx.base_url,
        crate::auth::stream_path(&ctx.state.stream_secret, &track.id, track.is_live)
    );

    let mut stream = json!({
        "url": stream_url,
        "token": token,
        "offsetInMilliseconds": offset_ms
    });
    let play_behavior = if let Some(prev) = enqueue_after {
        stream["expectedPreviousToken"] = json!(prev);
        "ENQUEUE"
    } else {
        "REPLACE_ALL"
    };

    alexa_response(json!({
        "directives": [{
            "type": "AudioPlayer.Play",
            "playBehavior": play_behavior,
            "audioItem": {
                "stream": stream,
                "metadata": {
                    "title": track.title,
                    "subtitle": if track.channel.is_empty() {
                        "YouTube MultiRoom".to_string()
                    } else {
                        track.channel.clone()
                    }
                }
            }
        }],
        "shouldEndSession": true
    }))
}
