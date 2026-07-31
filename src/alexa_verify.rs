//! Alexa request signature verification.
//!
//! /alexa is exempt from Bearer auth; instead, Amazon's signature verification
//! confirms that requests genuinely originate from Alexa.
//! Steps: validate certificate URL → verify certificate chain (SAN, expiry,
//! trust chain) → verify request body signature → check timestamp freshness.
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
use tokio::sync::{Mutex, Semaphore};

/// Allowed timestamp skew (Amazon specifies max 150 seconds).
const TIMESTAMP_TOLERANCE_SECS: i64 = 150;
/// Timeout for fetching the certificate chain.
const CERT_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on cert fetches in flight. /alexa is unauthenticated, so anyone can make
/// this endpoint issue an outbound request by naming a cert URL we have not
/// cached. Steady-state traffic reuses the cache and never reaches the fetch,
/// so a small cap is generous for real Alexa requests while keeping a flood of
/// forged URLs from tying up connections and sockets.
const CERT_FETCH_MAX_INFLIGHT: usize = 4;
/// Bound query-variant and certificate-rotation entries. Alexa normally uses a
/// single URL, so this leaves ample overlap while preventing unbounded growth.
const CERT_CACHE_MAX_ENTRIES: usize = 16;
/// Hostname required in the certificate's SAN.
const ECHO_API_SAN: &str = "echo-api.amazon.com";

/// Cache of verified public keys. Certificate URLs change on cert renewal,
/// so each URL is cached until the certificate's expiry.
struct CachedKey {
    key: PKey<Public>,
    not_after: SystemTime,
}

fn cert_cache() -> &'static Mutex<HashMap<String, CachedKey>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedKey>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop entries whose certificate has expired. Run on every cache access, so an
/// entry that can no longer verify anything never occupies a slot.
fn prune_expired(cache: &mut HashMap<String, CachedKey>) {
    let now = SystemTime::now();
    cache.retain(|_, cached| now < cached.not_after);
}

/// Look up a still-valid cached key for this certificate URL.
async fn cached_key(cert_url: &str) -> Option<PKey<Public>> {
    let mut cache = cert_cache().lock().await;
    prune_expired(&mut cache);
    cache.get(cert_url).map(|c| c.key.clone())
}

/// Cache a verified key, keeping the map bounded. Only an entry for a URL not
/// already cached can grow the map, and the soonest-to-expire entry is the one
/// evicted, since it is the closest to being useless anyway.
async fn cache_verified_key(cert_url: &str, key: PKey<Public>, not_after: SystemTime) {
    let mut cache = cert_cache().lock().await;
    prune_expired(&mut cache);
    if !cache.contains_key(cert_url)
        && cache.len() >= CERT_CACHE_MAX_ENTRIES
        && let Some(soonest) = cache
            .iter()
            .min_by_key(|(_, cached)| cached.not_after)
            .map(|(url, _)| url.clone())
    {
        cache.remove(&soonest);
    }
    cache.insert(cert_url.to_string(), CachedKey { key, not_after });
}

fn cert_fetch_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(CERT_FETCH_MAX_INFLIGHT))
}

/// Shared HTTP client for cert fetches, so connections and the TLS session
/// cache are reused across requests instead of rebuilt per fetch.
fn cert_http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(CERT_FETCH_TIMEOUT)
                .build()
                .map_err(|e| format!("http client init failed: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Verify request body authenticity using signature headers and the certificate chain.
pub async fn verify_request(headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
    let cert_url = header_str(headers, "signaturecertchainurl")?;
    // Prefer SHA-256 signature (Signature-256) if available; fall back to legacy SHA-1
    let (sig_b64, digest) = match header_str(headers, "signature-256") {
        Ok(s) => (s, MessageDigest::sha256()),
        Err(_) => (header_str(headers, "signature")?, MessageDigest::sha1()),
    };

    let cert_url = validate_cert_url(cert_url)?;
    let sig = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|e| format!("invalid signature base64: {e}"))?;
    // Reject malformed signatures before doing certificate network I/O.
    let key = fetch_verified_key(cert_url.as_str()).await?;

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

/// Verify that request.timestamp is within tolerance of current time (replay protection).
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

/// Validate that the certificate URL meets Amazon's requirements (https,
/// s3.amazonaws.com, port 443, path under /echo.api/).
/// The url crate's parser normalizes dot segments (..).
/// A query string is left intact — Amazon's requirements do not forbid one, so
/// rejecting it would break verification outright if one ever appears.
fn validate_cert_url(cert_url: &str) -> Result<url::Url, String> {
    let mut u = url::Url::parse(cert_url).map_err(|e| format!("invalid cert URL: {e}"))?;
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
    // Userinfo is never legitimate here and only serves to disguise the host.
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!("cert URL must not contain userinfo: {cert_url}"));
    }
    if !u.path().starts_with("/echo.api/") {
        return Err(format!("cert URL path is not under /echo.api/: {cert_url}"));
    }
    // Fragments are not sent to the server, so drop it to keep the cache key
    // canonical instead of letting it multiply entries for the same cert.
    u.set_fragment(None);
    Ok(u)
}

