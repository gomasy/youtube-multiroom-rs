//! The shape shared by the background jobs that visit one video at a time.
//!
//! A playlist import and a bulk metadata refresh do different work but stop
//! under the same rules, which are the subtle part: only a real Stop all ends
//! the job, a cancellation without one means that single video dropped out (its
//! track was deleted mid-fetch) and the rest still stands, and however the job
//! ends it reports how far it got.

use super::ytdlp::DownloadError;
use tokio_util::sync::CancellationToken;

/// What one visit did, once the video's own yt-dlp run came back successfully.
/// A visit that failed reports itself through the `Err` side instead.
pub(crate) enum Visited {
    /// Something landed; count it towards the job's tally.
    Done,
    /// Nothing landed, but the job goes on. The visit has already logged why.
    Skipped,
    /// Something the whole job depends on is gone (the playlist it was filling,
    /// say). Stop here rather than doing work nobody can see.
    Stop(String),
}

/// A job visiting a list of videos, each visit costing a yt-dlp run.
pub(crate) struct VideoJob {
    /// Names the job in every line it logs, e.g. `Playlist import 'Mix'`.
    label: String,
    /// Past participle for the tally, e.g. `imported` in `(3/10 imported)`.
    counted: &'static str,
    /// Why one video can drop out without the job being stopped, e.g.
    /// `deleted while downloading`.
    dropped: &'static str,
}

impl VideoJob {
    pub(crate) fn new(label: String, counted: &'static str, dropped: &'static str) -> Self {
        Self {
            label,
            counted,
            dropped,
        }
    }

