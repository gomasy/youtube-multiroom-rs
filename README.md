# YouTube MultiRoom

A Spotify Connect-style system for simultaneously playing YouTube audio on multiple Amazon Echo Dot devices.
Built with axum + tokio (backend) and React + TypeScript (frontend).

## Project Structure

```
youtube-multiroom-rs/
├── build.rs                # Embed git hash & build date
├── .env.example            # Environment variable template
├── .github/workflows/      # build-image (ghcr.io), check (clippy/fmt/test), release
├── locales/{en,ja}.yml     # Backend message catalogs
├── alexa_interaction_model{,_en}.json   # Alexa interaction models (ja / en)
├── src/
│   ├── main.rs             # Entry point & router
│   ├── auth.rs             # Bearer auth middleware & signed stream URLs
│   ├── locale.rs           # Request locale resolution (X-App-Lang / Alexa locale)
│   ├── alexa/              # The Alexa skill, split by subject
│   │   ├── mod.rs              # Per-request context; the dispatch into the rest
│   │   ├── verify.rs           # Alexa request signature verification
│   │   ├── session.rs          # Opening the skill: starting or resuming playback
│   │   ├── intent.rs           # Spoken intents & the touch/remote controls
│   │   ├── event.rs            # AudioPlayer events: started, finished, failed
│   │   ├── next_up.rs          # Choosing what plays next & its token
│   │   └── response.rs         # Response envelope, speech, AudioPlayer directives
│   ├── state/              # Shared state (AppState), split by subject
│   │   ├── mod.rs              # AppState itself; re-exports the module API
│   │   ├── model.rs            # Wire/storage types and AudioPlayer tokens
│   │   ├── track.rs            # Audio library: registration, ordering, selection
│   │   ├── matching.rs         # Ranking stored text against a spoken phrase
│   │   ├── device.rs           # Per-device state, pending commands, queues
│   │   ├── playback.rs         # Playback mode & sleep timer
│   │   ├── playlist.rs         # Named playlists & YouTube playlist import
│   │   ├── cache.rs            # Reconciling the cache directory with the library
│   │   ├── library.rs          # Exporting the library structure & importing it back
│   │   ├── download.rs         # Audio downloading & metadata refresh
│   │   ├── remux.rs            # Rebuilding a downloaded file's container
│   │   ├── progress.rs         # Progress reporting & cancellation generation
│   │   ├── job.rs              # Stopping rules for background per-video jobs
│   │   ├── ytdlp.rs            # yt-dlp invocation & process group reaping
│   │   └── url.rs              # YouTube URL / video ID validation
│   └── handlers/           # HTTP / WebSocket handlers, mirroring the split above
│       ├── mod.rs              # Error type, shared 404 lookups, response locale
│       ├── audio.rs            # Audio streaming, live relay, signed stream URLs
│       ├── search.rs           # YouTube search
│       ├── tracks.rs           # Track listing, reordering, deletion
│       ├── playlists.rs        # Playlist CRUD & membership
│       ├── devices.rs          # Device state, playback commands, device sync
│       ├── cache.rs            # Cache status, orphan cleanup, missing-file repair
│       ├── library.rs          # Library export & import
│       ├── alexa.rs            # Alexa webhook endpoint
│       └── ws.rs               # WebSocket push channel
└── front/
    ├── locales/{en,ja}.json    # Frontend message catalogs (fetched at runtime)
    └── src/
        ├── App.tsx, index.tsx, index.html
        ├── api.ts              # Auth-aware fetch wrapper
        ├── errors.ts           # Turns a rejected API call into a toast
        ├── format.ts           # Shared time/duration formatters
        ├── hooks.ts            # WebSocket channel & drag-to-reorder hooks
        ├── i18n.ts             # Locale detection & lookup
        ├── types.ts            # Shared type definitions
        ├── styles/             # SCSS (tokens, mixins, component partials)
        ├── icons/              # SVG sources + generated PNGs
        ├── manifest.webmanifest
        └── components/         # DeviceList, NowPlaying, SeekBar, LibraryTools, Toast, …
```