/// Fetch and verify the certificate chain, returning the public key for signature verification (cached).
async fn fetch_verified_key(cert_url: &str) -> Result<PKey<Public>, String> {
    if let Some(key) = cached_key(cert_url).await {
        return Ok(key);
    }

    // Queue rather than fail fast: a burst of genuine Alexa events at cold start
    // would otherwise have most of its requests rejected. The wait is bounded so
    // a flood cannot make requests pile up indefinitely either.
    let _slot = tokio::time::timeout(CERT_FETCH_TIMEOUT, cert_fetch_slots().acquire())
        .await
        .map_err(|_| "timed out waiting for a certificate fetch slot".to_string())?
        .map_err(|e| format!("certificate fetch semaphore closed: {e}"))?;

    // A request ahead of us in the queue may have fetched the same certificate
    // while we waited, which is what a cold-start burst looks like once the
    // permits are taken. The first CERT_FETCH_MAX_INFLIGHT requests never wait,
    // so they still duplicate one fetch between them — bounded and one-off, and
    // cheaper than the per-URL single-flight map it would take to avoid.
    if let Some(key) = cached_key(cert_url).await {
        return Ok(key);
    }

    let pem = cert_http_client()?
        .get(cert_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("failed to fetch cert chain: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("failed to read cert chain: {e}"))?;

    let (key, not_after) = verify_cert_chain(&pem)?;
    cache_verified_key(cert_url, key.clone(), not_after).await;
    tracing::info!("Verified and cached Alexa signing cert: {cert_url}");
    Ok(key)
}

/// Verify a PEM certificate chain and return (leaf public key, expiry).
/// The chain is verified against the system CA store (including expiry check).
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

    // Convert the certificate's expiry to SystemTime for cache retention
    let now = Asn1Time::days_from_now(0).map_err(|e| format!("time init failed: {e}"))?;
    let remaining = now
        .diff(leaf.not_after())
        .map_err(|e| format!("failed to read cert expiry: {e}"))?;
    let secs = i64::from(remaining.days) * 86400 + i64::from(remaining.secs);
    // A certificate with no lifetime left can verify nothing. Rejecting it here
    // is also what makes the conversion exact rather than a cast that would turn
    // an expired certificate into one valid for hundreds of billions of years.
    let not_after = match u64::try_from(secs) {
        Ok(secs) if secs > 0 => SystemTime::now() + Duration::from_secs(secs),
        _ => return Err("certificate expired".to_string()),
    };

    let key = leaf
        .public_key()
        .map_err(|e| format!("failed to extract public key: {e}"))?;
    Ok((key, not_after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::rsa::Rsa;
    use serde_json::json;
    use time::format_description::well_known::Rfc3339;

    #[test]
    fn accepts_valid_cert_urls() {
        for url in [
            "https://s3.amazonaws.com/echo.api/echo-api-cert.pem",
            "https://s3.amazonaws.com:443/echo.api/echo-api-cert.pem",
            "https://S3.AMAZONAWS.COM/echo.api/cert.pem",
            // Dot segments are normalized back under /echo.api/
            "https://s3.amazonaws.com/echo.api/../echo.api/echo-api-cert.pem",
            "https://s3.amazonaws.com/echo.api/cert.pem?versionId=1",
        ] {
            assert!(validate_cert_url(url).is_ok(), "should accept {url}");
        }
    }

    #[test]
    fn cert_url_fragment_is_dropped() {
        let u = validate_cert_url("https://s3.amazonaws.com/echo.api/cert.pem#frag").unwrap();
        assert_eq!(u.as_str(), "https://s3.amazonaws.com/echo.api/cert.pem");
    }

    #[tokio::test]
    async fn cert_cache_is_bounded_and_prunes_expired_entries() {
        let private = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let public = PKey::public_key_from_pem(&private.public_key_to_pem().unwrap()).unwrap();
        let now = SystemTime::now();

        {
            let mut cache = cert_cache().lock().await;
            cache.clear();
            cache.insert(
                "expired".to_string(),
                CachedKey {
                    key: public.clone(),
                    not_after: now - Duration::from_secs(1),
                },
            );
        }

        for i in 0..(CERT_CACHE_MAX_ENTRIES + 4) {
            cache_verified_key(
                &format!("https://s3.amazonaws.com/echo.api/cert.pem?v={i}"),
                public.clone(),
                now + Duration::from_secs(3600 + i as u64),
            )
            .await;
        }

        let mut cache = cert_cache().lock().await;
        assert_eq!(cache.len(), CERT_CACHE_MAX_ENTRIES);
        assert!(!cache.contains_key("expired"));
        assert!(cache.contains_key(&format!(
            "https://s3.amazonaws.com/echo.api/cert.pem?v={}",
            CERT_CACHE_MAX_ENTRIES + 3
        )));
        cache.clear();
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
            "https://user@s3.amazonaws.com/echo.api/cert.pem",
            "https://user:pass@s3.amazonaws.com/echo.api/cert.pem",
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
