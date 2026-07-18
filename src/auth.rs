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

/// 署名付きストリーム URL の有効期限。
/// URL は再生開始時に発行され、長尺トラックでは再生中も Range リクエストで
/// 同じ URL が使われ続けるため、十分長めに取る
const STREAM_URL_TTL_SECS: u64 = 24 * 3600;

pub async fn require_token(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(ref expected) = state.api_token else {
        return next.run(request).await;
    };

    let path = request.uri().path();

    // Alexa webhook はスキル署名検証が前提のため Bearer 認証の対象外
    if path == "/alexa" {
        return next.run(request).await;
    }

    // Echo は Authorization ヘッダを付けられないため、
    // ストリーム URL は HMAC 署名クエリ (exp & sig) で認証する
    if let Some(audio_id) = audio_endpoint_id(path)
        && verify_stream_query(expected, audio_id, request.uri().query())
    {
        return next.run(request).await;
    }
    // 署名がなくても通常のトークン認証は受け付ける (下へフォールスルー)

    let header_ok = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token, expected));

    let query_ok = query_param(request.uri().query(), "token")
        .is_some_and(|token| constant_time_eq(&token, expected));

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

/// ストリーム URL に付与する署名クエリ ("exp=...&sig=..." 形式) を生成する
pub fn stream_query(secret: &str, audio_id: &str) -> String {
    let exp = now_secs() + STREAM_URL_TTL_SECS;
    let sig = sign(secret, audio_id, exp);
    format!("exp={exp}&sig={sig}")
}

/// トラックのストリーム再生パスを組み立てる (認証有効時は署名クエリ付き)。
/// Echo もブラウザの audio 要素も Authorization ヘッダを付けられないため、
/// 再生 URL はこの署名で認証する。ライブ配信はファイルがなく、CDN の音声を
/// 中継する /live を使う
pub fn stream_path(api_token: Option<&str>, audio_id: &str, is_live: bool) -> String {
    let endpoint = if is_live { "live" } else { "stream" };
    let mut path = format!("/api/audio/{audio_id}/{endpoint}");
    if let Some(secret) = api_token {
        path.push('?');
        path.push_str(&stream_query(secret, audio_id));
    }
    path
}

fn verify_stream_query(secret: &str, audio_id: &str, query: Option<&str>) -> bool {
    let Some(exp) = query_param(query, "exp").and_then(|v| v.parse::<u64>().ok()) else {
        return false;
    };
    let Some(sig) = query_param(query, "sig") else {
        return false;
    };
    exp >= now_secs() && constant_time_eq(&sign(secret, audio_id, exp), &sig)
}

fn sign(secret: &str, audio_id: &str, exp: u64) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(audio_id.as_bytes());
    mac.update(b"\n");
    mac.update(exp.to_string().as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// "/api/audio/{id}/stream" または "/api/audio/{id}/live" からトラック ID を取り出す。
/// パスは main.rs のルート定義・alexa.rs の URL 生成と一致していること
fn audio_endpoint_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/audio/")?;
    rest.strip_suffix("/stream")
        .or_else(|| rest.strip_suffix("/live"))
}

/// クエリ文字列から値を取り出す。クライアントが encodeURIComponent 等で
/// エンコードしたトークンも一致するよう、%XX はデコードして返す
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs()
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
    fn rejects_wrong_track_or_secret() {
        let q = stream_query("secret", "abc123");
        assert!(!verify_stream_query("secret", "other", Some(&q)));
        assert!(!verify_stream_query("wrong", "abc123", Some(&q)));
    }

    #[test]
    fn rejects_expired_or_tampered_exp() {
        let exp = now_secs() - 1;
        let q = format!("exp={exp}&sig={}", sign("secret", "abc123", exp));
        assert!(!verify_stream_query("secret", "abc123", Some(&q)));

        // exp を伸ばすと署名が合わなくなる
        let future = now_secs() + 100;
        let q = format!("exp={future}&sig={}", sign("secret", "abc123", exp));
        assert!(!verify_stream_query("secret", "abc123", Some(&q)));
    }

    #[test]
    fn stream_path_matches_endpoint_and_auth() {
        // 認証無効時は素のパス
        assert_eq!(
            stream_path(None, "abc123", false),
            "/api/audio/abc123/stream"
        );
        assert_eq!(stream_path(None, "abc123", true), "/api/audio/abc123/live");

        // 認証有効時は署名クエリ付きで、そのまま検証を通る
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
