//! CDN sources for the live and streamed relays. Resolving takes seconds, so a
//! streamed track's URL is kept until shortly before it expires.

use super::url::watch_url;
use super::ytdlp::run_yt_dlp;
use super::{AppState, since_epoch};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// An Echo cannot wait much longer than this for the response.
const RESOLVE_TIMEOUT_SECS: u64 = 15;
/// Stop handing out a URL this long before its `expire`.
const EXPIRY_MARGIN_SECS: u64 = 30 * 60;
/// Lifetime of a URL that names no expiry.
const FALLBACK_LIFETIME_SECS: u64 = 60 * 60;
const MAX_LIFETIME_SECS: u64 = 5 * 60 * 60;
/// No fresh URL can outlive more than this, so a kept one is never asked to.
const MAX_NEEDED_SECS: u64 = MAX_LIFETIME_SECS - 10 * 60;
/// AAC first, so the relay copies rather than transcodes. Never a merge, so
/// `url` names exactly one stream.
const STREAMED_FORMAT: &str = "bestaudio[acodec^=mp4a]/bestaudio";
/// Live streams often lack an audio-only HLS format; the fallback is the
/// lowest-bitrate muxed one.
const LIVE_FORMAT: &str = "bestaudio[protocol^=m3u8]/worst[protocol^=m3u8]/bestaudio/worst";

#[derive(Clone, Debug)]
pub struct RelaySource {
    pub url: String,
    /// Headers yt-dlp requires with `url`, as one CRLF-joined string for
    /// ffmpeg's `-headers`.
    pub headers: String,
    pub acodec: String,
}

pub(super) struct CachedSource {
    source: RelaySource,
    stale_at: Instant,
}

/// Locked while resolving, so devices playing one track share a yt-dlp run.
pub(super) type SourceSlot = Arc<Mutex<Option<CachedSource>>>;

impl AppState {
    /// A streamed video's source: the kept one if it outlives `remaining`, else
    /// a fresh resolve.
    pub async fn relay_source(
        &self,
        video_id: &str,
        remaining: Duration,
    ) -> Result<RelaySource, String> {
        let slot = {
            let mut slots = self.relay_sources.lock().await;
            let now = Instant::now();
            // Prune stale entries nobody is using.
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || slot
                        .try_lock()
                        .is_ok_and(|c| c.as_ref().is_some_and(|c| c.stale_at > now))
            });
            slots.entry(video_id.to_string()).or_default().clone()
        };

        let mut cached = slot.lock().await;
        // `stale_at` already sits EXPIRY_MARGIN before the expiry.
        let needed = remaining
            .saturating_sub(Duration::from_secs(EXPIRY_MARGIN_SECS))
            .min(Duration::from_secs(MAX_NEEDED_SECS));
        if let Some(kept) = cached
            .as_ref()
            .filter(|c| c.stale_at > Instant::now() + needed)
        {
            return Ok(kept.source.clone());
        }

        let source = resolve_source(video_id, STREAMED_FORMAT).await?;
        let stale_at = Instant::now() + source_lifetime(&source.url, since_epoch().as_secs());
        *cached = Some(CachedSource {
            source: source.clone(),
            stale_at,
        });
        Ok(source)
    }

    /// Drop the kept source after a relay from `url` failed, unless it has
    /// been replaced since.
    pub async fn forget_relay_source(&self, video_id: &str, url: &str) {
        let Some(slot) = self.relay_sources.lock().await.get(video_id).cloned() else {
            return;
        };
        let mut cached = slot.lock().await;
        if cached.as_ref().is_some_and(|c| c.source.url == url) {
            *cached = None;
        }
    }
}

/// A live stream's source. Never kept: an HLS manifest's `expire` means less.
pub async fn live_source(video_id: &str) -> Result<RelaySource, String> {
    resolve_source(video_id, LIVE_FORMAT).await
}

