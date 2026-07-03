# YouTube MultiRoom

A Spotify Connect-style system for simultaneously playing YouTube audio on multiple Amazon Echo Dot devices.
Built with axum + tokio (backend) and React + TypeScript (frontend).

## Project Structure

```
youtube-multiroom-rs/
├── Cargo.toml
├── Dockerfile
├── .github/
│   └── workflows/
│       └── build-image.yml   # Container image build (ghcr.io)
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
│           ├── PlaybackModeSelector.tsx
│           ├── ScrollingText.tsx
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
- `/api/audio/:id/stream` requires a signed URL: stream URLs handed to Alexa carry an HMAC-SHA256 signature (`?exp=<unix>&sig=<hmac>`, derived from `API_TOKEN`, valid for 24h) since Echo devices cannot send auth headers. Bearer auth is also accepted
- `/alexa` is excluded from authentication since Alexa accesses it directly

If `API_TOKEN` is not set, no authentication is required.

### Docker

A multi-arch (amd64/arm64) container image is built by GitHub Actions and published to GHCR.
The image bundles `yt-dlp`, `ffmpeg`, and `deno`; only Redis is needed externally.

```bash
docker run -d -p 8888:8888 \
  -e REDIS_URL=redis://<redis-host>/ \
  -e API_TOKEN=your-secret-token \
  ghcr.io/gomasy/youtube-multiroom-rs
```

To build locally: `docker build -t youtube-multiroom .`

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
6. Optionally pick a playback mode (off / loop / shuffle) in the Web UI to auto-play the next track

## Architecture

```
    AppState (Arc)
    ├── redis: ConnectionManager    # all persistent state
    │    ├── youtube:tracks                # track metadata (hash)
    │    ├── youtube:devices               # Echo device states (hash)
    │    ├── youtube:playback_mode         # auto-play mode ("off" | "loop" | "shuffle")
    │    └── youtube:pending:{device_id}   # queued play command (10 min TTL)
    └── tx: broadcast::Sender      # real-time sync
         │
    ┌────┴─────────────────────────────────────────────────┐
    │  axum Router                                         │
    ├──────────────────────────────────────────────────────┤
    │  GET    /api/audio/:id/stream   MP3 streaming        │
    │  GET    /api/tracks             track list (paged)    │
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

All state lives in Redis, so tracks, devices, and queued play commands survive server restarts.
Queued play commands are stored per device with a native Redis TTL (10 minutes) and consumed atomically via `GETDEL`.

### Playback Modes

Auto-play behavior when a track finishes is controlled by a global playback mode (selectable in the Web UI, persisted in Redis, default `off`):

- `off` — stop after the current track
- `loop` — continue with the next track in library order (newest first), wrapping to the top
- `shuffle` — continue with a random track other than the current one

On Alexa's `AudioPlayer.PlaybackNearlyFinished` event, the server picks the next track according to the mode and enqueues it via an `ENQUEUE` directive.

### WebSocket Protocol

Audio extraction is handled via WebSocket to avoid reverse proxy read timeouts.
The client sends `{ "type": "extract_audio", "url": "..." }` and receives `extract_audio_result` or `extract_audio_error`.
The client can also send `{ "type": "set_playback_mode", "mode": "off" | "loop" | "shuffle" }` and `{ "type": "ping" }` (answered with `pong`).

On connect, the server sends an `init` message containing the current device map and playback mode
(the track list is fetched separately via `GET /api/tracks`).

State changes are broadcast to all WebSocket clients via `tokio::sync::broadcast`:
- `device_update` — device status, track assignment, connection changes (full device map)
- `tracks_update` — notification that the track list changed; clients refetch their current page via `GET /api/tracks`
- `playback_mode_update` — the playback mode was changed by a client

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
| GET | `/api/audio/:id/stream` | Signed URL | Stream MP3 audio (supports Range requests) |
| GET | `/api/tracks` | Yes | List extracted tracks, newest first (paginated) |
| DELETE | `/api/tracks/:id` | Yes | Delete a track and its cached file |
| GET | `/api/devices` | Yes | List connected devices |
| DELETE | `/api/devices/:id` | Yes | Delete a device |
| POST | `/api/play` | Yes | Queue playback on selected devices |
| POST | `/api/play-all` | Yes | Queue playback on all devices |
| POST | `/api/devices/:id/stop` | Yes | Stop a device |
| POST | `/alexa` | No | Alexa skill webhook |
| WS | `/ws` | Yes | Real-time sync & audio extraction |

`GET /api/tracks` accepts `page` (default 1) and `per_page` (default 10, max 100) query parameters and returns:

```json
{ "tracks": [ ... ], "total": 42, "page": 1, "per_page": 10 }
```

## License

[MIT](LICENSE)
