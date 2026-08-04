//! Recognizing YouTube URLs. Host and ID validation live here so a
//! user-supplied URL is never handed to yt-dlp as-is.

/// Classified input URL type (video / playlist / unknown).
pub enum UrlKind {
    Video,
    Playlist(String),
    Unknown,
}

/// The canonical watch URL for a video ID. Every yt-dlp invocation naming a
/// single video builds its URL here rather than forwarding what the user typed:
/// the ID has been validated by [`extract_video_id`] or [`is_video_id`], so
/// rebuilding the URL keeps a deceptive host out of yt-dlp's hands.
pub(crate) fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// Classify a URL as video, playlist, or unknown. URLs containing both a video
/// ID and a playlist ID (e.g. a watch URL during playlist playback) are treated
/// as video for backward compatibility.
pub fn classify_url(url: &str) -> UrlKind {
    if extract_video_id(url).is_some() {
        UrlKind::Video
    } else if let Some(list_id) = extract_playlist_id(url) {
        UrlKind::Playlist(list_id)
    } else {
        UrlKind::Unknown
    }
}

/// Parse an HTTP(S) URL on one of the supported YouTube hosts. This is the
/// authoritative host check; isYoutubeUrl() in front/src/components/UrlInput.tsx
/// mirrors the host list for UI purposes only, so keep the two in sync.
fn parse_youtube_url(value: &str) -> Option<url::Url> {
    let parsed = url::Url::parse(value)
        .or_else(|_| url::Url::parse(&format!("https://{value}")))
        .ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    if matches!(
        host,
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    ) {
        Some(parsed)
    } else {
        None
    }
}

/// Extract a YouTube playlist ID from the list= parameter. Whether the list can
/// be expanded is left to yt-dlp, so auth-gated lists such as WL remain valid.
fn extract_playlist_id(url: &str) -> Option<String> {
    let parsed = parse_youtube_url(url)?;
    if parsed.host_str()? == "youtu.be" {
        return None;
    }
    parsed
        .query_pairs()
        .find(|(key, value)| key == "list" && is_playlist_id(value))
        .map(|(_, value)| value.into_owned())
}

/// Character set shared by YouTube video and playlist IDs.
fn is_id_chars(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Check whether a string matches the YouTube video ID format.
pub(crate) fn is_video_id(s: &str) -> bool {
    s.len() == 11 && is_id_chars(s)
}

pub(crate) fn extract_video_id(url: &str) -> Option<String> {
    let parsed = parse_youtube_url(url)?;
    let host = parsed.host_str()?;
    let candidate = if host == "youtu.be" {
        parsed.path_segments()?.next()
    } else if parsed.path() == "/watch" {
        return parsed
            .query_pairs()
            .find(|(key, value)| key == "v" && is_video_id(value))
            .map(|(_, value)| value.into_owned());
    } else {
        let mut segments = parsed.path_segments()?;
        match (segments.next(), segments.next()) {
            (Some("embed" | "shorts" | "live"), id) => id,
            _ => None,
        }
    }?;

    if is_video_id(candidate) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn is_playlist_id(value: &str) -> bool {
    value.len() >= 2 && is_id_chars(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_id_from_url_variants() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "www.youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(
                extract_video_id(url).as_deref(),
                Some("dQw4w9WgXcQ"),
                "failed for {url}"
            );
        }
        assert_eq!(extract_video_id("https://example.com/watch?v=x"), None);
        assert_eq!(
            extract_video_id("http://127.0.0.1/youtube.com/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(
            extract_video_id("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(
            extract_video_id("HTTPS://WWW.YOUTUBE.COM/watch?feature=share&v=dQw4w9WgXcQ")
                .as_deref(),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn classifies_urls() {
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            UrlKind::Video
        ));
        // URL with both v= and list= is treated as video
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL0123456789abcdefghij"),
            UrlKind::Video
        ));
        match classify_url("https://www.youtube.com/playlist?list=PL0123456789abcdefghij") {
            UrlKind::Playlist(id) => assert_eq!(id, "PL0123456789abcdefghij"),
            _ => panic!("expected playlist"),
        }
        // watch?list= without v= is also treated as playlist
        assert!(matches!(
            classify_url("https://www.youtube.com/watch?list=PL0123456789abcdefghij"),
            UrlKind::Playlist(_)
        ));
        // Short special list IDs like WL are also accepted (expansion is left to yt-dlp)
        assert!(matches!(
            classify_url("https://www.youtube.com/playlist?list=WL"),
            UrlKind::Playlist(_)
        ));
        assert!(matches!(
            classify_url("https://example.com/watch?v=x"),
            UrlKind::Unknown
        ));
        assert!(matches!(
            classify_url("https://www.youtube.com/feed/library"),
            UrlKind::Unknown
        ));
        assert!(matches!(
            classify_url("https://youtube.com.evil.example/playlist?list=PL0123456789abcdefghij"),
            UrlKind::Unknown
        ));
    }

    #[test]
    fn video_id_format_check() {
        assert!(is_video_id("dQw4w9WgXcQ"));
        assert!(!is_video_id("short"));
        assert!(!is_video_id("dQw4w9WgXcQ-too-long"));
    }
}
