use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// TTL for signed stream URLs. URLs are issued at playback start and reused
/// for Range requests throughout long tracks, so the TTL must be generous.
const STREAM_URL_TTL_SECS: u64 = 24 * 3600;

/// The one path that accepts `?token=`. main.rs registers the route from this
/// constant so a rename cannot silently disable query-token auth; it must also
/// match the URL built by useWebSocket() in front/src/hooks.ts.
pub const WS_PATH: &str = "/ws";

pub async fn require_token(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(ref expected) = state.api_token else {
        return next.run(request).await;
    };

    let path = request.uri().path();

    // Alexa webhook relies on skill signature verification, not Bearer auth
    if path == "/alexa" {
        return next.run(request).await;
    }

    // Echo devices cannot attach Authorization headers, so stream URLs are
    // authenticated by HMAC-signed query parameters (exp & sig). Without a
    // valid signature the request falls through to Bearer auth below.
    if let Some(audio_id) = audio_endpoint_id(path)
        && verify_stream_query(expected, audio_id, request.uri().query())
    {
        return next.run(request).await;
    }

    let header_ok = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token, expected));

    let query_ok = verify_query_token(expected, path, request.uri().query());

    if header_ok || query_ok {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "Unauthorized" })),
        )
            .into_response()
    }
}

/// Generate a signed query string ("exp=...&sig=...") for stream URLs.
pub fn stream_query(secret: &str, audio_id: &str) -> String {
    // exp = 0 (unreadable clock) yields an already-expired URL, which is the
    // safe outcome: playback fails rather than the URL never expiring.
    let exp = now_secs().map_or(0, |now| now + STREAM_URL_TTL_SECS);
    let sig = sign(secret, audio_id, exp);
    format!("exp={exp}&sig={sig}")
}

/// Build a stream path for a track (with signed query when auth is enabled).
/// Neither Echo nor the browser audio element can attach Authorization headers,
/// so playback URLs rely on this signature. Live streams use /live (CDN relay)
/// instead of /stream (local file).
pub fn stream_path(api_token: Option<&str>, audio_id: &str, is_live: bool) -> String {
    let endpoint = if is_live { "live" } else { "stream" };
    let mut path = format!("/api/audio/{audio_id}/{endpoint}");
    if let Some(secret) = api_token {
        path.push('?');
        path.push_str(&stream_query(secret, audio_id));
    }
    path
}

/// Whether `?token=` authenticates this request. Only the WebSocket handshake
/// qualifies — the browser WebSocket API cannot attach headers. Accepting it
/// everywhere would spread the token into proxy logs, browser history and
/// Referer headers for requests that have no need for it.
fn verify_query_token(expected: &str, path: &str, query: Option<&str>) -> bool {
    path == WS_PATH
        && query_param(query, "token").is_some_and(|token| constant_time_eq(&token, expected))
}

fn verify_stream_query(secret: &str, audio_id: &str, query: Option<&str>) -> bool {
    let Some(exp) = query_param(query, "exp").and_then(|v| v.parse::<u64>().ok()) else {
        return false;
    };
    let Some(sig) = query_param(query, "sig") else {
        return false;
    };
    // An unreadable clock fails closed: no expiry can be trusted
    now_secs().is_some_and(|now| exp >= now) && constant_time_eq(&sign(secret, audio_id, exp), &sig)
}

fn sign(secret: &str, audio_id: &str, exp: u64) -> String {
    // Modelled as a Result but cannot fail: HMAC hashes a key longer than the
    // block size and pads a shorter one, so every length is accepted. Nor would
    // there be a safe fallback — signing with anything but the configured
    // secret hands out URLs that verify against nothing.
    #[expect(
        clippy::expect_used,
        reason = "Hmac::new_from_slice accepts a key of any length"
    )]
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(audio_id.as_bytes());
    mac.update(b"\n");
    mac.update(exp.to_string().as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// Lowercase hex in a single allocation. An Echo re-verifies its signed URL on
/// every Range request during a track, so this runs far more often than the one
/// signature per playback that produced it.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Extract the track ID from "/api/audio/{id}/stream" or "/api/audio/{id}/live".
/// The paths must match the route definitions in main.rs and URL generation in alexa.rs.
fn audio_endpoint_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/audio/")?;
    rest.strip_suffix("/stream")
        .or_else(|| rest.strip_suffix("/live"))
}

