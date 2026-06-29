mod alexa;
mod auth;
mod handlers;
mod state;

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use state::AppState;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let base_url = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "https://your-tunnel-url.ngrok-free.app".to_string());
    let api_token = std::env::var("API_TOKEN").ok().filter(|s| !s.is_empty());

    let state = AppState::new(base_url.clone(), api_token.clone());

    let app = Router::new()
        .route("/api/audio/extract", post(handlers::extract_audio))
        .route("/api/audio/:audio_id/stream", get(handlers::stream_audio))
        .route("/api/tracks", get(handlers::list_tracks))
        .route("/api/devices", get(handlers::get_devices))
        .route("/api/play", post(handlers::play_on_devices))
        .route("/api/play-all", post(handlers::play_on_all))
        .route("/api/devices/:device_id/stop", post(handlers::stop_device))
        .route("/alexa", post(handlers::alexa_webhook))
        .route("/ws", get(handlers::ws_upgrade))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .fallback_service(ServeDir::new("static"))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8888));

    println!("══════════════════════════════════════════");
    println!("  YouTube MultiRoom Server (Rust)");
    println!("  BASE_URL = {base_url}");
    println!("  Web UI   → http://localhost:8888");
    println!("  Alexa    → POST {{BASE_URL}}/alexa");
    if api_token.is_some() {
        println!("  Auth     → API_TOKEN is set");
    } else {
        println!("  Auth     → disabled (set API_TOKEN to enable)");
    }
    println!("══════════════════════════════════════════");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
}