async fn resolve_source(video_id: &str, format: &str) -> Result<RelaySource, String> {
    let stdout = run_yt_dlp(
        &[
            "-f",
            format,
            "--print",
            "%(.{url,acodec,http_headers})j",
            "--no-playlist",
            &watch_url(video_id),
        ],
        Duration::from_secs(RESOLVE_TIMEOUT_SECS),
    )
    .await
    .map_err(|e| format!("Failed to resolve stream URL: {e}"))?;
    parse_source(&String::from_utf8_lossy(&stdout))
}

/// Read yt-dlp's printed `{url, acodec, http_headers}` object.
fn parse_source(stdout: &str) -> Result<RelaySource, String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with('{'))
        .ok_or("yt-dlp printed no stream info")?;
    let v: Value =
        serde_json::from_str(line).map_err(|e| format!("Failed to parse stream info: {e}"))?;
    let url = v["url"]
        .as_str()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .ok_or("yt-dlp returned no usable stream URL")?
        .to_string();
    let acodec = v["acodec"].as_str().unwrap_or("").to_string();
    // Anything that could break out of its header line is dropped.
    let headers = v["http_headers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k, v.as_str()?)))
        .filter(|(k, v)| !k.contains([':', '\r', '\n']) && !v.contains(['\r', '\n']))
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    Ok(RelaySource {
        url,
        headers,
        acodec,
    })
}

/// Until EXPIRY_MARGIN before the URL's `expire=<unix secs>`, bounded either way.
fn source_lifetime(url: &str, now: u64) -> Duration {
    let expire = url::Url::parse(url).ok().and_then(|u| {
        u.query_pairs()
            .find(|(k, _)| k == "expire")
            .and_then(|(_, v)| v.parse::<u64>().ok())
    });
    let secs = match expire {
        Some(expire) => expire
            .saturating_sub(now)
            .saturating_sub(EXPIRY_MARGIN_SECS),
        None => FALLBACK_LIFETIME_SECS,
    };
    Duration::from_secs(secs.min(MAX_LIFETIME_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_printed_stream_info() {
        let out = "WARNING: something\n{\"url\": \"https://rr1.googlevideo.com/videoplayback?expire=1\", \
                   \"acodec\": \"mp4a.40.2\", \"http_headers\": {\"User-Agent\": \"UA\", \
                   \"Bad\": \"a\\r\\nInjected: b\", \"Num\": 1}}\n";
        let source = parse_source(out).unwrap();
        assert_eq!(
            source.url,
            "https://rr1.googlevideo.com/videoplayback?expire=1"
        );
        assert_eq!(source.acodec, "mp4a.40.2");
        // A value that would smuggle a header of its own is dropped, as is one
        // that is not a string
        assert_eq!(source.headers, "User-Agent: UA\r\n");
    }

    #[test]
    fn rejects_output_without_a_url() {
        assert!(parse_source("").is_err());
        assert!(parse_source("{\"url\": null, \"acodec\": \"opus\"}").is_err());
        assert!(parse_source("{\"url\": \"file:///etc/passwd\"}").is_err());
    }

    #[test]
    fn a_source_is_kept_until_shortly_before_it_expires() {
        let now = 1_000_000;
        let url =
            |expire: u64| format!("https://x.googlevideo.com/videoplayback?a=1&expire={expire}");
        // Six hours out, as googlevideo issues them: capped
        assert_eq!(
            source_lifetime(&url(now + 6 * 3600), now),
            Duration::from_secs(MAX_LIFETIME_SECS)
        );
        assert_eq!(
            source_lifetime(&url(now + 3600), now),
            Duration::from_secs(3600 - EXPIRY_MARGIN_SECS)
        );
        // Inside the margin, or already gone: not kept at all
        assert_eq!(source_lifetime(&url(now + 60), now), Duration::ZERO);
        assert_eq!(source_lifetime(&url(now - 60), now), Duration::ZERO);
        // No expiry to go by
        assert_eq!(
            source_lifetime("https://example.com/a.m4a", now),
            Duration::from_secs(FALLBACK_LIFETIME_SECS)
        );
    }
}
