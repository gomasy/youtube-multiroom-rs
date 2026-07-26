mod alexa;
mod alexa_verify;
mod auth;
mod handlers;
mod state;

rust_i18n::i18n!("locales", fallback = "ja");

/// Resolve a language code to the best available locale, or `None` if unknown.
fn resolve_locale(code: &str) -> Option<String> {
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

pub const VERSION: &str = concat!(
    "v",
    env!("CARGO_PKG_VERSION"),
    "-",
    env!("GIT_HASH"),
    " (",
    env!("BUILD_DATE"),
    ")",
);

use axum::Router;
use axum::middleware;
use axum::routing::{delete, get, patch, post};
use state::AppState;
use std::net::SocketAddr;
use std::process;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dotenv_loaded = dotenvy::dotenv().is_ok();

    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let api_token = std::env::var("API_TOKEN").ok().filter(|s| !s.is_empty());
    let auth_enabled = api_token.is_some();
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| die("REDIS_URL must be set"));
    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8888".to_string());
    let addr: SocketAddr = listen_addr.parse().unwrap_or_else(|_| {
        die(format!(
            "LISTEN_ADDR is not a valid socket address: {listen_addr}"
        ))
    });

    // Do not swap this for a hardcoded default: the i18n! macro's constant is
    // the single source of truth for the fallback locale. Reported through die()
    // like every other fatal startup condition rather than as a panic.
    let fallback = _RUST_I18N_FALLBACK_LOCALE
        .and_then(|l| l.first())
        .unwrap_or_else(|| die("i18n fallback locale is not set"))
        .to_string();
    let (locale, locale_src) = match std::env::var("APP_LANG")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(s) => {
            let resolved = resolve_locale(&s).unwrap_or_else(|| {
                tracing::warn!(
                    "APP_LANG=\"{s}\" is not a known language; defaulting to \"{fallback}\""
                );
                fallback.clone()
            });
            (resolved, format!("APP_LANG={s}"))
        }
        None => (fallback, "default".to_string()),
    };

    let state = AppState::new(api_token, &redis_url, locale.clone())
        .await
        .unwrap_or_else(|e| die(e));

    state.restore_sleep_timer().await;

    let app = Router::new()
        .route("/api/audio/{audio_id}/stream", get(handlers::stream_audio))
        .route("/api/audio/{audio_id}/live", get(handlers::live_audio))
        .route("/api/audio/{audio_id}/url", get(handlers::audio_url))
        .route("/api/search", get(handlers::search_youtube))
        .route("/api/tracks", get(handlers::list_tracks))
        .route("/api/tracks/reorder", post(handlers::reorder_track))
        .route(
            "/api/tracks/bulk-delete",
            post(handlers::bulk_delete_tracks),
        )
        .route("/api/tracks/{track_id}", delete(handlers::delete_track))
        .route(
            "/api/playlists",
            get(handlers::list_playlists).post(handlers::create_playlist),
        )
        .route(
            "/api/playlists/{playlist_id}",
            patch(handlers::rename_playlist).delete(handlers::delete_playlist),
        )
        .route(
            "/api/playlists/{playlist_id}/tracks",
            post(handlers::add_playlist_track),
        )
        .route(
            "/api/playlists/{playlist_id}/tracks/bulk",
            post(handlers::bulk_add_playlist_tracks),
        )
        .route(
            "/api/playlists/{playlist_id}/tracks/{track_id}",
            delete(handlers::remove_playlist_track),
        )
        .route("/api/devices", get(handlers::get_devices))
        .route("/api/devices/{device_id}", delete(handlers::delete_device))
        .route("/api/play", post(handlers::play_on_devices))
        .route("/api/play-all", post(handlers::play_on_all))
        .route("/api/queue", post(handlers::queue_next))
        .route(
            "/api/devices/{device_id}/queue",
            delete(handlers::clear_queue),
        )
        .route(
            "/api/devices/{device_id}/queue/{entry}",
            delete(handlers::remove_queue_item),
        )
        .route("/api/devices/{device_id}/seek", post(handlers::seek_device))
        .route("/api/devices/{device_id}/stop", post(handlers::stop_device))
        .route("/alexa", post(handlers::alexa_webhook))
        .route(auth::WS_PATH, get(handlers::ws_upgrade))
        // route_layer only wraps the routes registered above it, which is what
        // makes the two static services below deliberately public: the browser
        // must be able to load the app shell and its message catalogs in order
        // to show the login prompt that obtains a token in the first place.
        // Anything that touches user data must be registered above this layer.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .nest_service("/locales", ServeDir::new("front/locales"))
        .fallback_service(ServeDir::new("front/dist"))
        .with_state(state);

    println!("══════════════════════════════════════════");
    println!("  YouTube MultiRoom Server {VERSION}");
    if dotenv_loaded {
        println!("  Config   → loaded .env");
    }
    println!("  Redis    = {}", redact_url(&redis_url));
    println!("  Web UI   → http://localhost:{}", addr.port());
    println!("  Alexa    → POST /alexa");
    println!("  Lang     → {} ({})", locale, locale_src);
    if auth_enabled {
        println!("  Auth     → API_TOKEN is set");
    } else {
        println!("  Auth     → disabled (set API_TOKEN to enable)");
    }
    println!("══════════════════════════════════════════");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    // Registration failure degrades to Ctrl-C rather than taking the process down.
    // `None` then leaves the pattern below unmatched, which disables that branch.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .inspect_err(|e| tracing::warn!("Cannot listen for SIGTERM: {e}; Ctrl-C only"))
        .ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        Some(_) = async { Some(sigterm.as_mut()?.recv().await) } => {}
    }
    tracing::info!("Shutting down...");
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("Error: {msg}");
    process::exit(1);
}

