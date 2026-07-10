//! Alexa リクエスト署名検証
//!
//! /alexa は Bearer 認証の対象外のため、Amazon が規定する署名検証で
//! リクエストが本当に Alexa から送られたものであることを確認する。
//! 手順: 証明書 URL の妥当性確認 → 証明書チェーン検証 (SAN・有効期限・
//! 信頼チェーン) → リクエストボディの署名検証 → timestamp の鮮度確認。
//! <https://developer.amazon.com/docs/custom-skills/host-a-custom-skill-as-a-web-service.html>

use axum::http::HeaderMap;
use base64::Engine;
use openssl::asn1::Asn1Time;
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Public};
use openssl::sign::Verifier;
use openssl::stack::Stack;
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::{X509, X509StoreContext, X509VerifyResult};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// リクエスト timestamp の許容ずれ (Amazon の規定は最大 150 秒)
const TIMESTAMP_TOLERANCE_SECS: i64 = 150;
/// 証明書チェーン取得のタイムアウト
const CERT_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 証明書の SAN に含まれるべきホスト名
const ECHO_API_SAN: &str = "echo-api.amazon.com";

/// 検証済み公開鍵のキャッシュ。証明書 URL は証明書の更新で変わるため
/// URL ごとに証明書の有効期限まで保持する
struct CachedKey {
    key: PKey<Public>,
    not_after: SystemTime,
}

fn cert_cache() -> &'static Mutex<HashMap<String, CachedKey>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedKey>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 署名ヘッダと証明書チェーンでリクエストボディの真正性を検証する
pub async fn verify_request(headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
    let cert_url = header_str(headers, "signaturecertchainurl")?;
    // SHA-256 署名 (Signature-256) があれば優先し、なければ従来の SHA-1
    let (sig_b64, digest) = match header_str(headers, "signature-256") {
        Ok(s) => (s, MessageDigest::sha256()),
        Err(_) => (header_str(headers, "signature")?, MessageDigest::sha1()),
    };

    validate_cert_url(cert_url)?;
    let key = fetch_verified_key(cert_url).await?;

    let sig = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|e| format!("invalid signature base64: {e}"))?;

    let mut verifier =
        Verifier::new(digest, &key).map_err(|e| format!("verifier init failed: {e}"))?;
    verifier
        .update(body)
        .map_err(|e| format!("verifier update failed: {e}"))?;
    match verifier.verify(&sig) {
        Ok(true) => Ok(()),
        Ok(false) => Err("signature mismatch".to_string()),
        Err(e) => Err(format!("signature verification failed: {e}")),
    }
}

/// request.timestamp が現在時刻から許容範囲内であることを確認する (リプレイ対策)
pub fn verify_timestamp(body: &Value) -> Result<(), String> {
    let ts = body["request"]["timestamp"]
        .as_str()
        .ok_or("missing request.timestamp")?;
    let t = time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .map_err(|e| format!("invalid timestamp '{ts}': {e}"))?;
    let diff = (time::OffsetDateTime::now_utc() - t).whole_seconds().abs();
    if diff > TIMESTAMP_TOLERANCE_SECS {
        return Err(format!("timestamp out of tolerance ({diff}s)"));
    }
    Ok(())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| format!("missing {name} header"))
}

/// 証明書 URL が Amazon の規定 (https / s3.amazonaws.com / ポート 443 /
/// パスが /echo.api/ 配下) を満たすことを確認する。
/// url クレートのパースがドットセグメント (..) の正規化を行う
fn validate_cert_url(cert_url: &str) -> Result<(), String> {
    let u = url::Url::parse(cert_url).map_err(|e| format!("invalid cert URL: {e}"))?;
    if u.scheme() != "https" {
        return Err(format!("cert URL scheme is not https: {cert_url}"));
    }
    if !u
        .host_str()
        .is_some_and(|h| h.eq_ignore_ascii_case("s3.amazonaws.com"))
    {
        return Err(format!("cert URL host is not s3.amazonaws.com: {cert_url}"));
    }
    if u.port().is_some_and(|p| p != 443) {
        return Err(format!("cert URL port is not 443: {cert_url}"));
    }
    if !u.path().starts_with("/echo.api/") {
        return Err(format!("cert URL path is not under /echo.api/: {cert_url}"));
    }
    Ok(())
}

