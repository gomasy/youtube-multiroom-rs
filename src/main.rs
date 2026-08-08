mod alexa;
mod auth;
mod handlers;
mod locale;
mod state;
mod static_files;

rust_i18n::i18n!("locales", fallback = "en");

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

    let state = AppState::new(api_token, &redis_url)
        .await
        .unwrap_or_else(|e| die(e));

    state.clear_download_staging().await;
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
        .route(
            "/api/tracks/refresh-metadata",
            post(handlers::refresh_tracks_metadata),
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
            "/api/playlists/{playlist_id}/tracks/bulk-remove",
            post(handlers::bulk_remove_playlist_tracks),
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
        .route(
            "/api/devices/{device_id}/sync",
            post(handlers::sync_devices),
        )
        .route("/api/devices/{device_id}/stop", post(handlers::stop_device))
        .route("/api/cache", get(handlers::cache_status))
        .route("/api/cache/cleanup", post(handlers::cleanup_cache))
        .route("/api/cache/repair", post(handlers::repair_cache))
        .route("/api/library/export", get(handlers::export_library))
        .route("/api/library/import", post(handlers::import_library))
        .route("/alexa", post(handlers::alexa_webhook))
        .route(auth::WS_PATH, get(handlers::ws_upgrade))
        // route_layer only wraps the routes registered above it, which is what
        // makes the static files merged below deliberately public: the browser
        // must be able to load the app shell and its message catalogs in order
        // to show the login prompt that obtains a token in the first place.
        // Anything that touches user data must be registered above this layer.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .merge(static_files::router())
        .with_state(state);

    println!("══════════════════════════════════════════");
    println!("  YouTube MultiRoom Server {VERSION}");
    if dotenv_loaded {
        println!("  Config   → loaded .env");
    }
    println!("  Redis    = {}", redact_url(&redis_url));
    println!("  Web UI   → http://localhost:{}", addr.port());
    println!("  Alexa    → POST /alexa");
    // Also the first read of ALEXA_SKILL_ID. Printing the value rather than
    // just "is set" is what turns a typo into something visible at startup
    // instead of into an Echo that has silently stopped being answered.
    match alexa::skill_id() {
        Some(id) => println!("  Skill ID = {id}"),
        None => println!("  Skill ID → any (set ALEXA_SKILL_ID to restrict)"),
    }
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

pub(crate) fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("Error: {msg}");
    process::exit(1);
}

/// Redact user credentials from a URL for log output. Every path that shows a
/// connection URL to a human goes through here — the startup banner and
/// AppState::new's connection error alike — since a Redis URL routinely
/// carries a password.
pub(crate) fn redact_url(url: &str) -> String {
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

    #[test]
    fn redacts_userinfo_only() {
        assert_eq!(
            redact_url("redis://user:pass@localhost:6379/0"),
            "redis://***@localhost:6379/0"
        );
        // Redis URLs usually carry the password with no username at all
        assert_eq!(
            redact_url("redis://:pass@localhost:6379/0"),
            "redis://***@localhost:6379/0"
        );
        // Neither a missing path nor a TLS scheme may let credentials through
        assert_eq!(
            redact_url("rediss://user:pass@localhost:6379"),
            "rediss://***@localhost:6379"
        );
        assert_eq!(redact_url("redis://localhost/"), "redis://localhost/");
    }
}