/// Redact user credentials from a URL for log output.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}{path}"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::redact_url;
    use super::resolve_locale;
    use rust_i18n::t;

    #[test]
    fn redacts_userinfo_only() {
        assert_eq!(
            redact_url("redis://user:pass@localhost:6379/0"),
            "redis://***@localhost:6379/0"
        );
        assert_eq!(redact_url("redis://localhost/"), "redis://localhost/");
    }

    #[test]
    fn resolve_exact_match() {
        assert_eq!(resolve_locale("en"), Some("en".to_string()));
        assert_eq!(resolve_locale("ja"), Some("ja".to_string()));
    }

    #[test]
    fn resolve_base_code_fallback() {
        assert_eq!(resolve_locale("en-us"), Some("en".to_string()));
        assert_eq!(resolve_locale("en-US"), Some("en".to_string()));
        assert_eq!(resolve_locale("ja-JP"), Some("ja".to_string()));
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert_eq!(resolve_locale("xx"), None);
        assert_eq!(resolve_locale("xx-YY"), None);
    }

    #[test]
    fn resolve_empty_returns_none() {
        assert_eq!(resolve_locale(""), None);
        assert_eq!(resolve_locale("   "), None);
    }

    #[test]
    fn fallback_locale_is_ja() {
        let fallback = super::_RUST_I18N_FALLBACK_LOCALE
            .and_then(|l| l.first())
            .copied();
        assert_eq!(fallback, Some("ja"));
    }

    #[test]
    fn embedded_translations_load() {
        assert_eq!(
            t!("alexa_connected", locale = "en"),
            "Connected to YouTube MultiRoom. You can control playback from the web interface."
        );
        assert_eq!(
            t!("alexa_connected", locale = "ja"),
            "YouTube マルチルームに接続しました。Web 画面から操作できます。"
        );
    }

    #[test]
    fn template_substitution_works() {
        assert_eq!(
            t!("api_added_to_playlist", locale = "en", title = "Song"),
            "Added \"Song\" to playlist"
        );
    }
}