/// 証明書チェーンを取得・検証し、署名検証用の公開鍵を返す (キャッシュあり)
async fn fetch_verified_key(cert_url: &str) -> Result<PKey<Public>, String> {
    {
        let cache = cert_cache().lock().await;
        if let Some(c) = cache.get(cert_url)
            && SystemTime::now() < c.not_after
        {
            return Ok(c.key.clone());
        }
    }

    let pem = reqwest::Client::builder()
        .timeout(CERT_FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("http client init failed: {e}"))?
        .get(cert_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("failed to fetch cert chain: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("failed to read cert chain: {e}"))?;

    let (key, not_after) = verify_cert_chain(&pem)?;
    cert_cache().lock().await.insert(
        cert_url.to_string(),
        CachedKey {
            key: key.clone(),
            not_after,
        },
    );
    tracing::info!("Verified and cached Alexa signing cert: {cert_url}");
    Ok(key)
}

/// PEM 証明書チェーンを検証し、(リーフ証明書の公開鍵, 有効期限) を返す。
/// チェーンはシステムの CA ストアを起点に検証する (有効期限の確認も含む)
fn verify_cert_chain(pem: &[u8]) -> Result<(PKey<Public>, SystemTime), String> {
    let mut certs =
        X509::stack_from_pem(pem).map_err(|e| format!("failed to parse cert chain: {e}"))?;
    if certs.is_empty() {
        return Err("cert chain is empty".to_string());
    }
    let leaf = certs.remove(0);

    let san_ok = leaf
        .subject_alt_names()
        .is_some_and(|names| names.iter().any(|n| n.dnsname() == Some(ECHO_API_SAN)));
    if !san_ok {
        return Err(format!("certificate SAN does not include {ECHO_API_SAN}"));
    }

    let mut store = X509StoreBuilder::new().map_err(|e| format!("cert store init failed: {e}"))?;
    store
        .set_default_paths()
        .map_err(|e| format!("failed to load system CA store: {e}"))?;
    let store = store.build();

    let mut chain = Stack::new().map_err(|e| format!("stack init failed: {e}"))?;
    for c in certs {
        chain
            .push(c)
            .map_err(|e| format!("stack push failed: {e}"))?;
    }

    let mut ctx =
        X509StoreContext::new().map_err(|e| format!("verify context init failed: {e}"))?;
    let result = ctx
        .init(&store, &leaf, &chain, |c| {
            c.verify_cert()?;
            Ok(c.error())
        })
        .map_err(|e| format!("certificate verification failed: {e}"))?;
    if result != X509VerifyResult::OK {
        return Err(format!(
            "certificate chain invalid: {}",
            result.error_string()
        ));
    }

    // キャッシュの保持期限として有効期限を SystemTime に換算する
    let now = Asn1Time::days_from_now(0).map_err(|e| format!("time init failed: {e}"))?;
    let remaining = now
        .diff(leaf.not_after())
        .map_err(|e| format!("failed to read cert expiry: {e}"))?;
    let secs = remaining.days as i64 * 86400 + remaining.secs as i64;
    if secs <= 0 {
        return Err("certificate expired".to_string());
    }
    let not_after = SystemTime::now() + Duration::from_secs(secs as u64);

    let key = leaf
        .public_key()
        .map_err(|e| format!("failed to extract public key: {e}"))?;
    Ok((key, not_after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::format_description::well_known::Rfc3339;

    #[test]
    fn accepts_valid_cert_urls() {
        for url in [
            "https://s3.amazonaws.com/echo.api/echo-api-cert.pem",
            "https://s3.amazonaws.com:443/echo.api/echo-api-cert.pem",
            "https://S3.AMAZONAWS.COM/echo.api/cert.pem",
            // ドットセグメントは正規化されて /echo.api/ 配下に戻る
            "https://s3.amazonaws.com/echo.api/../echo.api/echo-api-cert.pem",
        ] {
            assert!(validate_cert_url(url).is_ok(), "should accept {url}");
        }
    }

    #[test]
    fn rejects_invalid_cert_urls() {
        for url in [
            "http://s3.amazonaws.com/echo.api/echo-api-cert.pem",
            "https://s3.amazonaws.com:563/echo.api/echo-api-cert.pem",
            "https://myhost.example.com/echo.api/echo-api-cert.pem",
            "https://s3.amazonaws.com/EcHo.aPi/echo-api-cert.pem",
            "https://s3.amazonaws.com/echo.api/../not-echo/cert.pem",
            "https://s3.amazonaws.com.evil.example/echo.api/cert.pem",
            "not a url",
        ] {
            assert!(validate_cert_url(url).is_err(), "should reject {url}");
        }
    }

    #[test]
    fn timestamp_within_tolerance_passes() {
        let ts = time::OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        let body = json!({ "request": { "timestamp": ts } });
        assert!(verify_timestamp(&body).is_ok());
    }

    #[test]
    fn stale_or_missing_timestamp_fails() {
        let old = (time::OffsetDateTime::now_utc()
            - time::Duration::seconds(TIMESTAMP_TOLERANCE_SECS + 60))
        .format(&Rfc3339)
        .unwrap();
        let body = json!({ "request": { "timestamp": old } });
        assert!(verify_timestamp(&body).is_err());

        assert!(verify_timestamp(&json!({ "request": {} })).is_err());
        assert!(verify_timestamp(&json!({ "request": { "timestamp": "garbage" } })).is_err());
    }
}
