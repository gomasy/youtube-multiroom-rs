# YouTube MultiRoom

A Spotify Connect-style system for simultaneously playing YouTube audio on multiple Amazon Echo Dot devices.
Built with axum + tokio (backend) and React + TypeScript (frontend).

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
├── front/
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       ├── index.html
│       ├── index.tsx
│       ├── App.tsx
│       ├── api.ts         # Auth-aware fetch wrapper
│       ├── hooks.ts       # WebSocket hook
│       ├── types.ts       # Shared type definitions
│       ├── styles.css
│       └── components/
│           ├── AuthModal.tsx
│           ├── DeviceList.tsx
│           ├── Header.tsx
│           ├── History.tsx
│           ├── NowPlaying.tsx
│           ├── Toast.tsx
│           └── UrlInput.tsx
├── alexa_interaction_model.json
└── README.md
```

## Build & Run

### Prerequisites

- Rust 1.75+
- Node.js 18+
- Redis
- yt-dlp
- ffmpeg
- A tunnel to expose the server (e.g. ngrok, Cloudflare Tunnel, Tailscale Funnel)

### Build

```bash
# Frontend
cd front && npm install && npm run build && cd ..

# Backend
cargo build --release
```

### Environment Variables

| Variable | Required | Description |
|---|---|---|
| `REDIS_URL` | Yes | Redis connection URL (e.g. `redis://127.0.0.1/`) |
| `API_TOKEN` | No | Bearer token for API authentication |

### Run

```bash
# Create a tunnel in a separate terminal (e.g. ngrok)
ngrok http 8888

# Start the server
REDIS_URL=redis://127.0.0.1/ ./target/release/youtube-multiroom
```

Access the Web UI at `http://localhost:8888`.

### Development

```bash
cd front
npm run dev   # Runs both cargo run and parcel watch via concurrently
```

### Authentication

You can protect the API with a Bearer token by setting the `API_TOKEN` environment variable:

```bash
REDIS_URL=redis://127.0.0.1/ API_TOKEN=your-secret-token ./target/release/youtube-multiroom
```

When enabled:
- The Web UI prompts for the token on first access (stored in localStorage)
- API endpoints and WebSocket require `Authorization: Bearer <token>` (or `?token=` query param for WebSocket)
- `/alexa` and `/api/audio/:id/stream` are excluded from authentication since Alexa accesses them directly

If `API_TOKEN` is not set, no authentication is required.

### Cross-compilation for Raspberry Pi

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/youtube-multiroom pi@raspberrypi:~/
```

The binary, `front/dist/`, `yt-dlp`, and `ffmpeg` are needed on the Pi.

## Alexa Skill Setup

1. Create a custom skill on the [Alexa Developer Console](https://developer.amazon.com/alexa/console/ask)
2. Invocation name: `youtube プレーヤー`
3. Interaction Model > JSON Editor: paste `alexa_interaction_model.json`
4. Interfaces > Enable **Audio Player**
5. Endpoint > HTTPS > `https://<your-tunnel-url>/alexa`
6. Test > Set to **Development**

## Usage

1. Open `http://localhost:8888`
2. Paste a YouTube URL (auto-extracts on paste)
3. Say to your Echo: **「アレクサ、YouTube プレーヤーを開いて」**
4. Select devices in the Web UI and click play
5. Say to each Echo again: **「アレクサ、YouTube プレーヤーを開いて」** to start playback

## Architecture

```
    AppState (Arc)
    ├── redis:   ConnectionManager  # track metadata (Redis hash)
    ├── devices: RwLock<HashMap>    # connected Echo devices
    ├── pending: RwLock<HashMap>    # queued play commands
    └── tx: broadcast::Sender      # real-time sync
         │
    ┌────┴─────────────────────────────────────────────────┐
    │  axum Router                                         │
    ├──────────────────────────────────────────────────────┤
    │  GET    /api/audio/:id/stream   MP3 streaming        │
    │  GET    /api/tracks             track list            │
    │  DELETE /api/tracks/:id         delete track          │
    │  GET    /api/devices            device list           │
    │  DELETE /api/devices/:id        delete device         │
    │  POST   /api/play              queue to devices       │
    │  POST   /api/play-all          queue to all           │
    │  POST   /api/devices/:id/stop  stop device            │
    │  POST   /alexa                 Alexa webhook          │
    │  WS     /ws                    real-time sync         │
    │  GET    /*                     front/dist static      │
    └──────────────────────────────────────────────────────┘
```

Audio extraction is handled via WebSocket to avoid reverse proxy read timeouts.
The client sends `{ "type": "extract_audio", "url": "..." }` and receives `extract_audio_result` or `extract_audio_error`.

State changes are broadcast to all WebSocket clients via `tokio::sync::broadcast`:
- `device_update` — device status, track assignment, connection changes
- `tracks_update` — track extraction, deletion

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
Environment=REDIS_URL=redis://127.0.0.1/
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
| GET | `/api/audio/:id/stream` | No | Stream MP3 audio (supports Range requests) |
| GET | `/api/tracks` | Yes | List extracted tracks |
| DELETE | `/api/tracks/:id` | Yes | Delete a track and its cached file |
| GET | `/api/devices` | Yes | List connected devices |
| DELETE | `/api/devices/:id` | Yes | Delete a device |
| POST | `/api/play` | Yes | Queue playback on selected devices |
| POST | `/api/play-all` | Yes | Queue playback on all devices |
| POST | `/api/devices/:id/stop` | Yes | Stop a device |
| POST | `/alexa` | No | Alexa skill webhook |
| WS | `/ws` | Yes | Real-time sync & audio extraction |

## License

[MIT](LICENSE)
