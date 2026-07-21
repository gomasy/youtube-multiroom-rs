//! Internationalization powered by `rust-i18n`.
//!
//! Translations live in YAML files under the `locales/` directory, one file per
//! language named after its code, e.g. `en.yml`, `ja.yml`. Every `*.yml` is
//! embedded into the binary at compile time, so the binary is self-contained.
//!
//! [`Lang`] is a lightweight, `Copy` handle holding the requested language
//! code. Its methods resolve translations via `rust_i18n::t!`, falling back to
//! the default language (Japanese) when a code has no file.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use rust_i18n::t;

const DEFAULT_LANG: &str = "ja";

fn locale_available(code: &str) -> bool {
    let available = rust_i18n::available_locales!();
    available.iter().any(|a| a.as_ref() == code)
}

/// Whether a language code resolves to a loaded locale (exact or base code).
fn is_known(code: &str) -> bool {
    let base = code.split('-').next().unwrap_or(code);
    locale_available(code) || locale_available(base)
}

/// Intern a language code as a `&'static str`, leaking each distinct value at
/// most once.
fn intern(code: &str) -> &'static str {
    static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    let mut set = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = set.get(code) {
        existing
    } else {
        let leaked: &'static str = Box::leak(code.to_string().into_boxed_str());
        set.insert(leaked);
        leaked
    }
}

/// Resolve a language code to the best available locale.
/// "en-us" -> "en" if "en" exists, unknown codes -> DEFAULT_LANG.
fn resolve_locale(code: &str) -> &'static str {
    if locale_available(code) {
        return intern(code);
    }
    let base = code.split('-').next().unwrap_or(code);
    if locale_available(base) {
        return intern(base);
    }
    DEFAULT_LANG
}

macro_rules! catalog_accessors {
    ($($name:ident),* $(,)?) => {
        $(pub fn $name(&self) -> String {
            t!(stringify!($name), locale = self.locale).into_owned()
        })*
    };
}

/// A resolved request language.
///
/// Cheap to copy; holds only the language code. Translations are looked up via
/// `rust_i18n::t!` on each method call.
#[derive(Clone, Copy, Debug)]
pub struct Lang {
    code: &'static str,
    locale: &'static str,
}

impl Lang {
    fn new(code: String) -> Self {
        let code = intern(&code);
        Lang {
            code,
            locale: resolve_locale(code),
        }
    }

    /// Parse a language code such as "en", "en-US", "ja", or "ja-JP".
    pub fn parse(s: &str) -> Option<Self> {
        let code = s.trim().to_ascii_lowercase();
        if code.is_empty() {
            None
        } else {
            Some(Lang::new(code))
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Resolve the language from the APP_LANG environment variable.
    pub fn from_env() -> Self {
        match std::env::var("APP_LANG").ok().as_deref() {
            Some(s) => match Self::parse(s) {
                Some(l) if is_known(l.code) => l,
                _ => {
                    tracing::warn!(
                        "APP_LANG=\"{s}\" is not a known language; defaulting to \"{DEFAULT_LANG}\""
                    );
                    Lang::new(DEFAULT_LANG.to_string())
                }
            },
            None => Lang::new(DEFAULT_LANG.to_string()),
        }
    }

    catalog_accessors!(
        alexa_not_understood,
        alexa_connected,
        alexa_no_queued_track,
        alexa_no_track,
        alexa_no_next,
        alexa_no_prev,
        alexa_help,
        alexa_use_web,
        api_play_queued,
    );

    pub fn api_added_to_playlist(&self, title: &str) -> String {
        t!("api_added_to_playlist", locale = self.locale, title = title).into_owned()
    }

    pub fn api_queued_next(&self, title: &str) -> String {
        t!("api_queued_next", locale = self.locale, title = title).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalogs_load() {
        assert_eq!(
            Lang::parse("en").unwrap().alexa_connected(),
            "Connected to YouTube MultiRoom. You can control playback from the web interface."
        );
        assert_eq!(
            Lang::parse("ja").unwrap().alexa_connected(),
            "YouTube マルチルームに接続しました。Web 画面から操作できます。"
        );
    }

    #[test]
    fn base_code_resolves_via_prefix() {
        assert_eq!(
            Lang::parse("en-us").unwrap().alexa_connected(),
            Lang::parse("en").unwrap().alexa_connected()
        );
    }

    #[test]
    fn unknown_code_falls_back_to_default() {
        let unknown = Lang::parse("xx").unwrap();
        assert_eq!(
            unknown.alexa_connected(),
            Lang::parse("ja").unwrap().alexa_connected()
        );
    }

    #[test]
    fn title_template_is_substituted() {
        assert_eq!(
            Lang::parse("en").unwrap().api_added_to_playlist("Song"),
            "Added \"Song\" to playlist"
        );
    }

    #[test]
    fn equal_codes_share_one_interned_str() {
        let a = Lang::parse("en").unwrap();
        let b = Lang::parse("en").unwrap();
        assert!(std::ptr::eq(a.code, b.code));
    }

    #[test]
    fn empty_code_is_rejected() {
        assert!(Lang::parse("   ").is_none());
    }
}
