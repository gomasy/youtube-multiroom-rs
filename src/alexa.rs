use crate::state::{
    AppState, AudioTrack, DeviceUpdate, auto_token, is_auto_token, new_token, token_track_id,
};
use rust_i18n::t;
use serde_json::{Value, json};
use std::sync::Arc;

/// Maximum retries for the same track on PlaybackFailed before marking as error.
const MAX_PLAYBACK_RETRIES: u32 = 3;

/// Everything derived once from a request and needed throughout the response.
/// Built in handle_alexa, which is where req_type — the thing both `locale` and
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

/// A track chosen to play next, with everything the directive for it needs.
///
/// The three travel together through every selection path — pending command,
/// Up Next queue, playback-mode auto-selection — and the token is not derivable
/// from the track: a queue-sourced play uses the queue entry itself so
/// PlaybackStarted can consume it by value, and auto-continuation carries a
/// marker that PlaybackStarted checks the playback mode against.
struct NextUp {
    track: AudioTrack,
    offset_ms: u64,
    token: String,
}

impl NextUp {
    /// A play of `track` from `offset_ms` under a freshly minted token — what
    /// every path that is neither consuming a queue entry nor carrying an
    /// auto-continuation marker wants.
    fn at(track: AudioTrack, offset_ms: u64) -> Self {
        Self {
            token: new_token(&track.id),
            track,
            offset_ms,
        }
    }

    /// The same, from the start of the track.
    fn fresh(track: AudioTrack) -> Self {
        Self::at(track, 0)
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
        "LaunchRequest" => on_launch(&ctx).await,
        "IntentRequest" => on_intent(&ctx, &body).await,
        t if t.starts_with("AudioPlayer.") => on_audio_event(&ctx, t, &body).await,
        t if t.starts_with("PlaybackController.") => on_playback_controller(&ctx, t, &body).await,
        _ => speech(&t!("alexa_not_understood", locale = &ctx.locale), true),
    };

    state.broadcast_devices().await;
    resp
}

// ── Launch ──

async fn on_launch(ctx: &ReqCtx<'_>) -> Value {
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
/// the device was playing or paused mid-way through. None when it has nothing
/// to play.
///
/// LaunchRequest and PlayFromWebIntent both go through here because Alexa
/// routes skill-opening phrases to either one and the user means the same thing
/// by both. Only the reply when there is nothing to play differs.
async fn start_or_resume(ctx: &ReqCtx<'_>) -> Option<Value> {
    if let Some(resp) = start_pending_or_queue(ctx).await {
        return Some(resp);
    }

    // Only an interrupted track is resumed: one that already finished leaves
    // the device idle while still remembering it, and replaying that from the
    // top is not what opening the skill asked for.
    let dev = ctx.state.get_device(&ctx.device_id).await?;
    if !dev.playback_in_progress() {
        return None;
    }
    Some(resume_at(ctx, dev.current_track?, dev.position_ms).await)
}

/// Start playback from a pending command or the front of the "next up" queue.
/// Returns None if neither is available. Queue playback is limited to when
/// nothing is currently playing (to avoid discarding a track during seek
/// reloads, etc.). Queue entries are used directly as the token and consumed
/// by PlaybackStarted on value match.
///
/// Similar to pending_or_queue_next but with a different role: this one
/// "immediately starts playback as a launch/resume response," consumes
/// pending via take, and guards against interrupting an in-progress track.
/// Use pending_or_queue_next for choosing the next track on skip/auto-continue.
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
        let next = NextUp {
            track,
            offset_ms: 0,
            token: entry,
        };
        return Some(play_directive(ctx, &next).await);
    }
    None
}

// ── Intents ──

async fn on_intent(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    let intent = body["request"]["intent"]["name"].as_str().unwrap_or("");
    let locale = &ctx.locale;

    match intent {
        "PlayFromWebIntent" => match start_or_resume(ctx).await {
            Some(resp) => resp,
            None => speech(&t!("alexa_no_queued_track", locale = locale), true),
        },

        "AMAZON.PauseIntent" => stop_directive(ctx, "paused").await,

        "AMAZON.StopIntent" | "AMAZON.CancelIntent" => stop_directive(ctx, "stopped").await,

        "AMAZON.ResumeIntent" => resume_playback(ctx).await,

        "AMAZON.NextIntent" => skip_next(ctx, body).await,

        "AMAZON.PreviousIntent" => skip_prev(ctx, body).await,

        "AMAZON.HelpIntent" => speech(&t!("alexa_help", locale = locale), false),

        _ => speech(&t!("alexa_use_web", locale = locale), false),
    }
}

