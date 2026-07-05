mod alexa;
mod auth;
mod handlers;
mod state;

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use state::AppState;
use std::net::SocketAddr;
use std::process;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let api_token = std::env::var("API_TOKEN").ok().filter(|s| !s.is_empty());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| {
        eprintln!("Error: REDIS_URL must be set");
        process::exit(1);
    });

    let state = match AppState::new(api_token.clone(), &redis_url).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            process::exit(1);
        }
    };

    let app = Router::new()
        .route("/api/audio/:audio_id/stream", get(handlers::stream_audio))
        .route("/api/audio/:audio_id/live", get(handlers::live_audio))
        .route("/api/tracks", get(handlers::list_tracks))
        .route("/api/tracks/reorder", post(handlers::reorder_track))
        .route("/api/tracks/:track_id", delete(handlers::delete_track))
        .route("/api/devices", get(handlers::get_devices))
        .route("/api/devices/:device_id", delete(handlers::delete_device))
        .route("/api/play", post(handlers::play_on_devices))
        .route("/api/play-all", post(handlers::play_on_all))
        .route("/api/devices/:device_id/stop", post(handlers::stop_device))
        .route("/alexa", post(handlers::alexa_webhook))
        .route("/ws", get(handlers::ws_upgrade))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .fallback_service(ServeDir::new("front/dist"))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8888));

    println!("══════════════════════════════════════════");
    println!("  YouTube MultiRoom Server (Rust)");
    println!("  Redis    = {redis_url}");
    println!("  Web UI   → http://localhost:8888");
    println!("  Alexa    → POST /alexa");
    if api_token.is_some() {
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
