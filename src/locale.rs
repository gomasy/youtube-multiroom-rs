//! Resolving a client-advertised language code to a locale we ship.
//!
//! Single home for that policy: the HTTP layer feeds it the X-App-Lang header,
//! the Alexa layer request.locale. The `rust_i18n::i18n!` invocation itself has
//! to stay at the crate root, so only the resolution rules live here.

/// Locale used when a request advertises no language, or one we don't ship.
/// Read from the i18n! macro's constant rather than hardcoded, so the two
/// cannot disagree.
pub fn fallback() -> &'static str {
    crate::_RUST_I18N_FALLBACK_LOCALE
        .and_then(|l| l.first())
        .copied()
        .unwrap_or_else(|| crate::die("i18n fallback locale is not set"))
}

/// Resolve a client-advertised language code to a locale we ship, falling back
/// to the built-in default.
pub fn or_default(code: Option<&str>) -> String {
    code.and_then(resolve)
        .unwrap_or_else(|| fallback().to_string())
}

/// Resolve a language code to the best available locale, or `None` if unknown.
fn resolve(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_lowercase();
    if code.is_empty() {
        return None;
    }
    let available = rust_i18n::available_locales!();
    if available.iter().any(|a| a.as_ref() == code) {
        return Some(code);
    }
    let base = code.split('-').next()?;
    if available.iter().any(|a| a.as_ref() == base) {
        return Some(base.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_i18n::t;

    #[test]
    fn resolves_a_language_code_to_a_shipped_locale() {
        // Exact match, then the base of a regional code
        assert_eq!(resolve("ja"), Some("ja".to_string()));
        assert_eq!(resolve("en-US"), Some("en".to_string()));
        // Nothing to resolve to, so or_default falls back
        assert_eq!(resolve("xx-YY"), None);
        assert_eq!(resolve("   "), None);
        assert_eq!(or_default(Some("xx")), fallback());
        assert_eq!(or_default(None), "en");
    }

    #[test]
    fn embedded_catalogs_load_and_substitute() {
        assert_eq!(
            t!("alexa_connected", locale = "en"),
            "Connected to YouTube MultiRoom. You can control playback from the web interface."
        );
        assert_eq!(
            t!("alexa_connected", locale = "ja"),
            "YouTube マルチルームに接続しました。Web 画面から操作できます。"
        );
        assert_eq!(
            t!("api_added_to_playlist", locale = "en", title = "Song"),
            "Added \"Song\" to playlist"
        );
    }
}
