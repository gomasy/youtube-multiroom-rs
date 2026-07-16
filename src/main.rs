mod alexa;
mod alexa_verify;
mod auth;
mod handlers;
mod state;

use axum::Router;
use axum::middleware;
use axum::routing::{delete, get, post};
use state::AppState;
use std::net::SocketAddr;
use std::process;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // .env があれば読み込む (無くてもエラーにしない)
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

    let app = Router::new()
        .route("/api/audio/{audio_id}/stream", get(handlers::stream_audio))
        .route("/api/audio/{audio_id}/live", get(handlers::live_audio))
        .route("/api/search", get(handlers::search_youtube))
        .route("/api/tracks", get(handlers::list_tracks))
        .route("/api/tracks/reorder", post(handlers::reorder_track))
        .route("/api/tracks/{track_id}", delete(handlers::delete_track))
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
        .route("/ws", get(handlers::ws_upgrade))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .fallback_service(ServeDir::new("front/dist"))
        .with_state(state);

    println!("══════════════════════════════════════════");
    println!("  YouTube MultiRoom Server (Rust)");
    if dotenv_loaded {
        println!("  Config   → loaded .env");
    }
    println!("  Redis    = {}", redact_url(&redis_url));
    println!("  Web UI   → http://localhost:{}", addr.port());
    println!("  Alexa    → POST /alexa");
    if auth_enabled {
        println!("  Auth     → API_TOKEN is set");
    } else {
        println!("  Auth     → disabled (set API_TOKEN to enable)");
    }
    println!("══════════════════════════════════════════");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

/// 起動に必須の設定が欠けている場合にエラーを表示して終了する
fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("Error: {msg}");
    process::exit(1);
}

/// URL のユーザー情報 (user:password@) を伏せてログ用に整形する
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

    #[test]
    fn redacts_userinfo_only() {
        assert_eq!(
            redact_url("redis://user:pass@localhost:6379/0"),
            "redis://***@localhost:6379/0"
        );
        assert_eq!(redact_url("redis://localhost/"), "redis://localhost/");
    }
}
