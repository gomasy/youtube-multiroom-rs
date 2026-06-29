# YouTube MultiRoom (Rust)

A Spotify Connect-style system for simultaneously playing YouTube audio on multiple Amazon Echo Dot devices.
Rewritten from the Python version using axum + tokio.

## Differences from the Python Version

| | Python (FastAPI) | Rust (axum) |
|---|---|---|
| Startup time | ~1s | ~50ms |
| Memory usage | ~60MB | ~5MB |
| Distribution | Requires Python + pip | Single binary |
| Async runtime | asyncio | tokio (multi-threaded) |
| WebSocket | Manual Set management | broadcast channel |
| Type safety | Runtime errors | Compile-time guarantees |

## Project Structure

```
youtube-multiroom-rs/
├── Cargo.toml
├── src/
│   ├── main.rs        # Entry point & router
│   ├── state.rs       # Shared state, audio & device management
│   ├── handlers.rs    # HTTP / WebSocket handlers
│   ├── auth.rs        # Bearer token authentication middleware
│   └── alexa.rs       # Alexa skill handler
├── static/
│   └── index.html     # Web UI (Spotify Connect-style)
├── alexa_interaction_model.json
└── README.md
```

## Build & Run

### Prerequisites

- Rust 1.75+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- yt-dlp (`pip install yt-dlp` / `brew install yt-dlp`)
- ngrok or Cloudflare Tunnel

### Build

```bash
# Dev build (fast compilation)
cargo build

# Release build (optimized + LTO)
cargo build --release
```

### Run

```bash
# Create a tunnel in a separate terminal
ngrok http 8888

# Start the server
BASE_URL=https://xxxx.ngrok-free.app cargo run --release

# Or run the binary directly
BASE_URL=https://xxxx.ngrok-free.app ./target/release/youtube-multiroom
```

Access the Web UI at `http://localhost:8888`.

### Authentication

Since the server is exposed to the internet via a tunnel, you can enable Bearer token authentication by setting the `API_TOKEN` environment variable:

```bash
API_TOKEN=your-secret-token BASE_URL=https://xxxx.ngrok-free.app cargo run --release
```

When enabled:
- The Web UI prompts for the token on first access (stored in localStorage)
- All API endpoints and WebSocket require `Authorization: Bearer <token>`
- `/alexa` and `/api/audio/{id}/stream` are excluded (accessed directly by Alexa)

If `API_TOKEN` is not set, all endpoints are accessible without authentication (same as before).

### Cross-compilation for Raspberry Pi

```bash
# Add target
rustup target add armv7-unknown-linux-gnueabihf   # Pi 3/4 (32-bit)
rustup target add aarch64-unknown-linux-gnu        # Pi 4/5 (64-bit)

# Cross-compile
cargo build --release --target aarch64-unknown-linux-gnu

# Transfer to Pi
scp target/aarch64-unknown-linux-gnu/release/youtube-multiroom pi@raspberrypi:~/
```

Only the single binary + `static/` folder + `yt-dlp` are needed on the Pi.

## Alexa Skill Setup

1. Create a custom skill on the [Alexa Developer Console](https://developer.amazon.com/alexa/console/ask)
2. Invocation name: `youtube player`
3. Interaction Model > JSON Editor: paste `alexa_interaction_model.json`
4. Interfaces > Enable **Audio Player**
5. Endpoint > HTTPS > `https://xxxx.ngrok-free.app/alexa`
6. Test > Set to **Development**

## Usage

1. Open `http://localhost:8888`
2. Paste a YouTube URL and click **Extract**
3. Say to your Echo: **"Alexa, open YouTube player"**
4. Select devices in the Web UI and click **Play**
5. Say to each Echo again: **"Alexa, open YouTube player"** to start playback

## Architecture

```
         +-- broadcast::Sender --+
         |                       |
    AppState (Arc)               |
    |-- tracks: RwLock<HashMap>  |
    |-- devices: RwLock<HashMap> |
    |-- pending: RwLock<HashMap> |
    +-- tx ----------------------+
         |
    +----+----+
    |  axum   |
    |  Router |
    +---------+
    | GET  /api/audio/:id/stream   -> Audio streaming
    | POST /api/audio/extract      -> Run yt-dlp
    | POST /api/play               -> Queue to devices
    | POST /alexa                  -> Alexa Webhook
    | WS   /ws                     -> Real-time sync
    | GET  /*                      -> static/ (Web UI)
    +---------+
```

**WebSocket sync:**
Uses `tokio::sync::broadcast` channel.
Whenever device state changes, `broadcast_devices()` sends to the channel,
and all WebSocket clients receive updates via a `select!` loop.

## systemd Service

```ini
# /etc/systemd/system/yt-multiroom.service
[Unit]
Description=YouTube MultiRoom
After=network.target

[Service]
Type=simple
User=pi
WorkingDirectory=/home/pi/youtube-multiroom
Environment=BASE_URL=https://your-fixed-tunnel-url
Environment=API_TOKEN=your-secret-token
ExecStart=/home/pi/youtube-multiroom/youtube-multiroom
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now yt-multiroom
```

## API Reference

| Method | Path | Auth | Description |
|---|---|---|---|
| POST | `/api/audio/extract` | Yes | YouTube URL -> extract audio |
| GET | `/api/audio/{id}/stream` | No | MP3 streaming |
| GET | `/api/tracks` | Yes | List extracted tracks |
| GET | `/api/devices` | Yes | List devices |
| POST | `/api/play` | Yes | Queue playback on selected devices |
| POST | `/api/play-all` | Yes | Queue playback on all devices |
| POST | `/api/devices/{id}/stop` | Yes | Stop a device |
| POST | `/alexa` | No | Alexa Webhook |
| WS | `/ws` | Yes | Real-time sync |