## Build & Run

### Prerequisites

- Rust 1.88+
- OpenSSL headers & pkg-config (build only; `libssl-dev` on Debian/Ubuntu, for Alexa signature verification)
- Node.js 22.12+
- Redis
- yt-dlp
- ffmpeg (`ffprobe` included)
- A tunnel to expose the server (e.g. ngrok, Cloudflare Tunnel, Tailscale Funnel)

### Build

```bash
cd front && npm install && npm run build && cd ..   # Frontend
cargo build --release                               # Backend
```

### Environment Variables

| Variable | Required | Description |
|---|---|---|
| `REDIS_URL` | Yes | Redis connection URL (e.g. `redis://127.0.0.1/`) |
| `API_TOKEN` | No | Bearer token for API authentication |
| `LISTEN_ADDR` | No | Address and port to listen on (default: `0.0.0.0:8888`) |
| `ALEXA_SKILL_ID` | No | Skill ID (`amzn1.ask.skill.…`) that `/alexa` requests must name; any skill is accepted when unset |

These can also live in a `.env` file in the working directory, loaded automatically at startup; real environment variables take precedence. See `.env.example`.

### Run

```bash
ngrok http 8888                                     # Tunnel, in another terminal
REDIS_URL=redis://127.0.0.1/ ./target/release/youtube-multiroom
```

Access the Web UI at `http://localhost:8888`. For development, `cd front && npm run dev` runs `cargo run` and `parcel watch` together.

### Authentication

Setting `API_TOKEN` protects the API with a Bearer token:

- The Web UI prompts for the token on first access (stored in localStorage).
- API endpoints and the WebSocket require `Authorization: Bearer <token>` (or `?token=` for the WebSocket, which cannot send headers).
- `/api/audio/{id}/stream` and `/api/audio/{id}/live` accept a signed URL instead, since Echo devices cannot send auth headers: `?exp=<unix>&sig=<hmac>`, HMAC-SHA256 derived from `API_TOKEN` and valid for 24 h. Bearer auth is also accepted.
- `/alexa` is exempt from Bearer auth — every request to it is instead verified as genuinely coming from Alexa (certificate chain validation, body signature, timestamp freshness), whether or not `API_TOKEN` is set. This means you cannot `curl` it manually.
- Setting `ALEXA_SKILL_ID` additionally requires each request to name your skill. Signature verification only proves a request came from Amazon, so without this anyone who learns your URL can drive your devices from a skill of their own.

Without `API_TOKEN`, no authentication is required.

### Internationalization

UI text and Alexa voice responses come from per-language catalogs; no language is hard-coded, and the supported set is whatever files are present.

- **Backend**: `locales/*.yml`, embedded at compile time via `rust-i18n`. Adding or changing a language requires a rebuild.
- **Frontend**: `front/locales/*.json`, fetched on demand. The Web UI detects `navigator.language`, loads the matching catalog plus `en` as fallback, and sends the language in the `X-App-Lang` header so the backend replies in kind.

The language is resolved per request — `X-App-Lang` for the Web UI, `request.locale` for Alexa, so an Echo set to Japanese gets Japanese replies. Anything unrecognized falls back to `en`. To add a language, copy `en.yml` / `en.json` to the new code and translate every key.

### Docker

A multi-arch (amd64/arm64) image is built by GitHub Actions and published to GHCR. It bundles `yt-dlp`, `ffmpeg` and `deno`; only Redis is needed externally.

```bash
docker run -d -p 8888:8888 \
  -e REDIS_URL=redis://<redis-host>/ \
  -e API_TOKEN=your-secret-token \
  ghcr.io/gomasy/youtube-multiroom-rs
```

To build locally: `docker build -t youtube-multiroom .`

### GitHub Releases

Pre-built x86_64 and aarch64 Linux binaries (with `front/dist/` bundled) are published on version tags. `yt-dlp`, `ffmpeg` and Redis are still needed on the host.

```bash
tar xzf youtube-multiroom-aarch64-unknown-linux-gnu.tar.gz
cd youtube-multiroom && ./youtube-multiroom
```

