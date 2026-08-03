//! Running yt-dlp.
//!
//! Every invocation goes through here so the stdio setup, the process group
//! used for cancellation, and the bounded pipe draining cannot drift apart
//! between call sites.

use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;
use tokio_util::sync::CancellationToken;

/// Per-item timeout for metadata fetches to prevent yt-dlp from stalling.
const METADATA_TIMEOUT_SECS: u64 = 30;
/// Grace period for draining yt-dlp's pipes after it exits. Descendants can
/// inherit the pipes and hold them open, so the drain needs its own bound.
const OUTPUT_DRAIN_GRACE_SECS: u64 = 5;
/// Give yt-dlp and its ffmpeg descendants a short chance to exit cleanly before
/// killing the whole process group.
const PROCESS_TERMINATE_GRACE_MILLIS: u64 = 500;
/// Prefix added via --progress-template to distinguish progress lines.
pub(crate) const PROGRESS_PREFIX: &str = "__progress__ ";

/// Failure of a yt-dlp-backed operation. Cancellation is a separate variant
/// rather than a recognizable message so callers cannot mistake a user-visible
/// error for a stop request (or the reverse) by comparing strings.
#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    Failed(String),
}

impl DownloadError {
    /// Prefix a failure with context. Cancellation carries no message, so it
    /// passes through untouched.
    pub(crate) fn context(self, context: &str) -> Self {
        match self {
            Self::Cancelled => Self::Cancelled,
            Self::Failed(message) => Self::Failed(format!("{context}: {message}")),
        }
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Download cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<String> for DownloadError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for DownloadError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_string())
    }
}

pub(crate) fn snippet(s: &str) -> String {
    s.chars().take(300).collect()
}

/// Extract the percentage from a yt-dlp progress line (PROGRESS_PREFIX + " 23.4%" etc.).
/// Returns None for non-progress lines or indeterminate values ("N/A" / non-finite),
/// since f64::parse accepts "nan"/"inf" which become null in JSON.
pub(crate) fn parse_progress_percent(line: &str) -> Option<f64> {
    let rest = line.strip_prefix(PROGRESS_PREFIX)?;
    rest.trim()
        .strip_suffix('%')?
        .trim()
        .parse()
        .ok()
        .filter(|p: &f64| p.is_finite())
}

/// Run yt-dlp and return stdout on success within the timeout.
/// On failure or timeout, return an error message (including stderr snippet).
pub async fn run_yt_dlp(args: &[&str], timeout: time::Duration) -> Result<Vec<u8>, String> {
    // Without a token the run is uncancellable, so DownloadError::Cancelled is
    // unreachable here and the message form loses nothing.
    run_yt_dlp_cancellable(args, timeout, None)
        .await
        .map_err(|e| e.to_string())
}

/// As `run_yt_dlp`, but a cancelled token stops the process group and returns
/// `DownloadError::Cancelled`.
pub(crate) async fn run_yt_dlp_cancellable(
    args: &[&str],
    timeout: time::Duration,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<u8>, DownloadError> {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(DownloadError::Cancelled);
    }
    let mut child = spawn_yt_dlp(args).map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or("Failed to capture yt-dlp stdout")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Failed to capture yt-dlp stderr")?;
    let stdout_task = spawn_reader(stdout);
    let stderr_task = spawn_reader(stderr);

    enum WaitOutcome {
        Exited(std::io::Result<std::process::ExitStatus>),
        TimedOut,
        Cancelled,
    }
    let outcome = tokio::select! {
        result = time::timeout(timeout, child.wait()) => match result {
            Ok(status) => WaitOutcome::Exited(status),
            Err(_) => WaitOutcome::TimedOut,
        },
        _ = wait_for_cancellation(cancel) => WaitOutcome::Cancelled,
    };
    let status = match outcome {
        WaitOutcome::Exited(result) => {
            result.map_err(|e| format!("Failed to wait for yt-dlp: {e}"))?
        }
        // A timeout and a stop end the run the same way, and sharing the
        // teardown is what keeps them from drifting apart: kill the whole
        // process group, then abandon the pipes rather than waiting for EOF,
        // which a descendant that inherited them can delay indefinitely.
        stopped => {
            stop_yt_dlp(&mut child).await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(match stopped {
                WaitOutcome::TimedOut => "yt-dlp timed out".into(),
                _ => DownloadError::Cancelled,
            });
        }
    };
    let Some(stdout) = drain_output(stdout_task).await else {
        stderr_task.abort();
        return Err("yt-dlp output drain timed out".into());
    };
    let stderr = drain_output(stderr_task).await.unwrap_or_default();

    if !status.success() {
        return Err(DownloadError::Failed(format!(
            "yt-dlp failed: {}",
            snippet(&String::from_utf8_lossy(&stderr))
        )));
    }
    Ok(stdout)
}

