//! Internationalization with runtime-loaded message catalogs.
//!
//! Translations live in JSON files under the `locales/` directory, one file per
//! language named after its code, e.g. `en.json`, `ja.json`. Every `*.json` is
//! embedded into the binary at compile time, so the language set is driven
//! entirely by the files present — no language is hard-coded in Rust. The
//! directory is also loaded from disk at startup (path configurable via
//! `LOCALES_DIR`, default `locales/`); files found there override or extend the
//! embedded ones, so a language can be added or fixed with just a file — no
//! recompile needed.
//!
//! [`Lang`] is a lightweight, `Copy` handle holding the requested language
//! code. Its methods resolve the matching catalog at call time, falling back to
//! the default language (Japanese, or the first catalog loaded) when a code has
//! no file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};

use include_dir::{Dir, include_dir};
use serde::Deserialize;

/// The locales directory, embedded at compile time so the binary is
/// self-contained. No language is hard-coded here — every `*.json` file under
/// `locales/` is picked up automatically. At runtime, external files in the
/// configured locales directory override or extend these.
static LOCALES: Dir = include_dir!("$CARGO_MANIFEST_DIR/locales");

/// A message catalog for a single language, deserialized from a JSON file.
///
/// Every key is required, so an incomplete translation file fails fast at
/// startup instead of producing `None` lookups at runtime.
#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    // Alexa speech responses
    pub alexa_not_understood: String,
    pub alexa_connected: String,
    pub alexa_no_queued_track: String,
    pub alexa_no_track: String,
    pub alexa_no_next: String,
    pub alexa_no_prev: String,
    pub alexa_help: String,
    pub alexa_use_web: String,

    // API response messages (the last two are templates; `{title}` is replaced)
    pub api_play_queued: String,
    pub api_added_to_playlist: String,
    pub api_queued_next: String,
}

/// Loaded catalogs, keyed by lowercased file stem (the language code).
static CATALOGS: OnceLock<HashMap<&'static str, Catalog>> = OnceLock::new();

/// The fallback language code used when a requested code has no catalog.
static DEFAULT_LANG: OnceLock<&'static str> = OnceLock::new();

/// The configured fallback language (set by [`init`]), or `"en"` if [`init`]
/// has not run yet. Avoids hard-coding any specific language elsewhere.
fn default_lang() -> &'static str {
    DEFAULT_LANG.get().copied().unwrap_or("en")
}

/// Whether a language code resolves to a loaded catalog (exact or base code).
fn is_known(code: &str) -> bool {
    match CATALOGS.get() {
        Some(catalogs) => {
            let base = code.split('-').next().unwrap_or(code);
            catalogs.contains_key(code) || catalogs.contains_key(base)
        }
        None => false,
    }
}

/// Intern a language code as a `&'static str, leaking each distinct value at
/// most once.
///
/// `Lang` is constructed on every request (via `client_lang`), so storing the
/// code directly with `Box::leak` would leak a fresh allocation per request and
/// grow unbounded. Interning reuses the same `&'static str` for equal codes,
/// bounding the total leak to the number of distinct language codes ever seen.
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

macro_rules! catalog_accessors {
    ($($name:ident),* $(,)?) => {
        $(pub fn $name(&self) -> &'static str {
            &self.catalog().$name
        })*
    };
}

/// A resolved request language.
///
/// Cheap to copy; holds only the language code. The actual strings are looked
/// up from the global catalog registry on each method call.
#[derive(Clone, Copy, Debug)]
pub struct Lang {
    code: &'static str,
}

impl Lang {
    /// Construct a `Lang` from a language code, interning it as `&'static str`.
    fn new(code: String) -> Self {
        Lang {
            code: intern(&code),
        }
    }

    /// Parse a language code such as "en", "en-US", "ja", or "ja-JP".
    /// Always succeeds for a non-empty code; resolution to a catalog happens
    /// later and falls back to the default when no file matches.
    pub fn parse(s: &str) -> Option<Self> {
        let code = s.trim().to_ascii_lowercase();
        if code.is_empty() {
            None
        } else {
            Some(Lang::new(code))
        }
    }

    /// The language code this `Lang` represents, e.g. `"en"` or `"ja"`.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Resolve the language from the APP_LANG environment variable.
    /// Falls back to the default catalog language when APP_LANG is unset or
    /// does not match any loaded catalog, warning in those cases.
    pub fn from_env() -> Self {
        match std::env::var("APP_LANG").ok().as_deref() {
            Some(s) => match Self::parse(s) {
                Some(l) if is_known(l.code) => l,
                _ => {
                    tracing::warn!(
                        "APP_LANG=\"{s}\" is not a known language; defaulting to \"{}\"",
                        default_lang()
                    );
                    Lang::new(default_lang().to_string())
                }
            },
            None => Lang::new(default_lang().to_string()),
        }
    }

