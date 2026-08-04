//! Rebuilding a downloaded file's container.
//!
//! yt-dlp downloads a DASH-sourced m4a as fragments and, seeing a file already
//! in the target format, skips the conversion that would have merged them. The
//! result is a header indexing none of the audio, followed by fragments each
//! describing their own samples. ffmpeg walks all of them, so the download and
//! the browser preview see the whole track — but an Echo takes the header at
//! its word and reports a multi-hour live archive as nearly finished minutes
//! in. yt-dlp only fixes this for formats it knows are fragmented, which a
//! finished live stream is not by the time it is downloaded.

use super::ytdlp::{DownloadError, abort_reader, spawn_reader, stderr_snippet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// How far two measurements of the same file may sit apart before the
/// difference means audio was lost. Priming and a trailing partial frame move a
/// length by milliseconds.
const DURATION_TOLERANCE_SECS: f64 = 5.0;

/// How much of the video ffmpeg has to be able to read before the download
/// counts as whole. A fraction, not a margin in seconds, because one of the two
/// lengths comes from YouTube rather than the file: only a gross disagreement
/// is meaningful.
const MIN_READABLE_FRACTION: f64 = 0.9;

/// Rewrite `path`'s container in place, leaving the encoded audio untouched.
///
/// Best effort: a file that cannot be rewritten, or that comes back holding
/// less audio than it went in with, is left exactly as it was downloaded.
/// `reported_secs` is YouTube's duration for the video; zero means unknown.
pub(crate) async fn rebuild_container(path: &Path, reported_secs: u64, cancel: &CancellationToken) {
    let Some(rebuilt) = rebuilt_path(path) else {
        tracing::warn!("Skipping container rebuild: bad path {}", path.display());
        return;
    };
    // Every failure path leaves the rebuild for this one line to clear away.
    if !rebuild_into_place(path, &rebuilt, reported_secs, cancel).await {
        let _ = tokio::fs::remove_file(&rebuilt).await;
    }
}

/// Rebuild `path` by way of `rebuilt`, reporting whether `rebuilt` ended up
/// installed as `path`.
async fn rebuild_into_place(
    path: &Path,
    rebuilt: &Path,
    reported_secs: u64,
    cancel: &CancellationToken,
) -> bool {
    let Some(readable) = readable_duration(path).await else {
        tracing::warn!("Skipping container rebuild: cannot read {}", path.display());
        return false;
    };
    if !download_is_whole(readable, reported_secs) {
        tracing::warn!(
            "Skipping container rebuild: only {readable:.0}s of the {reported_secs}s YouTube \
             reports is readable: {}",
            path.display()
        );
        return false;
    }

    match stream_copy(path, rebuilt, cancel).await {
        Ok(()) => {}
        // A stop tears the whole staging directory down next.
        Err(DownloadError::Cancelled) => return false,
        Err(e) => {
            tracing::warn!("Container rebuild failed for {}: {e}", path.display());
            return false;
        }
    }

    // A stream copy either carries over everything it read or fails, so this
    // check is not expected to fire.
    let rebuilt_secs = readable_duration(rebuilt).await;
    if !rebuild_keeps_all_audio(readable, rebuilt_secs) {
        tracing::warn!(
            "Discarding rebuilt container: {} of the downloaded {readable:.0}s: {}",
            rebuilt_secs.map_or_else(|| "nothing readable".to_string(), |s| format!("{s:.0}s")),
            path.display()
        );
        return false;
    }

    if let Err(e) = tokio::fs::rename(rebuilt, path).await {
        tracing::warn!(
            "Failed to install rebuilt container {}: {e}",
            path.display()
        );
        return false;
    }
    true
}

/// Whether the audio ffmpeg can read out of the download accounts for the whole
/// video. When it does not, ffmpeg is stopping short of the end of the file and
/// a rebuild would keep only the part it read. Zero means YouTube gave no
/// duration to check against.
fn download_is_whole(readable: f64, reported_secs: u64) -> bool {
    reported_secs == 0 || readable >= reported_secs as f64 * MIN_READABLE_FRACTION
}

/// Whether a rebuild came out holding everything the download did. An
/// unreadable rebuild counts as holding nothing.
fn rebuild_keeps_all_audio(readable: f64, rebuilt_secs: Option<f64>) -> bool {
    rebuilt_secs.is_some_and(|secs| secs >= readable - DURATION_TOLERANCE_SECS)
}

/// Where the rebuild writes before it replaces the original. Prefixing rather
/// than extending the name keeps the extension ffmpeg picks its muxer from.
fn rebuilt_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    Some(path.with_file_name(format!("rebuilt-{name}")))
}