/// Resume the current track from its estimated position (Resume intent / play
/// button). If a web-queued command or "next up" entry is waiting, start that
/// instead.
async fn resume_playback(ctx: &ReqCtx<'_>) -> Value {
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
    // Carrying on with a track is a fresh chance for it, so clear the failure
    // counter — otherwise one left in the error state errors again immediately.
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    play_directive(ctx, &NextUp::at(track, offset_ms)).await
}

/// Explicit "next track" skip. Priority: pending → "next up" queue → playback
/// scope order (random for shuffle). Advances even when playback mode is "off"
/// since this is an explicit user command.
async fn skip_next(ctx: &ReqCtx<'_>, body: &Value) -> Value {
    // Reset failure counter on explicit action
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    let current_token = playing_context_token(ctx, body).await;

    let next = match pending_or_queue_next(ctx, &current_token).await {
        Ok(Some(next)) => Some(next),
        Ok(None) => ctx
            .state
            .skip_next_track(token_track_id(&current_token))
            .await
            .map(NextUp::fresh),
        // Cannot confirm queue state; stay on current track to be safe
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

// ── PlaybackController Events ──

/// Touch controls on Echo Show or physical remote buttons. The response cannot
/// include speech — only AudioPlayer directives are valid.
async fn on_playback_controller(ctx: &ReqCtx<'_>, event_type: &str, body: &Value) -> Value {
    match event_type {
        "PlaybackController.PlayCommandIssued" => resume_playback(ctx).await,
        "PlaybackController.PauseCommandIssued" => stop_directive(ctx, "paused").await,
        "PlaybackController.NextCommandIssued" => skip_next(ctx, body).await,
        "PlaybackController.PreviousCommandIssued" => skip_prev(ctx, body).await,
        _ => alexa_response(json!({})),
    }
}

// ── AudioPlayer Events ──

/// Everything an AudioPlayer event reports about the playback it concerns.
/// Read once in [`on_audio_event`] and passed to whichever handler the event
/// type selects, so no two of them can read the same field differently.
struct AudioEvent<'a> {
    /// The AudioPlayer token, which is also the queue entry for a
    /// queue-sourced play (see [`NextUp`]).
    token: &'a str,
    /// The track the token names, whether or not it is still registered.
    track_id: &'a str,
    /// Playback position the event reports, in milliseconds.
    offset: u64,
}

async fn on_audio_event(ctx: &ReqCtx<'_>, event_type: &str, body: &Value) -> Value {
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
            // Track finished successfully; reset failure counter
            ctx.state.clear_playback_failures(&ctx.device_id).await;
            mark_idle(ctx).await;
        }
        "AudioPlayer.PlaybackStopped" => {
            // Also fires for external interruptions (e.g., another content starting).
            // Transition to "paused" to stop the client's estimated position from
            // advancing while playback is actually stopped.
            ctx.state
                .pause_if_playing(&ctx.device_id, event.offset)
                .await;
        }
        "AudioPlayer.PlaybackNearlyFinished" => {
            // Don't consume pending here (PlaybackStarted handles that).
            // ENQUEUE being discarded won't lose the track, and replayed events
            // produce the same result.
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

    alexa_response(json!({ "shouldEndSession": true }))
}

/// Playback of `event.token` has begun: adopt it as the device's current track
/// and retire whatever queued it.
async fn on_playback_started(ctx: &ReqCtx<'_>, event: &AudioEvent<'_>) -> Value {
    let state = ctx.state;
    let device_id = &ctx.device_id;

    // If an auto-continued track (loop/shuffle ENQUEUE) has started but
    // the mode was switched to "off" in the meantime, stop it now.
    // NearlyFinished often fires right after playback starts, so the
    // mode check at ENQUEUE time alone is not enough.
    // (playback_mode_is_off errs on the side of NOT stopping on Redis errors)
    if is_auto_token(event.token) && state.playback_mode_is_off().await {
        tracing::info!(
            "Stopping auto-continued track on {} (playback mode is off)",
            ctx.log_id()
        );
        return stop_directive(ctx, "idle").await;
    }
    tracing::info!("Playback started: {}", ctx.log_id());
    // Reflect the current track from the token and the start position
    // (seek-based ENQUEUE may start from a non-zero offset)
    let mut upd = DeviceUpdate::new().status("playing").position(event.offset);
    if let Some(track) = state.get_track(event.track_id).await {
        upd = upd.track(track);
    }
    state.update_device(device_id, upd).await;
    // If the started track matches a web-queued pending command, clear it.
    // Also compare offset to avoid clearing a newer seek command (different
    // offset) that arrived between directive issuance and playback start.
    if state
        .peek_pending(device_id)
        .await
        .is_some_and(|cmd| cmd.track.id == event.track_id && cmd.offset_ms == event.offset)
    {
        state.clear_pending(device_id).await;
    }
    // If the started track came from the "next up" queue, remove its
    // entry by value match (pending and auto-continuation tokens won't
    // match any queue entry). Deferring consumption to here ensures we
    // don't lose a track if ENQUEUE is discarded, and prevents double
    // consumption on event replays.
    state.remove_queue_entry(device_id, event.token).await;

    alexa_response(json!({ "shouldEndSession": true }))
}

/// The device could not play `event.token`. An ended live stream is normal
/// termination and advances like PlaybackFinished; anything else is retried a
/// few times before the device is marked as errored.
async fn on_playback_failed(ctx: &ReqCtx<'_>, event: &AudioEvent<'_>, body: &Value) -> Value {
    let state = ctx.state;
    let device_id = &ctx.device_id;
    let err = &body["request"]["error"];

    // If the failed track came from the "next up" queue, consume its
    // entry so an unplayable item (e.g., ended live stream) doesn't block
    // subsequent tracks
    state.remove_queue_entry(device_id, event.token).await;
    let track = state.get_track(event.track_id).await;

    // Live streams become unresolvable after they end, causing PlaybackFailed.
    // This is normal termination, not an error — advance to the next track
    // as with PlaybackFinished.
    if let Some(track) = track.as_ref().filter(|t| t.is_live) {
        tracing::info!(
            "Live stream '{}' ended on {} ({:?})",
            track.title,
            ctx.log_id(),
            err
        );
        mark_idle(ctx).await;
        // Avoid auto-selecting another live stream (it may also have ended,
        // causing a failure chain). Pending (explicit web commands) are tried
        // once (if it fails, pending remains and the same-track exclusion
        // stops infinite retry).
        if let Some(next) = queued_or_auto_next(ctx, event.token, false)
            .await
            .filter(|next| next.track.id != event.track_id)
        {
            return play_directive(ctx, &next).await;
        }
    } else {
        // Retry a few times for transient failures (network drops, etc.).
        // Only mark as error after exhausting retries.
        if let Some(resp) = retry_playback(ctx, body, event.token, track, err).await {
            return resp;
        }
        tracing::error!("Playback failed on {}: {:?}", ctx.log_id(), err);
        state
            .update_device(device_id, DeviceUpdate::new().status("error"))
            .await;
    }

    alexa_response(json!({ "shouldEndSession": true }))
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

// ── Helpers ──

/// Determine the next track to play from: pending command → "next up" queue →
/// playback mode auto-selection.
/// When allow_live_auto is false, auto-selected live streams are excluded
/// (pending and queue items are explicit user choices and are not filtered).
async fn queued_or_auto_next(
    ctx: &ReqCtx<'_>,
    current_token: &str,
    allow_live_auto: bool,
) -> Option<NextUp> {
    match pending_or_queue_next(ctx, current_token).await {
        Ok(Some(next)) => return Some(next),
        Ok(None) => {}
        // Cannot confirm queue state; skip auto-selection too for safety
        Err(()) => return None,
    }

    ctx.state
        .auto_next_track(token_track_id(current_token))
        .await
        .filter(|t| allow_live_auto || !t.is_live)
        .map(|track| NextUp {
            token: auto_token(&track.id),
            track,
            offset_ms: 0,
        })
}

/// Return the next candidate from pending → "next up" queue (excludes playback
/// mode auto-selection). Ok(None) if no candidate; Err if Redis error prevents
/// confirming state. Pending is not consumed here (both REPLACE_ALL and ENQUEUE
/// paths rely on PlaybackStarted to clear it).
async fn pending_or_queue_next(
    ctx: &ReqCtx<'_>,
    current_token: &str,
) -> Result<Option<NextUp>, ()> {
    if let Some(cmd) = ctx.state.peek_pending(&ctx.device_id).await
        && cmd.action == "play"
    {
        return Ok(Some(NextUp::at(cmd.track, cmd.offset_ms)));
    }

    // If the currently playing track's entry is still at the head (e.g.,
    // PlaybackStarted consumption was lost to a Redis error), remove it
    // before looking at the next entry
    while let Some((entry, track)) = ctx.state.peek_queue(&ctx.device_id).await {
        if entry == current_token {
            if !ctx.state.remove_queue_entry(&ctx.device_id, &entry).await {
                return Err(());
            }
            continue;
        }
        // Queue-sourced playback uses the entry itself as the token so
        // PlaybackStarted / PlaybackFailed can consume it by value match.
        return Ok(Some(NextUp {
            track,
            offset_ms: 0,
            token: entry,
        }));
    }
    Ok(None)
}

/// Determine the token identifying the current playback context. Prefers the
/// AudioPlayer context from the request (available even after pause/stop) and
/// falls back to the device state's current track ID.
async fn playing_context_token(ctx: &ReqCtx<'_>, body: &Value) -> String {
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

/// Response when playback cannot be switched. Requests that cannot carry speech
/// (see ReqCtx::can_speak) get an empty response instead.
fn no_track_response(can_speak: bool, text: &str) -> Value {
    if can_speak {
        speech(text, true)
    } else {
        alexa_response(json!({}))
    }
}

/// Build a retry directive for PlaybackFailed. Returns None if the track
/// cannot be resolved, the situation doesn't warrant a retry, or consecutive
/// failures exceed MAX_PLAYBACK_RETRIES (caller should mark as error).
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

    // If the failed track is an ENQUEUE'd next track (not failed_current) and
    // nothing is currently playing, don't retry — REPLACE_ALL during pause/stop
    // would unexpectedly resume playback the user had stopped
    if !failed_current && cps["playerActivity"].as_str() != Some("PLAYING") {
        return None;
    }

    // Resume position: measured position for the failed current track; for a
    // track that hadn't started yet (ENQUEUE failure), use the pending offset
    // (web seek position) to preserve seek state and prevent PlaybackStarted
    // from missing the pending
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

    // Carry forward the auto-continuation marker only if playback hasn't
    // progressed yet. Resuming a partially-played track is not a new
    // auto-continuation, so don't stop it if the mode was switched to "off"
    let retry = NextUp {
        token: if is_auto_token(token) && offset_ms == 0 {
            auto_token(&track.id)
        } else {
            new_token(&track.id)
        },
        track,
        offset_ms,
    };

    // If another track is currently playing and the ENQUEUE'd next track
    // failed, retry as ENQUEUE to avoid interrupting the current track
    if !failed_current {
        let current = cps["token"].as_str()?;
        return Some(play_response(ctx, &retry, Some(current)));
    }
    Some(play_directive(ctx, &retry).await)
}

/// Wrap a response body in the standard Alexa response envelope.
fn alexa_response(response: Value) -> Value {
    json!({ "version": "1.0", "response": response })
}

/// Update device status and return an AudioPlayer.Stop directive.
async fn stop_directive(ctx: &ReqCtx<'_>, status: &str) -> Value {
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
async fn play_directive(ctx: &ReqCtx<'_>, next: &NextUp) -> Value {
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

/// Build an AudioPlayer.Play response.
/// If enqueue_after (the preceding token) is provided, use ENQUEUE; otherwise
/// REPLACE_ALL. Device state is not updated for ENQUEUE (PlaybackStarted will
/// reflect it once playback actually starts).
fn play_response(ctx: &ReqCtx<'_>, next: &NextUp, enqueue_after: Option<&str>) -> Value {
    let NextUp {
        track,
        offset_ms,
        token,
    } = next;
    let stream_url = format!(
        "{}{}",
        ctx.base_url,
        crate::auth::stream_path(ctx.state.api_token.as_deref(), &track.id, track.is_live)
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

fn tail_chars(s: &str, n: usize) -> &str {
    let skip = s.chars().count().saturating_sub(n);
    let offset = s.char_indices().nth(skip).map_or(s.len(), |(i, _)| i);
    &s[offset..]
}

fn speech(text: &str, end_session: bool) -> Value {
    alexa_response(json!({
        "outputSpeech": {
            "type": "PlainText",
            "text": text
        },
        "shouldEndSession": end_session
    }))
}
