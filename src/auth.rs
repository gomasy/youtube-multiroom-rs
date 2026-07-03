use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
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
    if let Some(audio_id) = stream_audio_id(path) {
        if verify_stream_query(expected, audio_id, request.uri().query()) {
            return next.run(request).await;
        }
        // 署名がなくても通常のトークン認証は受け付ける (下へフォールスルー)
    }

    let header_ok = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| token == expected.as_str());

    let query_ok = query_param(request.uri().query(), "token")
        .is_some_and(|token| token == expected.as_str());

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

fn verify_stream_query(secret: &str, audio_id: &str, query: Option<&str>) -> bool {
    let Some(exp) = query_param(query, "exp").and_then(|v| v.parse::<u64>().ok()) else {
        return false;
    };
    let Some(sig) = query_param(query, "sig") else {
        return false;
    };
    exp >= now_secs() && constant_time_eq(&sign(secret, audio_id, exp), sig)
}

fn sign(secret: &str, audio_id: &str, exp: u64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(audio_id.as_bytes());
    mac.update(b"\n");
    mac.update(exp.to_string().as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// "/api/audio/{id}/stream" からトラック ID を取り出す。
/// パスは main.rs のルート定義・alexa.rs の URL 生成と一致していること
fn stream_audio_id(path: &str) -> Option<&str> {
    path.strip_prefix("/api/audio/")?.strip_suffix("/stream")
}

fn query_param<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
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
    fn rejects_missing_query() {
        assert!(!verify_stream_query("secret", "abc123", None));
        assert!(!verify_stream_query("secret", "abc123", Some("exp=123")));
        assert!(!verify_stream_query("secret", "abc123", Some("sig=deadbeef")));
    }
}
