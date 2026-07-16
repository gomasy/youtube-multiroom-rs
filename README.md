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
│   ├── alexa.rs       # Alexa skill handler
│   └── alexa_verify.rs # Alexa request signature verification
├── front/
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       ├── favicon.svg
│       ├── index.html
│       ├── index.tsx
│       ├── App.tsx
│       ├── api.ts         # Auth-aware fetch wrapper
│       ├── format.ts      # Shared time/duration formatters
│       ├── hooks.ts       # WebSocket hook
│       ├── types.ts       # Shared type definitions
│       ├── parcel-env.d.ts # Ambient types for Parcel-specific imports
│       ├── styles.css
│       └── components/
│           ├── AuthModal.tsx
│           ├── DeviceList.tsx
│           ├── Header.tsx
│           ├── History.tsx
│           ├── NowPlaying.tsx
│           ├── PlaybackModeSelector.tsx
│           ├── ScrollingText.tsx
│           ├── SeekBar.tsx
│           ├── Toast.tsx
│           └── UrlInput.tsx
├── alexa_interaction_model.json
└── README.md
```

## Build & Run

### Prerequisites

- Rust 1.88+
- OpenSSL headers & pkg-config (build only; `libssl-dev` on Debian/Ubuntu, used for Alexa request signature verification)
- Node.js 22+
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
| `LISTEN_ADDR` | No | Address and port to listen on (default: `0.0.0.0:8888`) |

Variables can also be placed in a `.env` file in the working directory (loaded automatically at startup; real environment variables take precedence). See `.env.example`.

```bash
cp .env.example .env
# then edit .env
```

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
- `/api/audio/{id}/stream` and `/api/audio/{id}/live` require a signed URL: stream URLs handed to Alexa carry an HMAC-SHA256 signature (`?exp=<unix>&sig=<hmac>`, derived from `API_TOKEN`, valid for 24h) since Echo devices cannot send auth headers. Bearer auth is also accepted
- `/alexa` is excluded from Bearer authentication since Alexa accesses it directly; instead, every request to it is verified as genuinely coming from Alexa via Amazon's request signature scheme (certificate chain validation + body signature + timestamp freshness), regardless of whether `API_TOKEN` is set. Note this means you cannot `curl` the `/alexa` endpoint manually

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
7. Drag the grip handle (⋮⋮) on a track row to rearrange the library order (also used by loop playback); hold the drag over the prev/next pagination button to flip pages and drop the track on another page

## Architecture

```
    AppState (Arc)
    ├── redis: ConnectionManager    # all persistent state
    │    ├── youtube:tracks                # track metadata (hash)
    │    ├── youtube:tracks_order          # track display/playback order (list)
    │    ├── youtube:devices               # Echo device states (hash)
    │    ├── youtube:playback_mode         # auto-play mode ("off" | "loop" | "shuffle")
    │    └── youtube:pending:{device_id}   # queued play command (10 min TTL)
    └── tx: broadcast::Sender      # real-time sync
         │
    ┌────┴─────────────────────────────────────────────────┐
    │  axum Router                                         │
    ├──────────────────────────────────────────────────────┤
    │  GET    /api/audio/{id}/stream  m4a streaming        │
    │  GET    /api/audio/{id}/live    live audio relay      │
    │  GET    /api/tracks             track list (paged)    │
    │  POST   /api/tracks/reorder     move a track          │
    │  DELETE /api/tracks/{id}        delete track          │
    │  GET    /api/devices            device list           │
    │  DELETE /api/devices/{id}       delete device         │
    │  POST   /api/play              queue to devices       │
    │  POST   /api/play-all          queue to all           │
    │  POST   /api/devices/{id}/seek queue seek              │
    │  POST   /api/devices/{id}/stop stop device            │
    │  POST   /alexa                 Alexa webhook          │
    │  WS     /ws                    real-time sync         │
    │  GET    /*                     front/dist static      │
    └──────────────────────────────────────────────────────┘
```

All state lives in Redis, so tracks, devices, and queued play commands survive server restarts.
Queued play commands are stored per device with a native Redis TTL (10 minutes) and consumed atomically via `GETDEL`.

If the track metadata hash is ever lost (e.g. Redis was wiped), the next `GET /api/tracks` detects the missing key and rebuilds it in the background from the m4a filenames in `audio_cache/`, re-fetching metadata via yt-dlp (file mtime is used as the registration time to preserve ordering; a `tracks_update` is broadcast when done). The custom track order itself cannot be recovered this way — tracks fall back to newest-first.

### Track Ordering

Tracks are listed and auto-played in a user-defined order persisted in the `youtube:tracks_order` Redis list. Rows in the Web UI can be rearranged by dragging the grip handle (works with both mouse and touch via Pointer Events). Reordering works across pages: hovering the prev/next pagination button mid-drag auto-flips pages (one page per 650 ms) so the track can be dropped anywhere in the library. Newly extracted tracks are placed at the top; tracks not present in the order list (data from before this feature) are appended newest-first.

### Live Streams

YouTube Live streams (including `youtube.com/live/<id>` URLs) can be added like regular videos. Since a live stream cannot be cached as a file, only its metadata is stored (with an `is_live` flag) and the Web UI shows a red **LIVE** badge in place of the duration.

At playback time, `GET /api/audio/:id/live` resolves a fresh CDN HLS URL via yt-dlp (preferring audio-only HLS, falling back to the lowest-bitrate muxed HLS since live streams often lack audio-only formats) and relays the audio to the Echo as an ADTS AAC stream extracted by ffmpeg. When the source audio is already AAC the codec is copied without re-encoding, so CPU usage is minimal; if the fallback format carries a non-AAC codec (e.g. Opus, which cannot be wrapped in ADTS), it is transcoded to AAC instead. When the Echo disconnects, the pipe closes and ffmpeg exits on its own.

Caveats:

- Live tracks are not recoverable by the `audio_cache/` scan described above — if Redis is wiped, re-add them manually
- A track added while live keeps its `is_live` flag even after the broadcast ends; delete and re-add it to cache the archived video
- Playback starts a few segments behind the live edge (typical HLS latency)

### Playback Modes

Auto-play behavior when a track finishes is controlled by a global playback mode (selectable in the Web UI, persisted in Redis, default `off`):

- `off` — stop after the current track
- `loop` — continue with the next track in library order (see Track Ordering above), wrapping to the top
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
| GET | `/api/audio/{id}/stream` | Signed URL | Stream m4a audio (supports Range requests) |
| GET | `/api/audio/{id}/live` | Signed URL | Relay live stream audio as ADTS AAC via ffmpeg |
| GET | `/api/tracks` | Yes | List extracted tracks in library order (paginated) |
| POST | `/api/tracks/reorder` | Yes | Move a track within the library order |
| DELETE | `/api/tracks/{id}` | Yes | Delete a track and its cached file |
| GET | `/api/devices` | Yes | List connected devices |
| DELETE | `/api/devices/{id}` | Yes | Delete a device |
| POST | `/api/play` | Yes | Queue playback on selected devices |
| POST | `/api/play-all` | Yes | Queue playback on all devices |
| POST | `/api/devices/{id}/seek` | Yes | Queue playback of the device's current track from a position |
| POST | `/api/devices/{id}/stop` | Yes | Stop a device |
| POST | `/alexa` | Amazon signature | Alexa skill webhook |
| WS | `/ws` | Yes | Real-time sync & audio extraction |

`GET /api/tracks` accepts `page` (default 1) and `per_page` (default 10, max 100) query parameters and returns:

```json
{ "tracks": [ ... ], "total": 42, "page": 1, "per_page": 10 }
```

`POST /api/tracks/reorder` moves a track to a zero-based position in the overall library order (out-of-range indexes are clamped to the end):

```json
{ "track_id": "dQw4w9WgXcQ", "new_index": 3 }
```

`POST /api/devices/{id}/seek` queues a play command for the device's current track at the given position (clamped to just before the end of the track; rejected for live streams):

```json
{ "position_ms": 63000 }
```

Since a custom Alexa skill cannot push directives to an Echo, the seek — like play — takes effect the next time the device contacts the skill: when the user says "Alexa, open YouTube Player", or automatically at the next track transition. The Web UI shows a per-device seek bar with the playback position estimated from the last reported offset.

## License

[MIT](LICENSE)
