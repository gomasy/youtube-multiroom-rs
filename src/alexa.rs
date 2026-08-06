use crate::state::{
    AppState, AudioTrack, DeviceUpdate, auto_token, is_auto_token, new_token, token_track_id,
};
use rust_i18n::t;
use serde_json::{Value, json};
use std::sync::Arc;

/// Maximum retries for the same track on PlaybackFailed before marking as error.
const MAX_PLAYBACK_RETRIES: u32 = 3;

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

/// A track chosen to play next, with everything its directive needs.
///
/// The token is not derivable from the track, which is why it travels along:
/// a queue-sourced play reuses the queue entry so PlaybackStarted can consume
/// it by value, and auto-continuation carries a marker PlaybackStarted checks
/// the playback mode against.
struct NextUp {
    track: AudioTrack,
    offset_ms: u64,
    token: String,
}

impl NextUp {
    /// A play from `offset_ms` under a freshly minted token: every path that is
    /// neither consuming a queue entry nor auto-continuing.
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

    /// A play of a queue entry, keyed by the entry itself so PlaybackStarted
    /// and PlaybackFailed can consume it by value match.
    fn from_queue(entry: String, track: AudioTrack) -> Self {
        Self {
            track,
            offset_ms: 0,
            token: entry,
        }
    }

    /// A play the playback mode chose, marked so PlaybackStarted can stop it if
    /// the mode was switched off after the ENQUEUE went out.
    fn auto(track: AudioTrack) -> Self {
        Self {
            token: auto_token(&track.id),
            track,
            offset_ms: 0,
        }
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
/// the device was part-way through. None when there is nothing to play.
///
/// LaunchRequest and PlayFromWebIntent share this because Alexa routes
/// skill-opening phrases to either one; only the empty-handed reply differs.
async fn start_or_resume(ctx: &ReqCtx<'_>) -> Option<Value> {
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
/// The counterpart to [`pending_or_queue_next`], which only *chooses* the next
/// track for a skip or an auto-continuation. This one starts playback as the
/// response to a launch or resume, so it consumes pending rather than peeking
/// at it and leaves the queue alone while a track is in progress (a seek
/// reload would otherwise discard one).
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

// ── Intents ──

async fn on_intent(ctx: &ReqCtx<'_>, body: &Value) -> Value {
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

        "AMAZON.HelpIntent" => speech(&t!("alexa_help", locale = locale), false),

        _ => speech(&t!("alexa_use_web", locale = locale), false),
    }
}

/// Play the library track a spoken phrase names, on the device that heard it.
///
/// Searched over the whole library rather than the playback scope: naming
/// something is how a listener escapes whatever scope the web UI was left on.
/// Nothing close enough plays nothing at all — an arbitrary track is a worse
/// answer than saying so.
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

    // A track the user just asked for by name gets a clean slate, the way an
    // explicit resume does.
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    let title = track.title.clone();
    let resp = play_directive(ctx, &NextUp::fresh(track)).await;
    with_speech(resp, &t!("alexa_playing", locale = locale, title = title))
}

/// Switch the selection scope to the playlist a spoken phrase names and start
/// it from the top.
///
/// The scope moves with it: having asked for a playlist by name, what follows
/// the first track should come from that playlist and not from whatever the web
/// UI was last set to.
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
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    let resp = play_directive(ctx, &NextUp::fresh(track)).await;
    with_speech(
        resp,
        &t!(
            "alexa_playing_playlist",
            locale = locale,
            name = playlist.name
        ),
    )
}

/// What Alexa filled a slot with, trimmed. Empty when it heard nothing usable,
/// which is a case every caller has to answer for itself.
fn slot_value<'a>(body: &'a Value, name: &str) -> &'a str {
    body["request"]["intent"]["slots"][name]["value"]
        .as_str()
        .unwrap_or("")
        .trim()
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
    // Carrying on is a fresh chance for the track, so the failure counter is
    // cleared — otherwise one left in the error state errors again at once.
    ctx.state.clear_playback_failures(&ctx.device_id).await;
    play_directive(ctx, &NextUp::at(track, offset_ms)).await
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

// ── Helpers ──

/// The next track: pending command → "next up" queue → playback-mode
/// auto-selection. With `allow_live_auto` false, only the auto-selected track
/// is filtered for liveness — pending and queue items are explicit choices.
async fn queued_or_auto_next(
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
async fn pending_or_queue_next(
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

/// Wrap a response body in the standard Alexa response envelope.
fn alexa_response(response: Value) -> Value {
    json!({ "version": "1.0", "response": response })
}

/// The bare acknowledgement an event handler returns when it has no directive
/// to send.
fn end_session() -> Value {
    alexa_response(json!({ "shouldEndSession": true }))
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

/// Build an AudioPlayer.Play response: ENQUEUE after `enqueue_after`, or
/// REPLACE_ALL without it. An ENQUEUE leaves the device state alone —
/// PlaybackStarted reflects it once playback actually begins.
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

/// Add spoken confirmation to a response that already carries a directive. An
/// AudioPlayer.Play may be announced — Alexa speaks first and starts the audio
/// after — which is what lets "playing X" answer a request that names X.
fn with_speech(mut resp: Value, text: &str) -> Value {
    resp["response"]["outputSpeech"] = json!({ "type": "PlainText", "text": text });
    resp
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
