//! Serving audio bytes: the cached file, the live CDN relay, and the signed URL
//! the browser needs to play either of them.

use super::{AppError, AppResult, track_or_404};
use crate::state::{AUDIO_MIME, AppState, run_yt_dlp};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

/// GET /api/audio/:id/stream
///
/// Streams the file with Range support (no full in-memory read). Echo devices
/// issue repeated Range requests during playback.
pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let track = track_or_404(&state, &audio_id).await?;

    let mut file = fs::File::open(&track.file_path)
        .await
        .map_err(|e| AppError::not_found(format!("Failed to open file: {e}")))?;
    let total = file
        .metadata()
        .await
        .map_err(|e| AppError::internal(format!("Failed to stat file: {e}")))?
        .len() as usize;

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| parse_byte_range(r, total));

    let mut resp = Response::builder()
        .header(header::CONTENT_TYPE, AUDIO_MIME)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, max-age=3600");

    let body = if let Some((start, end)) = range {
        file.seek(SeekFrom::Start(start as u64))
            .await
            .map_err(|e| AppError::internal(format!("Failed to seek: {e}")))?;
        let len = end - start + 1;
        resp = resp
            .status(StatusCode::PARTIAL_CONTENT)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )
            .header(header::CONTENT_LENGTH, len);
        Body::from_stream(ReaderStream::new(file.take(len as u64)))
    } else {
        resp = resp.header(header::CONTENT_LENGTH, total);
        Body::from_stream(ReaderStream::new(file))
    };

    resp.body(body)
        .map_err(|e| AppError::internal(format!("Failed to build response: {e}")))
}

/// GET /api/audio/:id/live
///
/// Live streams cannot be saved as files, so we resolve the CDN HLS URL via
/// yt-dlp on each request, then relay audio-only (AAC) through ffmpeg as an
/// ADTS stream. Echo devices cannot play muxed HLS with video, so server-side
/// audio extraction is required. Audio is codec-copied (no re-encoding) for
/// minimal CPU overhead.
pub async fn live_audio(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Response> {
    let track = track_or_404(&state, &audio_id).await?;

    if !track.is_live {
        return Err(AppError::bad_request("Track is not a live stream"));
    }

    let url = format!("https://www.youtube.com/watch?v={audio_id}");
    // Prefer HLS which ffmpeg handles well. Live streams often lack audio-only
    // formats, so fall back to the lowest-bitrate muxed HLS (video+audio).
    // Also fetch acodec to decide whether re-encoding is needed.
    // Use a short timeout since Echo devices can't wait long.
    let stdout = run_yt_dlp(
        &[
            "--print",
            "urls",
            "--print",
            "acodec",
            "-f",
            "bestaudio[protocol^=m3u8]/worst[protocol^=m3u8]/bestaudio/worst",
            "--no-playlist",
            &url,
        ],
        std::time::Duration::from_secs(15),
    )
    .await
    .map_err(|e| AppError::internal(format!("Failed to get live stream URL: {e}")))?;

    // Output is URL (may be multiple lines for DASH) then acodec. Use only the first URL.
    let stdout_str = String::from_utf8_lossy(&stdout);
    let lines: Vec<&str> = stdout_str
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // At least one URL line plus the acodec line; extra URL lines are ignored
    let [cdn_url, .., acodec] = lines.as_slice() else {
        return Err(AppError::internal(
            "yt-dlp returned no usable stream URL and codec",
        ));
    };

    // AAC can be remuxed as-is; other codecs (Opus, etc.) need transcoding
    // since ADTS only supports AAC. "unknown" acodec (common in muxed HLS)
    // also triggers transcoding to be safe.
    let codec_args: &[&str] = if acodec.starts_with("mp4a") || acodec.starts_with("aac") {
        &["-c:a", "copy"]
    } else {
        tracing::info!("Live audio codec '{acodec}' is not AAC, transcoding");
        &["-c:a", "aac", "-b:a", "128k"]
    };

    let mut child = tokio::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i", cdn_url, "-vn"])
        .args(codec_args)
        .args(["-f", "adts", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::internal(format!("Failed to run ffmpeg: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::internal("Failed to capture ffmpeg stdout"))?;
    let stderr = child.stderr.take();

    // When Echo disconnects, the response body and stdout pipe close, causing
    // ffmpeg to exit naturally via EPIPE. Reap the child to prevent zombies
    // and log any stderr for debugging.
    tokio::spawn(async move {
        let mut err_buf = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut err_buf).await;
        }
        let err = err_buf.trim();
        match child.wait().await {
            Ok(status) if err.is_empty() => {
                tracing::info!("ffmpeg exited: {status}")
            }
            Ok(status) => tracing::warn!("ffmpeg exited: {status}: {err}"),
            Err(e) => tracing::warn!("ffmpeg wait error: {e}"),
        }
    });

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "audio/aac".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(ReaderStream::new(stdout)),
    )
        .into_response())
}

/// GET /api/audio/:id/url
///
/// Returns a stream URL (signed relative path when auth is enabled) for
/// browser preview playback via the audio element. This endpoint itself
/// requires Bearer auth, so third parties cannot mint signed URLs.
pub async fn audio_url(
    State(state): State<Arc<AppState>>,
    Path(audio_id): Path<String>,
) -> AppResult<Json<Value>> {
    let track = track_or_404(&state, &audio_id).await?;
    let url = crate::auth::stream_path(state.api_token.as_deref(), &track.id, track.is_live);
    Ok(Json(json!({ "url": url })))
}

fn parse_byte_range(header: &str, total: usize) -> Option<(usize, usize)> {
    if total == 0 {
        return None;
    }
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;
    let (start, end) = if start_str.is_empty() {
        // Suffix range (bytes=-N): last N bytes
        let suffix_len: usize = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        (total.saturating_sub(suffix_len), total - 1)
    } else {
        let start = start_str.parse().ok()?;
        let end = if end_str.is_empty() {
            total - 1
        } else {
            end_str.parse::<usize>().ok()?.min(total - 1)
        };
        (start, end)
    };
    if start <= end && start < total {
        Some((start, end))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::parse_byte_range;

    #[test]
    fn parses_byte_ranges() {
        // Normal range, clamped end, start-only
        assert_eq!(parse_byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_byte_range("bytes=900-1999", 1000), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=500-", 1000), Some((500, 999)));
        // Suffix range: last N bytes
        assert_eq!(parse_byte_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_byte_range("bytes=-2000", 1000), Some((0, 999)));
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert_eq!(parse_byte_range("bytes=-0", 1000), None);
        assert_eq!(parse_byte_range("bytes=1000-", 1000), None);
        assert_eq!(parse_byte_range("bytes=5-2", 1000), None);
        assert_eq!(parse_byte_range("bytes=0-99", 0), None);
        assert_eq!(parse_byte_range("items=0-99", 1000), None);
        // Multi-range not supported (caller falls back to 200 full)
        assert_eq!(parse_byte_range("bytes=0-1,5-6", 1000), None);
    }
}