/// Copy `input`'s audio into a freshly written container at `output`.
async fn stream_copy(
    input: &Path,
    output: &Path,
    cancel: &CancellationToken,
) -> Result<(), DownloadError> {
    if cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        // The AAC frames come out byte for byte; only the boxes describing them
        // are written anew. +faststart puts those boxes at the front, so an Echo
        // can start playing from its first range request.
        .args(["-map", "0:a:0", "-c:a", "copy", "-movflags", "+faststart"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;

    // Drained on its own task so a chatty ffmpeg cannot block on a full pipe
    // while we wait for it to exit.
    let stderr = child.stderr.take().map(spawn_reader);
    let status = tokio::select! {
        result = child.wait() => result.map_err(|e| format!("Failed to wait for ffmpeg: {e}"))?,
        _ = cancel.cancelled() => {
            // Reaped here rather than left to kill_on_drop so that ffmpeg is
            // gone before the caller deletes the file it was writing.
            if let Err(e) = child.start_kill() {
                tracing::warn!("Failed to kill ffmpeg process: {e}");
            }
            if let Err(e) = child.wait().await {
                tracing::warn!("Failed to reap ffmpeg process: {e}");
            }
            abort_reader(stderr);
            return Err(DownloadError::Cancelled);
        }
    };
    if !status.success() {
        return Err(DownloadError::Failed(format!(
            "ffmpeg failed: {}",
            stderr_snippet(stderr).await
        )));
    }
    Ok(())
}

/// How many seconds of audio ffmpeg can read out of `path` — deliberately not
/// what the header claims, since the demuxer walks every fragment to answer
/// this. `None` when ffprobe reports nothing usable, which callers treat as a
/// file to leave alone rather than as a length.
async fn readable_duration(path: &Path) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // "N/A" for a file that yields no duration, and parse accepts "inf"/"nan",
    // so the value is checked as well as parsed.
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|d| d.is_finite() && *d > 0.0)
}

#[cfg(test)]
mod tests {
    use super::{download_is_whole, rebuild_keeps_all_audio, rebuilt_path};
    use std::path::Path;

    #[test]
    fn rebuild_writes_beside_the_original_keeping_its_extension() {
        assert_eq!(
            rebuilt_path(Path::new("/cache/.downloads/a1/dQw4w9WgXcQ.m4a")),
            Some(Path::new("/cache/.downloads/a1/rebuilt-dQw4w9WgXcQ.m4a").to_path_buf())
        );
        // A path naming no file has nothing to write beside.
        assert_eq!(rebuilt_path(Path::new("/")), None);
    }

    #[test]
    fn a_download_ffmpeg_reads_short_of_is_left_alone() {
        // Encoder priming and a trailing partial frame are not a short read,
        // and neither is a duration YouTube rounds off
        assert!(download_is_whole(600.02, 600));
        assert!(download_is_whole(599.8, 600));
        assert!(download_is_whole(588.0, 600));
        // Reading one fragment of a long archive is
        assert!(!download_is_whole(10.0, 600));
        // Nothing to check against: the remaining safety check has to carry it
        assert!(download_is_whole(10.0, 0));
    }

    #[test]
    fn a_rebuild_is_kept_only_while_it_holds_what_the_download_did() {
        assert!(rebuild_keeps_all_audio(600.0, Some(600.02)));
        assert!(rebuild_keeps_all_audio(600.0, Some(599.8)));
        // Longer is not a loss: an edit list the rebuild drops adds priming back
        assert!(rebuild_keeps_all_audio(600.0, Some(700.0)));
        assert!(!rebuild_keeps_all_audio(600.0, Some(10.0)));
        // A rebuild nothing can read holds nothing
        assert!(!rebuild_keeps_all_audio(600.0, None));
    }
}