### Cross-compilation for Raspberry Pi

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
scp target/aarch64-unknown-linux-gnu/release/youtube-multiroom pi@raspberrypi:~/
```

The binary, `front/dist/`, `yt-dlp` and `ffmpeg` are needed on the Pi.

### systemd Service

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

## Alexa Skill Setup

1. Create a custom skill on the [Alexa Developer Console](https://developer.amazon.com/alexa/console/ask)
2. Interaction Model > JSON Editor: paste `alexa_interaction_model.json` (Japanese, invocation name `youtube プレーヤー`) or `alexa_interaction_model_en.json` (English, `youtube player`). Both declare the same intents; only the invocation name and sample utterances differ
3. Interfaces > enable **Audio Player**
4. Endpoint > HTTPS > `https://<your-tunnel-url>/alexa`
5. Test > set to **Development**
6. Optional: copy the skill ID (`amzn1.ask.skill.…`, shown under the skill name on the console's skill list) into `ALEXA_SKILL_ID` so the server answers only your skill

A skill can carry several languages: add each under Build > Language settings and paste the matching model into that language's JSON editor. The backend answers in the language the device reports, regardless of which model was built.

When upgrading from an older version, re-paste and rebuild the model — `AMAZON.NextIntent` / `AMAZON.PreviousIntent` (voice skip) and `PlayTrackIntent` / `PlayPlaylistIntent` (naming what to play) were added.

## PWA

The Web UI ships a web app manifest and icons, so it can be installed to the home screen (Android/Chrome 「アプリをインストール」, iOS Safari 「ホーム画面に追加」) and runs standalone in a dark themed window. Installation requires HTTPS — the tunnel URL works. Icon PNGs are generated from the SVG sources in `front/src/icons/` (regenerate with `rsvg-convert`).

## Usage

1. Open `http://localhost:8888`
2. Paste a YouTube URL (auto-extracted on paste), or type keywords and press 検索 to search and pick a result. A playlist URL (`playlist?list=...`) bulk-imports its videos. The box clears as soon as the request is sent, so URLs can be queued back to back — every accepted request appears in the progress list, which lives on the server and survives a reload
3. Say **「アレクサ、YouTube プレーヤーを開いて」** to each Echo
4. Select devices in the Web UI and click play, then say the phrase again on each Echo to start playback
5. Optionally pick a playback mode (off / loop / shuffle) to auto-play the next track, and narrow the scope to a playlist with 再生範囲
6. Drag the grip handle (⋮⋮) on a row to reorder the library; hold the drag over the pagination buttons to flip pages and drop the track elsewhere
7. 「次に再生に追加」 appends the selected track to the play-next queue of the selected devices; queued tracks play before the loop/shuffle mode kicks in and can be reviewed or removed on each device card
8. Say 「アレクサ、次の曲」 / 「アレクサ、前の曲」 while playing to skip; on an Echo Show the on-screen buttons work too. 「アレクサ、YouTube プレーヤーで〇〇をかけて」 plays a track from the library by name, and 「〇〇のプレイリストをかけて」 a playlist
9. The ▶ button under 選択中のトラック previews the selected track in the browser
10. The filter input above the track list searches title and channel (debounced 300 ms, case-insensitive)
11. Click a playlist name to rename it inline — Enter saves, Escape cancels
12. 「選択」 enters select mode: check tracks or 「全選択」 a page, then bulk-delete, bulk-add to a playlist, or 「メタデータ更新」. In a playlist view, removal only affects that playlist
13. Set a sleep timer (15 / 30 min, 1 / 3 / 6 h) below the playback mode selector
14. 「他を同期」 on a device card brings every other device to that device's track and position
15. 「メンテナンス」 at the bottom of the left column reports the cache size, removes files no track claims, re-downloads tracks whose audio is missing, and exports or imports the library structure

## Architecture

```
    AppState (Arc)
    ├── redis: ConnectionManager           # all persistent state
    │    ├── youtube:tracks                # track metadata (hash)
    │    ├── youtube:tracks_order          # track display/playback order (list)
    │    ├── youtube:devices               # Echo device states (hash)
    │    ├── youtube:playback_mode         # auto-play mode ("off" | "loop" | "shuffle")
    │    ├── youtube:playlists             # named playlist metadata (hash)
    │    ├── youtube:playlist:{id}         # playlist track ids in order (list)
    │    ├── youtube:active_playlist       # loop/shuffle selection scope (playlist id)
    │    ├── youtube:sleep_timer           # sleep timer expiry (UNIX seconds, with TTL)
    │    ├── youtube:pending:{device_id}   # queued play command (10 min TTL)
    │    └── youtube:queue:{device_id}     # play-next queue (list of unique entries)
    ├── downloads: Mutex<HashMap>          # in-memory download progress
    └── tx: broadcast::Sender              # real-time sync
```

All state lives in Redis, so tracks, devices and queued play commands survive restarts. Pending play commands carry a native Redis TTL (10 minutes) and are consumed atomically via `GETDEL`.

The play-next queue is a per-device Redis list of unique entries (`{track_id}#{millis}`, the same format as AudioPlayer tokens). A queued playback uses its entry as the token, so consumption is an exact `LREM` value match: the entry is removed once playback is confirmed (`AudioPlayer.PlaybackStarted`) or fails (`PlaybackFailed`, so an unplayable item never jams the queue). A discarded ENQUEUE or a re-delivered event therefore never loses or double-consumes a track, and removing an item from the Web UI can never delete the wrong one. When a track finishes, the next is chosen in priority order: pending play command → play-next queue → playback mode.

If the track metadata hash is lost (Redis wiped, say), the next `GET /api/tracks` rebuilds it in the background from the m4a filenames in `audio_cache/`, re-fetching metadata via yt-dlp; file mtime becomes the registration time to preserve ordering, and a `tracks_update` is broadcast when done. The custom order itself cannot be recovered — those tracks fall back to newest-first.

### Download Progress

Progress is tracked server-side and broadcast to all WebSocket clients as `downloads_update`, so every connected browser — including tabs opened after a download started — sees the same thing. The stages are `metadata`, `downloading` (with a percentage), `processing` (yt-dlp post-processing, then the container rebuild), and on failure `error` (shown for 60 seconds).

An entry opens as soon as the job is known — before it queues behind another request for the same video, before the cache lookup, and for a playlist import before it has expanded — which is what makes a request outlive the reload of the client that sent it. Until a title is known the entry shows the bare ID; a playlist import still expanding is reported with `kind: "playlist"` and namespaced apart from the video IDs. An entry belongs to the job that opened it, so a second request for a video already on display neither gets its own entry nor resets the first one's progress. Progress is not persisted and resets on restart. 「すべて停止」 terminates the active yt-dlp process groups (including ffmpeg descendants) and removes their staging directories, leaving completed files and later downloads intact.

Each attempt downloads into its own directory under `audio_cache/.downloads/`, so partial and post-processing files are never visible to the cache scanner or the stream endpoint. A finished file is published with a hard link, which is atomic and refuses to overwrite an existing entry, so a download completing alongside a cached copy cannot truncate what is being served. The staging root is discarded at startup, cleaning up after a crash.

Still in staging, the container is rebuilt with `ffmpeg -c copy` plus `+faststart` — the AAC frames are untouched and only the boxes describing them are rewritten. This is what keeps finished live streams playing to the end: yt-dlp downloads their audio as DASH fragments and skips the merge, leaving a header that indexes none of the audio, which an Echo takes at its word and reports a multi-hour archive as nearly finished minutes in. The rebuild is best effort and never installs a file holding less audio than the download. Only tracks downloaded since this was added have a rebuilt container; re-add an older one to fix it.

### Metadata Refresh

Tracks registered while YouTube was unreachable keep whatever little was known at the time (the video ID as the title, no thumbnail, no duration), and a retitled video or renamed channel leaves the stored copy stale. Selecting tracks and pressing 「メタデータ更新」 posts them to `/api/tracks/refresh-metadata`, which re-fetches each with yt-dlp.

Each track costs its own yt-dlp run, so the request only starts the job and reports how many tracks it will visit; results arrive as `tracks_update` frames, one per refreshed track. Progress goes through the same `downloads_update` channel as a download, which also means 「すべて停止」 cancels a refresh in flight.

What describes the local copy rather than the video is preserved: the cached file path, the registration time (so the custom order is undisturbed), and the `is_live` flag — an ended stream reports itself as a normal video, and adopting that would leave an entry claiming a file it never had. A refresh also never resurrects a track deleted while its fetch was running.

### Track Ordering

Tracks are listed and auto-played in a user-defined order persisted in `youtube:tracks_order`. Rows can be rearranged by dragging the grip handle (mouse and touch, via Pointer Events). Reordering works across pages: hovering a pagination button mid-drag auto-flips pages (one per 650 ms). Newly extracted tracks go to the top; tracks absent from the order list are appended newest-first.

The list may also name ids no track answers to — what an import leaves for videos that have not arrived yet. Listing skips them, so they are invisible until the video is downloaded, at which point it takes the place they were holding.

### Live Streams

Live streams (including `youtube.com/live/<id>` URLs) can be added like regular videos. Since they cannot be cached, only metadata is stored, with an `is_live` flag, and the Web UI shows a red **LIVE** badge in place of the duration.

At playback time, `GET /api/audio/{id}/live` resolves a fresh CDN HLS URL via yt-dlp (preferring audio-only HLS, falling back to the lowest-bitrate muxed HLS, which live streams often need) and relays the audio as ADTS AAC extracted by ffmpeg. AAC sources are codec-copied; a non-AAC fallback (Opus, which ADTS cannot carry) is transcoded. When the Echo disconnects, the pipe closes and ffmpeg exits on its own.

Caveats:

- Live tracks are not recoverable by the `audio_cache/` scan — if Redis is wiped, re-add them
- A track added while live keeps its `is_live` flag after the broadcast ends; delete and re-add it to cache the archived video
- Playback starts a few segments behind the live edge (typical HLS latency)

### Playback Modes

What happens when a track ends is set by a global playback mode, persisted in Redis and defaulting to `off`:

- `off` — stop after the current track
- `loop` — continue with the next track in order, wrapping to the top
- `shuffle` — continue with a random track other than the current one

On `AudioPlayer.PlaybackNearlyFinished` the server picks the next track and enqueues it via an `ENQUEUE` directive. The selection scope defaults to the whole library and can be narrowed to a playlist with 再生範囲 (persisted in `youtube:active_playlist`). A deleted scope falls back to the whole library; an empty one stops auto-play.

### Playlists

Tracks can be organized into named playlists, created from the Web UI or by an import. A playlist stores an ordered list of track ids in `youtube:playlist:{id}` and shares the library's files, so adding or removing entries never touches the cache. Playlist rows reorder by dragging, as the library does. Deleting a track from the library removes it from every playlist.

Pasting a YouTube playlist URL (or any YouTube URL with `list=` but no `v=`) bulk-imports it: the playlist is expanded with `yt-dlp --flat-playlist` (capped at 100 entries), a local playlist of the same name is created or appended to, and each video is downloaded sequentially in the background with the usual progress display. Videos that fail are skipped. A watch URL carrying both `v=` and `list=` is treated as a single video.

### Naming What to Play

`PlayTrackIntent` (「アレクサ、YouTube プレーヤーで〇〇をかけて」 / "Alexa, ask YouTube Player to play …") plays a library track by name, and `PlayPlaylistIntent` (「〇〇のプレイリストをかけて」) starts a playlist and makes it the selection scope, so what follows comes from it rather than from whatever 再生範囲 was left on. Both are `AMAZON.SearchQuery` slots, so re-paste the interaction model after upgrading.

What Alexa hands over is what it heard, not what was typed, so neither side is compared as written: both are folded to lowercase with everything but letters and digits dropped. 「けもの フレンズ」 therefore answers to "けものフレンズ", and a title like `【MV】Hello, World! - Official` to "hello world". A whole name beats a name it merely starts, which beats a name it only mentions; a title match always beats a channel match, so a song is never lost to the uploader of another one; and equal matches go to whichever is earlier in the library order, so a phrase always plays the same thing. Nothing close enough plays nothing at all and says so — an arbitrary track is a worse answer than "I couldn't find it".

The search covers the whole library rather than the playback scope: naming something is how a listener escapes the scope the web UI is set to.

### Device Sync

Since a custom skill cannot push directives, each Echo starts when it is spoken to, which is what leaves a room that joined late playing the same track minutes behind the others. 「他を同期」 on a device card queues that device's current track on every other device at the position it has reached — the same estimate the seek bar draws, taken at the moment the command is written. Like a seek, each follower applies it the next time it contacts the skill, so the phrase still has to be said on it (or it lands by itself at the next track transition).

A live stream is queued from zero instead: it has no position to match, and the relay hands out whatever is at the live edge regardless. Finite tracks are clamped a second short of the end, as a seek is, so a follower does not start by immediately finishing.

### Cache Health

The cache directory and the library drift apart in both directions, and neither direction is visible from the track list alone. `GET /api/cache` reports both, and the 「メンテナンス」 panel is where they surface:

- **Missing audio** — a track registered with no file behind it (Redis restored from a backup, a file deleted by hand). It fails only when an Echo is asked for it, so the library list marks the row 「音源なし」 and 「欠損を再取得」 re-downloads every such track. Live tracks never had a file and are never counted.
- **Orphaned files** — a cached file no registered track claims, left by a deletion that could not reach Redis. 「不要を削除」 removes them. Only files older than ten minutes count: a download publishes its file into the cache moments before it registers the track, and a young unclaimed file is far more likely to be a download landing right now.

A recovery is an ordinary download, which prepends — right for a track being added, wrong for one being repaired. So the order list is snapshotted first and each finished track is put back just after the last id that preceded it there and is still present, which leaves the rest of the order exactly as concurrent edits left it.

The track list itself only stats the page being served, so a library of any size costs the same handful of syscalls; the full scan runs on a blocking thread and only while the maintenance panel is open.

### Library Backup

Redis holds what no audio file records: which videos the library knows, the order they play in, and the playlists they belong to. The audio can always be fetched again — the cache scan rebuilds track metadata from the files alone — but the order and the playlists exist in exactly one place. `GET /api/library/export` is the copy that outlives it, a single JSON document carrying the track list in library order, every playlist with its membership, the playback mode and the selection scope. No file path appears in it: `AudioTrack` skips that field, so a document from one machine says nothing about another's disk.

`POST /api/library/import` applies one back, additively:

- Playlists are matched by **name** and appended to rather than replaced — the rule a YouTube playlist import already follows — so re-importing into a server that already has them does not duplicate anything. The exported selection scope is re-resolved through that matching, since its ID belonged to the exporting server.
- The restored order leads, and whatever this server has that the document never mentioned follows in the order it already had. Ids naming videos that are not here are kept as placeholders: the listing skips them, and downloading one later drops it straight into the gap it was holding.
- With 「手元にない曲の音源もダウンロードする」 (`"download": true`), the videos with no track here are then fetched in the background through the same recovery job a cache repair uses — so they land in their exported positions, not at the top. Left off, the import is instant and restores structure only.

A document whose `version` this server does not read is refused rather than applied in part.

### Voice Skip & Playback Controls

`AMAZON.NextIntent` (「アレクサ、次の曲」) switches immediately: a pending web command first, then the play-next queue, then the playback-mode selection (shuffle picks at random; loop/off advance in order — an explicit "next" moves on even when the mode is off). `AMAZON.PreviousIntent` goes back one track in scope order, wrapping from first to last. Both reset the playback-failure retry counter, like an explicit resume.

`PlaybackController.*` requests (Echo Show touch controls, physical remote buttons) map to the same handlers. Per the Alexa spec their responses carry only `AudioPlayer` directives and no speech.

### Sleep Timer

A sleep timer can be set from the Web UI (15 min, 30 min, 1 h, 3 h, 6 h). On expiry all devices are stopped and the playback mode is set to "off". The countdown is displayed in real time on all clients, and setting a new timer cancels any existing one. The expiry lives in `youtube:sleep_timer` with a Redis TTL; a generation counter keeps a stale spawned task from firing after a cancellation or reset.

### Browser Preview

The Now Playing card has a small preview player for checking a track before sending it to the Echos. Since `<audio>` cannot send an `Authorization` header, the client first calls `GET /api/audio/{id}/url` (Bearer-authenticated) for a signed stream URL — the same HMAC scheme Echo playback uses — and feeds that to the element. Live tracks are relayed through `/live` and play without seeking.

### WebSocket Protocol

Audio extraction runs over the WebSocket to avoid reverse proxy read timeouts. The client sends `{ "type": "extract_audio", "url": "..." }` and receives `extract_audio_result`, `extract_audio_error` or `extract_audio_cancelled`; a playlist URL instead answers with `playlist_import_result` (`name`, `total`) once the background import has started. Each request runs on its own task and is answered exactly once, so several can be in flight on one socket — two requests for the same video are serialized server-side, and the second gets the first's result from the cache.

The client can also send `cancel_downloads`, `set_playback_mode` (`"off" | "loop" | "shuffle"`), `set_active_playlist` (playlist id or null), `set_sleep_timer` (`minutes`, null or 0 to cancel), and `ping` (answered with `pong`).

On connect the server sends an `init` frame with the server version, device map, playback mode, in-progress downloads, playlists, active playlist, sleep timer expiry, and `tracks_rev`. The track list itself is fetched separately via `GET /api/tracks`; `tracks_rev` is what tells a client whether the page it already holds is current. A client that falls behind the broadcast channel is re-sent the whole `init` frame followed by a `tracks_update`, so a dropped message resyncs it rather than leaving it silently stale.

The client pings every 30 s and reconnects on its own when the socket drops, backing off from 1 s and doubling per consecutive failure up to 30 s. A frame that fails to parse is logged and skipped rather than tearing down the connection.

Broadcast frames:

- `device_update` — device status, track assignment, connection changes (full device map)
- `tracks_update` — the track list or a playlist's contents changed; clients refetch their page
- `playback_mode_update` — the playback mode was changed by a client
- `downloads_update` — download progress started, advanced, completed or failed
- `playlists_update` — playlist list or contents changed (full list with counts)
- `active_playlist_update` — the loop/shuffle selection scope changed
- `sleep_timer_update` — the timer was set, cancelled or expired (`expires_at`, or null)

## API Reference

| Method | Path | Auth | Description |
|---|---|---|---|
| GET | `/api/audio/{id}/stream` | Signed URL | Stream m4a audio (Range supported; see below) |
| GET | `/api/audio/{id}/live` | Signed URL | Relay live stream audio as ADTS AAC via ffmpeg |
| GET | `/api/audio/{id}/url` | Yes | Get a signed stream URL for browser preview |
| GET | `/api/search` | Yes | Search YouTube (`q`, optional `limit`) |
| GET | `/api/tracks` | Yes | List tracks in order (paginated, optional `playlist` and `q`) |
| POST | `/api/tracks/reorder` | Yes | Move a track within the library or a playlist order |
| POST | `/api/tracks/bulk-delete` | Yes | Bulk-delete tracks by ID list |
| POST | `/api/tracks/refresh-metadata` | Yes | Start a background metadata refresh (`track_ids`) |
| DELETE | `/api/tracks/{id}` | Yes | Delete a track and its cached file |
| GET | `/api/playlists` | Yes | List playlists with track counts |
| POST | `/api/playlists` | Yes | Create a playlist (`name`) |
| PATCH | `/api/playlists/{id}` | Yes | Rename a playlist (`name`) |
| DELETE | `/api/playlists/{id}` | Yes | Delete a playlist (tracks are kept) |
| POST | `/api/playlists/{id}/tracks` | Yes | Add a track to a playlist (`track_id`) |
| POST | `/api/playlists/{id}/tracks/bulk` | Yes | Bulk-add tracks (`track_ids`) |
| POST | `/api/playlists/{id}/tracks/bulk-remove` | Yes | Bulk-remove from one playlist (`track_ids`) |
| DELETE | `/api/playlists/{id}/tracks/{track_id}` | Yes | Remove a track from a playlist |
| GET | `/api/devices` | Yes | List connected devices |
| DELETE | `/api/devices/{id}` | Yes | Delete a device |
| POST | `/api/play` | Yes | Queue playback on selected devices |
| POST | `/api/play-all` | Yes | Queue playback on all devices |
| POST | `/api/queue` | Yes | Add a track to selected devices' play-next queue |
| DELETE | `/api/devices/{id}/queue` | Yes | Clear a device's play-next queue |
| DELETE | `/api/devices/{id}/queue/{entry}` | Yes | Remove one item from a device's queue |
| POST | `/api/devices/{id}/seek` | Yes | Queue playback of the current track from a position |
| POST | `/api/devices/{id}/sync` | Yes | Line other devices up with this one (see below) |
| POST | `/api/devices/{id}/stop` | Yes | Stop a device |
| GET | `/api/cache` | Yes | Cache size, orphaned files, tracks with missing audio |
| POST | `/api/cache/cleanup` | Yes | Delete cached files no track claims |
| POST | `/api/cache/repair` | Yes | Re-download the tracks whose audio is missing |
| GET | `/api/library/export` | Yes | Export the library structure as one JSON document |
| POST | `/api/library/import` | Yes | Apply an exported document (`download` to also fetch audio) |
| POST | `/alexa` | Amazon signature | Alexa skill webhook |
| WS | `/ws` | Yes | Real-time sync & audio extraction |

**Range requests.** `GET /api/audio/{id}/stream` follows RFC 9110: a satisfiable range comes back as a `206` (an end past the last byte is clamped); a well-formed range naming no byte of the file — a start at or past the end, `bytes=-0`, any range against an empty file — gets a `416` plus `Content-Range: bytes */<len>`; and a header that cannot be acted on (unparsable, `last < first`, or multi-range) is ignored so the whole file goes out as a `200`.

**Pagination.** `GET /api/tracks` accepts `page` (default 1), `per_page` (default 10, max 100) and `q` (case-insensitive substring filter on title and channel):

```json
{ "tracks": [ ... ], "total": 42, "page": 1, "per_page": 10, "rev": 7 }
```

`rev` is the track-list revision the page was served at, sampled before the read so a change landing mid-assembly reports as a newer revision rather than being hidden. A client that fetched a page before its WebSocket existed compares it with `tracks_rev` in `init`: equal means the page it holds is current. The counter is in-process and restarts at 0, so clients treat every reconnect as a change regardless.

**Reordering.** `POST /api/tracks/reorder` moves a track to a zero-based position in the library order (out-of-range indexes clamp to the end):

```json
{ "track_id": "dQw4w9WgXcQ", "new_index": 3 }
```

**Seeking.** `POST /api/devices/{id}/seek` queues a play command for the device's current track at the given position, clamped to just before the end and rejected for live streams:

```json
{ "position_ms": 63000 }
```

Since a custom Alexa skill cannot push directives, the seek — like play — takes effect the next time the device contacts the skill: when the user opens it by voice, or automatically at the next track transition. The Web UI shows a per-device seek bar with the position estimated from the last reported offset.

**Sync.** `POST /api/devices/{id}/sync` queues the named device's current track on the others at the position it has reached. An empty body means every other registered device, which is what the Web UI sends; an explicit list narrows it:

```json
{ "device_ids": ["amzn1.ask.device.…"] }
```

The reply names the devices reached and the offset they were given. The named device is never one of them, and a device that is no longer registered is skipped rather than failing the request, as it is for `/api/play`.

**Import.** `POST /api/library/import` takes an exported document with one field added:

```json
{ "version": 1, "tracks": [ ... ], "playlists": [ ... ], "download": true }
```

The reply reports how many playlists were created or appended to, how many ids the restored order names, how many of them this server has no track for, and how many of those a download was started for.

## License

[MIT](LICENSE)
