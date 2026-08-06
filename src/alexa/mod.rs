//! The Alexa skill: what an Echo asks of us, and the directives it gets back.
//!
//! A request is proved genuine, then answered. Both halves are grouped by
//! subject into sibling modules, as [`crate::state`] and [`crate::handlers`]
//! are:
//!
//! - [`verify`] — proving the request really came from Amazon, which is what
//!   stands in for the Bearer auth `/alexa` is exempt from
//! - [`session`] — opening the skill: starting something, or picking up
//!   whatever the launch interrupted
//! - [`intent`] — the spoken commands, and the touch and remote controls that
//!   amount to the same thing
//! - [`event`] — what the AudioPlayer reports back: started, finished, failed
//! - [`next_up`] — choosing what plays next, and the token identifying it
//! - [`response`] — the envelope, the speech, and the AudioPlayer directives
//!
//! The modules are private: everything the rest of the crate uses is
//! re-exported here, so `crate::alexa::X` stays the single import path. What
//! they share — the per-request context below — lives here.
//!
//! The HTTP edge itself is [`crate::handlers::alexa_webhook`], which is the one
//! caller of all three re-exports and runs them in that order.

mod event;
mod intent;
mod next_up;
mod response;
mod session;
mod verify;

pub use verify::{verify_request, verify_timestamp};

use crate::state::AppState;
use response::{alexa_response, no_track_response, speech};
use rust_i18n::t;
use serde_json::{Value, json};
use std::sync::Arc;

/// Everything derived once from a request and needed throughout the response.
/// Built in handle_alexa, which is where req_type — what both `locale` and
/// `can_speak` come from — lives, so neither is re-derived downstream.
struct ReqCtx<'a> {
    state: &'a Arc<AppState>,
    device_id: String,
    base_url: &'a str,
    /// Language for spoken responses (Alexa sends the user's locale per request,
    /// so an Echo set to Japanese gets Japanese replies). Falls back to the
    /// built-in default locale.
    locale: String,
    /// Whether this response may carry speech. Only spoken or launched sessions
    /// may; AudioPlayer and PlaybackController responses are directives only.
    can_speak: bool,
}

impl ReqCtx<'_> {
    /// Short device suffix for log lines.
    fn log_id(&self) -> &str {
        tail_chars(&self.device_id, 8)
    }
}

/// Process an Alexa skill request and return a response JSON.
pub async fn handle_alexa(state: &Arc<AppState>, body: Value, base_url: &str) -> Value {
    let req_type = body["request"]["type"].as_str().unwrap_or("");

    // Alexa discards responses to SessionEndedRequest, so an empty body
    // suffices. Returning before registration also keeps a session teardown
    // from resurrecting a device the user just deleted from the web UI.
    if req_type == "SessionEndedRequest" {
        return alexa_response(json!({}));
    }

    let locale = crate::locale::or_default(body["request"]["locale"].as_str());
    let can_speak = matches!(req_type, "LaunchRequest" | "IntentRequest");

    // Every remaining request type acts on one specific device. Without a
    // deviceId there is nothing to act on, and registering a placeholder would
    // leave a phantom device that /api/play-all would then target.
    let Some(device_id) = body["context"]["System"]["device"]["deviceId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        tracing::warn!("Rejecting Alexa {req_type} with no deviceId");
        return no_track_response(can_speak, &t!("alexa_not_understood", locale = &locale));
    };

    let ctx = ReqCtx {
        state,
        device_id,
        base_url,
        locale,
        can_speak,
    };

    let name = format!("Echo-{}", tail_chars(&ctx.device_id, 6));
    state.register_device(&ctx.device_id, &name).await;

    let resp = match req_type {
        "LaunchRequest" => session::on_launch(&ctx).await,
        "IntentRequest" => intent::on_intent(&ctx, &body).await,
        t if t.starts_with("AudioPlayer.") => event::on_audio_event(&ctx, t, &body).await,
        t if t.starts_with("PlaybackController.") => {
            intent::on_playback_controller(&ctx, t, &body).await
        }
        _ => speech(&t!("alexa_not_understood", locale = &ctx.locale), true),
    };

    state.broadcast_devices().await;
    resp
}

fn tail_chars(s: &str, n: usize) -> &str {
    let skip = s.chars().count().saturating_sub(n);
    let offset = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
    &s[offset..]
}