/// Spawn yt-dlp with no stdin and both output streams piped. Every yt-dlp
/// invocation goes through here so the stdio setup cannot drift between them.
///
/// Its own process group is what makes cancellation reliable: yt-dlp spawns
/// ffmpeg for post-processing, and signalling the group reaches the whole tree
/// rather than leaving an orphan writing to the staging directory.
pub(crate) fn spawn_yt_dlp<I, S>(args: I) -> std::io::Result<tokio::process::Child>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("yt-dlp")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
}

async fn wait_for_cancellation(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

fn signal_yt_dlp_group(pid: u32, signal: libc::c_int) {
    // spawn_yt_dlp puts yt-dlp in a process group whose ID is its PID. A
    // negative target sends the signal to yt-dlp and descendants such as ffmpeg.
    let result = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            tracing::warn!("Failed to signal yt-dlp process group {pid}: {error}");
        }
    }
}

pub(crate) async fn stop_yt_dlp(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        signal_yt_dlp_group(pid, libc::SIGTERM);
        // Do not reap the group leader during this grace period. Keeping its PID
        // reserved prevents a recycled process group from receiving SIGKILL.
        time::sleep(time::Duration::from_millis(PROCESS_TERMINATE_GRACE_MILLIS)).await;
        signal_yt_dlp_group(pid, libc::SIGKILL);
    } else if let Err(e) = child.start_kill() {
        tracing::warn!("Failed to kill yt-dlp process: {e}");
    }
    if let Err(e) = child.wait().await {
        tracing::warn!("Failed to reap yt-dlp process: {e}");
    }
}

/// Stop draining a pipe whose output is no longer wanted. The task would
/// otherwise linger until EOF, which a surviving descendant can delay.
pub(crate) fn abort_reader(task: Option<tokio::task::JoinHandle<Vec<u8>>>) {
    if let Some(task) = task {
        task.abort();
    }
}

/// Drain a pipe into a buffer on its own task. Reading the pipes concurrently
/// is what keeps a chatty child from blocking on a full pipe.
pub(crate) fn spawn_reader<R>(mut pipe: R) -> tokio::task::JoinHandle<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf).await;
        buf
    })
}

/// Await a pipe-drain task, giving up after the grace period so a descendant
/// holding the pipe open cannot block the caller forever.
pub(crate) async fn drain_output(mut task: tokio::task::JoinHandle<Vec<u8>>) -> Option<Vec<u8>> {
    let grace = time::Duration::from_secs(OUTPUT_DRAIN_GRACE_SECS);
    match time::timeout(grace, &mut task).await {
        Ok(joined) => Some(joined.unwrap_or_default()),
        Err(_) => {
            task.abort();
            None
        }
    }
}

/// Fetch metadata JSON via yt-dlp (no download).
pub(crate) async fn fetch_metadata(
    url: &str,
    cancel: Option<&CancellationToken>,
) -> Result<Value, DownloadError> {
    let stdout = run_yt_dlp_cancellable(
        &["--dump-json", "--no-download", url],
        time::Duration::from_secs(METADATA_TIMEOUT_SECS),
        cancel,
    )
    .await
    .map_err(|e| e.context("Failed to fetch metadata"))?;

    Ok(serde_json::from_slice(&stdout).map_err(|e| format!("Failed to parse metadata: {e}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_lines() {
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX} 23.4%")),
            Some(23.4)
        );
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}100.0%")),
            Some(100.0)
        );
        // Ignore lines with indeterminate percentage or non-progress output
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}N/A")),
            None
        );
        assert_eq!(
            parse_progress_percent("[download] Destination: x.m4a"),
            None
        );
        // Reject non-finite values accepted by f64::parse (they become null in JSON)
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}nan%")),
            None
        );
        assert_eq!(
            parse_progress_percent(&format!("{PROGRESS_PREFIX}inf%")),
            None
        );
    }

    #[test]
    fn adding_context_leaves_cancellation_recognizable() {
        // Callers wrap yt-dlp failures with their own context. A stop request
        // must survive that untouched, or it gets shown to the user as an error.
        let cancelled = DownloadError::Cancelled.context("Failed to expand playlist");
        assert!(matches!(cancelled, DownloadError::Cancelled));

        let failed =
            DownloadError::Failed("no such video".to_string()).context("Failed to expand playlist");
        assert_eq!(
            failed.to_string(),
            "Failed to expand playlist: no such video"
        );
    }

    #[tokio::test]
    async fn stopping_yt_dlp_terminates_its_process_group() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();

        stop_yt_dlp(&mut child).await;

        assert!(child.try_wait().unwrap().is_some());
        let result = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