    /// Return the catalog for this language, falling back as needed.
    fn catalog(&self) -> &'static Catalog {
        let catalogs = CATALOGS
            .get()
            .expect("i18n: catalogs not initialized; call i18n::init() at startup");
        catalogs
            .get(self.code)
            .or_else(|| {
                let base = self.code.split('-').next().unwrap_or(self.code);
                catalogs.get(base)
            })
            .or_else(|| catalogs.get(default_lang()))
            .or_else(|| catalogs.values().next())
            .expect("i18n: no catalogs loaded")
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
        self.catalog()
            .api_added_to_playlist
            .replace("{title}", title)
    }

    pub fn api_queued_next(&self, title: &str) -> String {
        self.catalog().api_queued_next.replace("{title}", title)
    }
}

/// Load every `*.json` message catalog.
///
/// The `locales/` directory is embedded in the binary, so it works as a single
/// self-contained file with no external assets. Any `*.json` in the locales
/// directory (default `locales/`, override via `LOCALES_DIR`) is loaded on top
/// and takes precedence, letting additional languages be added or existing ones
/// overridden without recompiling. Must be called once at startup, before any
/// [`Lang`] method is used.
pub fn init() -> Result<(), Box<dyn std::error::Error>> {
    let mut catalogs: HashMap<&'static str, Catalog> = HashMap::new();

    // 1. Embedded directory — every *.json is picked up, no language hard-coded.
    for file in LOCALES.files() {
        if file.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = file.path().file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let text = file
            .contents_utf8()
            .ok_or_else(|| format!("locale {} is not valid UTF-8", file.path().display()))?;
        let catalog: Catalog = serde_json::from_str(text).map_err(|e| {
            format!(
                "failed to parse embedded locale {}: {e}",
                file.path().display()
            )
        })?;
        let code: &'static str = Box::leak(stem.to_ascii_lowercase().into_boxed_str());
        catalogs.insert(code, catalog);
    }

    // 2. External files — optional overrides / extensions. A missing directory
    //    is fine; only a present-but-unparseable file is an error.
    let dir = std::env::var("LOCALES_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("locales"));

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let text = std::fs::read_to_string(&path)?;
            let catalog: Catalog = serde_json::from_str(&text)
                .map_err(|e| format!("failed to parse locale file {}: {e}", path.display()))?;
            let code: &'static str = Box::leak(stem.to_ascii_lowercase().into_boxed_str());
            catalogs.insert(code, catalog);
        }
    }

    let default = if catalogs.contains_key("ja") {
        "ja"
    } else {
        catalogs
            .keys()
            .next()
            .copied()
            .expect("i18n: no catalogs loaded")
    };

    CATALOGS
        .set(catalogs)
        .map_err(|_| "i18n: init called more than once")?;
    DEFAULT_LANG
        .set(default)
        .map_err(|_| "i18n: init called more than once")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `init` is process-global (OnceLock), so tolerate the "called more than
    /// once" error on repeated calls across tests.
    fn ensure_init() {
        let _ = init();
    }

    #[test]
    fn embedded_catalogs_load() {
        ensure_init();
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
        ensure_init();
        // "en-US" has no file but must resolve to the "en" catalog.
        assert_eq!(
            Lang::parse("en-US").unwrap().alexa_connected(),
            Lang::parse("en").unwrap().alexa_connected()
        );
    }

    #[test]
    fn unknown_code_falls_back_to_default() {
        ensure_init();
        let unknown = Lang::parse("xx").unwrap();
        assert_eq!(
            unknown.alexa_connected(),
            Lang::parse("ja").unwrap().alexa_connected()
        );
    }

    #[test]
    fn title_template_is_substituted() {
        ensure_init();
        assert_eq!(
            Lang::parse("en").unwrap().api_added_to_playlist("Song"),
            "Added \"Song\" to playlist"
        );
    }

    #[test]
    fn equal_codes_share_one_interned_str() {
        ensure_init();
        let a = Lang::parse("en").unwrap();
        let b = Lang::parse("en").unwrap();
        // Interning must return the same allocation, so per-request Lang
        // construction does not leak a fresh string each time.
        assert!(std::ptr::eq(a.code, b.code));
    }

    #[test]
    fn empty_code_is_rejected() {
        ensure_init();
        assert!(Lang::parse("   ").is_none());
    }
}