/// Extract a value from a query string. Percent-decodes %XX sequences so that
/// tokens encoded by the client's encodeURIComponent match correctly.
fn query_param<'a>(query: Option<&'a str>, key: &str) -> Option<Cow<'a, str>> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> Cow<'_, str> {
    if !s.contains('%') {
        return Cow::Borrowed(s);
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = hex_pair(bytes[i + 1], bytes[i + 2])
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let h = (hi as char).to_digit(16)?;
    let l = (lo as char).to_digit(16)?;
    Some((h * 16 + l) as u8)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Seconds since the UNIX epoch, or `None` if the system clock predates 1970.
fn now_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_query_verifies() {
        let q = stream_query("secret", "abc123");
        assert!(verify_stream_query("secret", "abc123", Some(&q)));
    }

    #[test]
    fn hex_pads_every_byte_to_two_digits() {
        // Signing and verifying both go through hex(), so a byte that lost its
        // leading zero would still compare equal to itself while quietly
        // shortening the signature. The round-trip tests cannot catch that,
        // which is why the encoding is pinned here directly.
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex(&[]), "");
        // SHA-256 is 32 bytes, so every signature is 64 characters wide
        assert_eq!(sign("secret", "abc123", 0).len(), 64);
    }

    #[test]
    fn rejects_wrong_track_or_secret() {
        let q = stream_query("secret", "abc123");
        assert!(!verify_stream_query("secret", "other", Some(&q)));
        assert!(!verify_stream_query("wrong", "abc123", Some(&q)));
    }

    #[test]
    fn rejects_expired_or_tampered_exp() {
        let now = now_secs().expect("test host clock is after 1970");
        let exp = now - 1;
        let q = format!("exp={exp}&sig={}", sign("secret", "abc123", exp));
        assert!(!verify_stream_query("secret", "abc123", Some(&q)));

        // Extending exp invalidates the signature
        let future = now + 100;
        let q = format!("exp={future}&sig={}", sign("secret", "abc123", exp));
        assert!(!verify_stream_query("secret", "abc123", Some(&q)));
    }

    #[test]
    fn stream_path_matches_endpoint_and_auth() {
        // Without auth: bare path
        assert_eq!(
            stream_path(None, "abc123", false),
            "/api/audio/abc123/stream"
        );
        assert_eq!(stream_path(None, "abc123", true), "/api/audio/abc123/live");

        // With auth: signed query that passes verification
        let path = stream_path(Some("secret"), "abc123", false);
        let (base, query) = path.split_once('?').unwrap();
        assert_eq!(base, "/api/audio/abc123/stream");
        assert!(verify_stream_query("secret", "abc123", Some(query)));
    }

    #[test]
    fn audio_endpoint_id_handles_stream_and_live() {
        assert_eq!(
            audio_endpoint_id("/api/audio/abc123/stream"),
            Some("abc123")
        );
        assert_eq!(audio_endpoint_id("/api/audio/abc123/live"), Some("abc123"));
        assert_eq!(audio_endpoint_id("/api/audio/abc123/other"), None);
        assert_eq!(audio_endpoint_id("/api/tracks"), None);
    }

    #[test]
    fn query_token_is_accepted_only_on_the_ws_path() {
        let q = Some("token=secret");
        assert!(verify_query_token("secret", "/ws", q));
        // Every other endpoint must use the Authorization header
        assert!(!verify_query_token("secret", "/api/tracks", q));
        assert!(!verify_query_token("secret", "/api/play", q));
        assert!(!verify_query_token("secret", "/ws/", q));
        // Wrong or missing token is still rejected on /ws
        assert!(!verify_query_token("secret", "/ws", Some("token=wrong")));
        assert!(!verify_query_token("secret", "/ws", None));
    }

    #[test]
    fn query_param_decodes_percent_encoding() {
        assert_eq!(
            query_param(Some("token=a%2Bb%20c"), "token").as_deref(),
            Some("a+b c")
        );
        assert_eq!(
            query_param(Some("token=plain"), "token").as_deref(),
            Some("plain")
        );
        assert_eq!(query_param(Some("token=x"), "other"), None);
    }

    #[test]
    fn rejects_missing_query() {
        assert!(!verify_stream_query("secret", "abc123", None));
        assert!(!verify_stream_query("secret", "abc123", Some("exp=123")));
        assert!(!verify_stream_query(
            "secret",
            "abc123",
            Some("sig=deadbeef")
        ));
    }
}