    /// Visit each video in turn until the list runs out or the job is stopped,
    /// returning the tally however it ended. `visit` owns the work and its own
    /// success logging; what it reports — a per-video failure, a reason to stop
    /// early — is folded into the tally and log lines here.
    ///
    /// The ID is handed over owned rather than lent: an `AsyncFnMut` borrowing
    /// it makes the returned future higher-ranked, which stops `tokio::spawn`
    /// from proving the job `Send`.
    ///
    /// Both callers spawn this and ignore the tally — it is already logged —
    /// but returning it is what lets the stopping rules be tested.
    pub(crate) async fn run<F, Fut>(
        self,
        video_ids: &[String],
        cancel: &CancellationToken,
        mut visit: F,
    ) -> usize
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<Visited, DownloadError>>,
    {
        let Self {
            label,
            counted,
            dropped,
        } = self;
        let total = video_ids.len();
        let mut done = 0usize;

        for video_id in video_ids {
            // Checked before the visit as well as after it: a stop that lands
            // between two videos must not buy the next one a yt-dlp run.
            if cancel.is_cancelled() {
                tracing::info!("{label} cancelled ({done}/{total} {counted})");
                return done;
            }
            match visit(video_id.clone()).await {
                Ok(Visited::Done) => done += 1,
                Ok(Visited::Skipped) => {}
                Ok(Visited::Stop(reason)) => {
                    tracing::info!("{label} stopped: {reason} ({done}/{total} {counted})");
                    return done;
                }
                // Only a real Stop all ends the job.
                Err(DownloadError::Cancelled) if cancel.is_cancelled() => {
                    tracing::info!("{label} cancelled ({done}/{total} {counted})");
                    return done;
                }
                Err(DownloadError::Cancelled) => {
                    tracing::info!("{label}: skipping {video_id}, {dropped}");
                }
                // Failures already reach the user through the download progress
                // error display, so they are only logged here.
                Err(e) => tracing::warn!("{label}: skipping {video_id}: {e}"),
            }
        }
        tracing::info!("{label} finished ({done}/{total} {counted})");
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("video{i}")).collect()
    }

    fn job() -> VideoJob {
        VideoJob::new("Test job".to_string(), "done", "deleted while fetching")
    }

    /// Run the job, recording which videos it actually reached.
    async fn run_recording<F, Fut>(
        video_ids: &[String],
        cancel: &CancellationToken,
        mut visit: F,
    ) -> (usize, Vec<String>)
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<Visited, DownloadError>>,
    {
        let seen = RefCell::new(Vec::new());
        let done = job()
            .run(video_ids, cancel, |id| {
                seen.borrow_mut().push(id.clone());
                visit(id)
            })
            .await;
        (done, seen.into_inner())
    }

    #[tokio::test]
    async fn visits_every_video_when_nothing_interrupts() {
        let (done, seen) = run_recording(&ids(3), &CancellationToken::new(), |_| async {
            Ok(Visited::Done)
        })
        .await;
        assert_eq!(done, 3);
        assert_eq!(seen.len(), 3);
    }

    #[tokio::test]
    async fn a_video_dropping_out_does_not_end_the_job() {
        // The rule the two callers depend on: a cancellation with the token
        // still live means that one video was deleted mid-fetch. The rest of
        // the job must still run, and must not count the one that dropped.
        let cancel = CancellationToken::new();
        let (done, seen) = run_recording(&ids(3), &cancel, |id| async move {
            if id == "video1" {
                Err(DownloadError::Cancelled)
            } else {
                Ok(Visited::Done)
            }
        })
        .await;
        assert_eq!(done, 2);
        assert_eq!(seen, ids(3));
    }

    #[tokio::test]
    async fn a_failure_does_not_end_the_job_either() {
        let (done, seen) = run_recording(&ids(3), &CancellationToken::new(), |id| async move {
            if id == "video0" {
                Err(DownloadError::Failed("no such video".to_string()))
            } else {
                Ok(Visited::Done)
            }
        })
        .await;
        assert_eq!(done, 2);
        assert_eq!(seen, ids(3));
    }

    #[tokio::test]
    async fn a_real_stop_ends_the_job_at_once() {
        // Stop all cancels the shared token, so the in-flight video reports
        // Cancelled *and* the token reads cancelled. Nothing after it is
        // fetched — that is what makes Stop all prompt.
        let cancel = CancellationToken::new();
        let (done, seen) = run_recording(&ids(4), &cancel, |id| {
            let cancel = cancel.clone();
            async move {
                if id == "video1" {
                    cancel.cancel();
                    return Err(DownloadError::Cancelled);
                }
                Ok(Visited::Done)
            }
        })
        .await;
        assert_eq!(done, 1);
        assert_eq!(seen, ids(2));
    }

    #[tokio::test]
    async fn a_stop_between_videos_buys_the_next_one_no_fetch() {
        // A visit that completes normally and only then gets stopped must still
        // not let the loop start the following yt-dlp run.
        let cancel = CancellationToken::new();
        let (done, seen) = run_recording(&ids(4), &cancel, |id| {
            let cancel = cancel.clone();
            async move {
                if id == "video0" {
                    cancel.cancel();
                }
                Ok(Visited::Done)
            }
        })
        .await;
        assert_eq!(done, 1);
        assert_eq!(seen, ids(1));
    }

    #[tokio::test]
    async fn losing_what_the_job_fills_stops_it() {
        // The playlist being imported into was deleted: there is nowhere left
        // to put the remaining videos, so none of them are fetched.
        let (done, seen) = run_recording(&ids(4), &CancellationToken::new(), |id| async move {
            if id == "video2" {
                Ok(Visited::Stop("the playlist was deleted".to_string()))
            } else {
                Ok(Visited::Done)
            }
        })
        .await;
        assert_eq!(done, 2);
        assert_eq!(seen, ids(3));
    }

    #[tokio::test]
    async fn a_job_cancelled_before_it_starts_fetches_nothing() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (done, seen) = run_recording(&ids(3), &cancel, |_| async { Ok(Visited::Done) }).await;
        assert_eq!(done, 0);
        assert!(seen.is_empty());
    }
}
