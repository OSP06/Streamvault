# StreamVault

A minimal private video streaming service. Upload any video file up to 1 GB and receive a token-based shareable link for immediate in-browser streaming — no account required, no transcoding delay.

**Stack:** Rust · Axum 0.7 · SQLite (SQLx) · FFmpeg · SvelteKit · nginx · Docker

---

## Contents

- [Quick Start — Docker](#quick-start--docker-recommended)
- [Quick Start — No Docker](#quick-start--no-docker)
- [Project Structure](#project-structure)
- [Environment Variables](#environment-variables)
- [API Reference](#api-reference)
- [How It Works](#how-it-works)
- [Development Setup](#development-setup)
- [Building for Production](#building-for-production)
- [Troubleshooting](#troubleshooting)
- [Known Limitations](#known-limitations)

**Documentation:**
- [docs/DESIGN.md](docs/DESIGN.md) — how the current system works: request flows, design decisions, performance benchmarks, edge cases
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — production architecture: scaling path, deployment tiers, technology alternatives, cost analysis
- [benchmarks/RESULTS.md](benchmarks/RESULTS.md) — measured performance: time-to-stream, HLS latency, seek characteristics, concurrent streaming

---

## Quick Start — Docker (Recommended)

**Requirements:** [Docker Desktop](https://www.docker.com/products/docker-desktop/) installed and running. Nothing else — Rust, Node.js, and FFmpeg are all inside the containers.

```bash
# Unzip and enter the project
cd streamvault

# First run: builds Rust binary + Svelte frontend (~5 min, one-time cost)
# Subsequent runs use Docker layer cache (~15 sec)
docker compose up --build
```

Wait for this exact output before opening the browser:

```
backend-1  | INFO streamvault: Upload dir: "/data/uploads"
backend-1  | INFO streamvault: Database: sqlite:///app/streamvault.db?mode=rwc
backend-1  | INFO streamvault: Database ready
backend-1  | INFO streamvault: StreamVault listening on 0.0.0.0:3000
frontend-1 | Configuration complete; ready for start up
```

Open **http://localhost** — drag a video file onto the page and you'll have a shareable link within seconds.

> **What a successful upload looks like in the logs:**
> ```
> backend-1 | INFO streamvault::handlers::upload: Uploaded video.mp4 (66759298 bytes) → token=wfuwbioe
> backend-1 | INFO streamvault::streaming: HLS transcode complete for token=wfuwbioe
> ```
> The share link is returned the moment the upload finishes. The HLS line appears 1–2 seconds later in the background — you don't wait for it.

### Stopping

```bash
# Stop containers, keep all uploaded videos and the database
docker compose down

# Stop containers AND wipe all uploaded videos and the database
docker compose down -v
```

> **Note on first build time:** The Rust compiler downloads and compiles ~80 crates from scratch on the first `--build`. Docker caches the compiled dependency layer separately from the source layer — subsequent builds after code changes take ~15 seconds, not 5 minutes.

---

## Quick Start — No Docker

If you don't have Docker or prefer to run natively, follow these steps. You need three things installed: Rust, Node.js, and FFmpeg.

### Step 1 — Install Prerequisites

**Rust** (if not installed):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
# Verify
rustc --version   # should be 1.85+
```

**Node.js** (if not installed):
```bash
# macOS with Homebrew
brew install node

# Ubuntu / Debian
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs

# Verify
node --version    # should be 18+
```

**FFmpeg** (optional but recommended — without it HLS is skipped, byte-range streaming still works):
```bash
# macOS
brew install ffmpeg

# Ubuntu / Debian
sudo apt-get install -y ffmpeg

# Windows (via Chocolatey)
choco install ffmpeg

# Verify
ffmpeg -version
```

---

### Step 2 — Run the Backend

```bash
cd streamvault/backend

# Create the upload directory
mkdir -p /tmp/streamvault-uploads

# Start the backend
UPLOAD_DIR=/tmp/streamvault-uploads \
DB_PATH=/tmp/streamvault.db \
BASE_URL=http://localhost:3000 \
RUST_LOG=streamvault=info \
cargo run --release
```

First run compiles all dependencies (~3–5 min). Subsequent runs start in under a second.

You should see:
```
INFO streamvault: Upload dir: "/tmp/streamvault-uploads"
INFO streamvault: Database: sqlite:////tmp/streamvault.db?mode=rwc
INFO streamvault: Database ready
INFO streamvault: StreamVault listening on 0.0.0.0:3000
```

The backend API is now live at **http://localhost:3000**.

---

### Step 3 — Run the Frontend

Open a **second terminal**:

```bash
cd streamvault/frontend

npm install       # downloads Svelte + Vite (~30 sec, one-time)
npm run dev       # starts Vite dev server with hot reload
```

You should see:
```
  VITE v5.x.x  ready in 800ms

  ➜  Local:   http://localhost:5173/
```

Open **http://localhost:5173** in your browser.

> The Vite dev server automatically proxies `/api/*` requests to `http://localhost:3000` (configured in `vite.config.js`). The backend must be running for uploads and streaming to work.

---

### Step 4 — Use the App (No Docker)

Everything works identically to the Docker version:

1. Drag a video file onto the upload zone at **http://localhost:5173**
2. A shareable link appears immediately after upload
3. Click **Watch Now** or share the link — anyone on your network can stream it
4. If FFmpeg is installed, the HLS badge appears on the video card within a few seconds

---

### No-Docker Environment Variables Reference

| Variable | Value for local dev | Description |
|---|---|---|
| `UPLOAD_DIR` | `/tmp/streamvault-uploads` | Where video files are stored |
| `DB_PATH` | `/tmp/streamvault.db` | SQLite database file |
| `BASE_URL` | `http://localhost:3000` | Embedded in share links — must match where your backend is |
| `RUST_LOG` | `streamvault=info` | Log verbosity |
| `BIND_ADDR` | `0.0.0.0:3000` | TCP address (default, can omit) |

> **Important:** `BASE_URL` must match the address people actually use to reach the backend. In no-Docker mode the frontend runs on `:5173` but the API is on `:3000`, so `BASE_URL=http://localhost:3000` is correct for local use. For the share link to work when sending to others, use your machine's LAN IP: `BASE_URL=http://192.168.1.x:3000`.

---

## Project Structure

```
streamvault/
├── backend/                    # Rust/Axum API server
│   ├── src/
│   │   ├── main.rs             # Entry point, router, AppState wiring
│   │   ├── db.rs               # SQLite pool, WAL mode, all queries
│   │   ├── models.rs           # Video, VideoResponse, UploadResponse structs
│   │   ├── error.rs            # AppError enum → HTTP status code mapping
│   │   ├── streaming.rs        # Background FFmpeg HLS transcode
│   │   └── handlers/
│   │       ├── mod.rs
│   │       ├── upload.rs       # POST /api/upload, GET /api/videos
│   │       ├── stream.rs       # GET /api/stream/:token, /api/hls/*
│   │       └── health.rs       # GET /health
│   ├── Cargo.toml
│   └── Dockerfile              # Two-stage: rust:slim builder → debian:bookworm-slim runtime
├── frontend/
│   ├── src/
│   │   ├── routes/
│   │   │   ├── +page.svelte              # Home — upload zone + video grid
│   │   │   ├── +layout.svelte            # Root layout, imports global CSS
│   │   │   ├── +layout.ts                # SPA mode: ssr=false
│   │   │   └── watch/[token]/
│   │   │       └── +page.svelte          # Player page with live HLS status polling
│   │   ├── lib/components/
│   │   │   ├── UploadZone.svelte         # Drag-drop upload, XHR progress bar
│   │   │   ├── VideoGrid.svelte          # Video card grid
│   │   │   └── Toast.svelte              # Success/error notifications
│   │   ├── lib/types.ts                  # TypeScript interfaces for API responses
│   │   ├── app.html                      # SvelteKit HTML shell
│   │   └── app.css                       # CSS custom properties + global base
│   ├── svelte.config.js                  # adapter-static, fallback: index.html
│   ├── vite.config.js                    # Vite + SvelteKit, /api proxy for dev
│   ├── package.json
│   ├── nginx.conf                        # Reverse proxy config for Docker
│   └── Dockerfile                        # node:20-alpine build → nginx:alpine serve
├── docs/
│   └── ARCHITECTURE.md         # Full system design, decision log, scaling guide
├── docker-compose.yml
├── .gitignore
└── README.md
```

---

## Environment Variables

All variables have defaults suitable for local development. Override in `docker-compose.yml` for Docker, or pass as shell exports for no-Docker.

| Variable | Default | Description |
|---|---|---|
| `BIND_ADDR` | `0.0.0.0:3000` | TCP address the Axum server binds to |
| `UPLOAD_DIR` | `/data/uploads` | Directory where video files are stored |
| `DB_PATH` | `/app/streamvault.db` | Absolute path to the SQLite database file |
| `BASE_URL` | `http://localhost` | Public base URL embedded in share links |
| `RUST_LOG` | `streamvault=info` | Log level. Use `streamvault=debug,tower_http=debug` for per-request logs |

### Important: `DB_PATH` in Docker

The database path must be **absolute** and the parent directory must be writable. Do **not** put `DB_PATH` inside `/data/` — that path is a Docker volume mount, which overwrites everything at container start. The default `/app/streamvault.db` is correct.

### Important: `BASE_URL`

This value is embedded in the `share_url` field of every API response. Set it to wherever your service is publicly reachable:

```yaml
# docker-compose.yml (production)
BASE_URL: https://stream.yourdomain.com

# No-Docker, local network sharing
BASE_URL: http://192.168.1.100:3000
```

---

## API Reference

All endpoints return JSON. Errors return `{"error": "message"}` with an appropriate HTTP status code.

### `POST /api/upload`

Upload a video file via `multipart/form-data`. The field must be named `video`.

**Accepted formats:** `mp4`, `webm`, `mov`, `avi`, `mkv`, `ts`, `mpeg`, `ogg`

**Size limit:** 1 GB (enforced in the handler, not nginx)

**Response `200 OK`:**
```json
{
  "token": "a3f7bc12",
  "original_name": "demo.mp4",
  "size_bytes": 52428800,
  "share_url": "http://localhost/watch/a3f7bc12",
  "stream_url": "http://localhost/api/stream/a3f7bc12"
}
```

**Errors:** `400` (missing field), `413` (over 1GB), `415` (unsupported format)

---

### `GET /api/videos`

List all uploaded videos, newest first. Returns up to 100.

---

### `GET /api/videos/:token`

Metadata for a single video.

**Response `200 OK`:**
```json
{
  "token": "a3f7bc12",
  "original_name": "demo.mp4",
  "content_type": "video/mp4",
  "size_bytes": 52428800,
  "duration_secs": null,
  "hls_ready": true,
  "created_at": "2026-04-05 23:02:49",
  "stream_url": "http://localhost/api/stream/a3f7bc12",
  "share_url": "http://localhost/watch/a3f7bc12"
}
```

**Error:** `404` if token not found

---

### `GET /api/stream/:token`

HTTP byte-range streaming (RFC 7233). Available immediately after upload.

```
Range: bytes=0-1048575
→ 206 Partial Content
  Content-Range: bytes 0-1048575/52428800
  Accept-Ranges: bytes
```

No `Range` header → `200 OK`, full file. Seeking works at any position without re-downloading from the start.

---

### `GET /api/hls/:token/playlist.m3u8`

HLS playlist. Returns `200` with the `.m3u8` if `hls_ready=true`. Returns `307` redirect to `/api/stream/:token` if transcoding is still in progress — the player falls back to byte-range automatically.

---

### `GET /api/hls/:token/:segment`

Individual `.ts` segment file. `Cache-Control: public, max-age=86400, immutable`.

---

### `GET /health`

```json
{ "status": "ok", "service": "streamvault", "version": "0.1.0" }
```

---

## How It Works

### Upload → Instant Streaming

```
1. Browser → POST /api/upload (multipart)

2. nginx forwards bytes immediately
   proxy_request_buffering off — nginx does not buffer the body

3. Axum writes chunks (~64KB at a time) directly to disk
   Peak RAM = O(chunk_size), not O(file_size)
   A 1GB upload uses ~64KB of RAM throughout

4. Upload write completes → Axum:
   a. Inserts metadata row in SQLite
   b. Returns HTTP 200 with share_url and token
   ↑ Video is streamable at this exact moment

5. tokio::spawn fires FFmpeg in the background
   Upload response already sent — user does not wait
   On completion: UPDATE videos SET hls_ready = TRUE
```

### Dual Streaming Protocol

StreamVault uses two protocols simultaneously:

**HTTP Range (immediate):** The raw uploaded file is served byte-by-byte with full seek support. Available the instant the upload write completes. No processing required.

**HLS (background):** FFmpeg remuxes the file into 2-second MPEG-TS segments + an M3U8 playlist in the background. Once ready (`hls_ready=true`), the watch page uses the HLS URL instead. HLS provides better CDN cacheability and seek performance on large files.

The watch page picks the protocol on load and polls every 3 seconds to detect when HLS becomes ready. The status chip flips from amber ("Direct stream") to green ("HLS ready") without a page reload.

### FFmpeg Remux Command

```bash
ffmpeg -i /data/uploads/{uuid}.mp4 \
  -c:v copy \                        # no re-encode — copies bitstream (seconds, not minutes)
  -c:a aac \                         # universal HLS audio codec
  -hls_time 2 \                      # 2-second segments (fast seeks)
  -hls_list_size 0 \                 # VOD: keep all segments in playlist
  -hls_segment_type mpegts \         # MPEG-TS: widest device compatibility
  -hls_segment_filename .../seg%03d.ts \
  -f hls .../playlist.m3u8
```

`-c:v copy` is the architectural key: remuxing skips the decode/encode cycle entirely, making the operation I/O-bound. A 1GB file remuxes in seconds. Re-encoding the same file at `libx264` medium quality takes minutes.

### Share Token

8 characters from `[a-z0-9]` → 36^8 ≈ **2.8 trillion** combinations. At 1,000 requests/second it would take 88 years to exhaust the space. Generated using OS-seeded `rand::thread_rng()`.

---

## Development Setup

### Backend Only

```bash
cd backend

# Run with debug logging
UPLOAD_DIR=/tmp/sv-uploads \
DB_PATH=/tmp/sv.db \
BASE_URL=http://localhost:3000 \
RUST_LOG=streamvault=debug,tower_http=debug \
cargo run

# Watch mode (auto-restart on file change — install cargo-watch first)
cargo install cargo-watch
cargo watch -x run
```

### Frontend Only (requires backend running on :3000)

```bash
cd frontend
npm install
npm run dev    # http://localhost:5173, hot reload
```

### Inspect the Database

```bash
# Docker
docker exec -it streamvault-backend-1 sqlite3 /app/streamvault.db

# No-Docker
sqlite3 /tmp/sv.db

# Useful queries
.mode column
.headers on
SELECT token, original_name, size_bytes, hls_ready, created_at FROM videos;
SELECT COUNT(*) FROM videos;
PRAGMA journal_mode;   -- should be 'wal'
```

### Adding a Feature

| What to change | Where to look |
|---|---|
| New API endpoint | `src/main.rs` (route) + new handler in `src/handlers/` |
| Database schema | `src/db.rs` → `migrate()` |
| Upload metadata | `src/models.rs` + `src/db.rs` → `insert_video()` |
| HLS transcode settings | `src/streaming.rs` (FFmpeg args) |
| Frontend UI | `src/routes/+page.svelte` (home), `watch/[token]/+page.svelte` (player) |
| nginx config | `frontend/nginx.conf` |

---

## Building for Production

### Docker Image Details

The backend Dockerfile uses a two-stage build:

**Stage 1 — Builder** (`rust:1.85-slim`): Compiles the binary. Dependency compilation is cached in a separate layer from source compilation — only changed source files trigger a recompile.

**Stage 2 — Runtime** (`debian:bookworm-slim`): Copies only the compiled binary (~8MB) and installs `ffmpeg`. Final image is ~150MB. Using `debian:bookworm-slim` instead of `rust:latest` reduces the image from ~2GB to ~150MB.

```bash
# Build and check size
docker build -t streamvault-backend:prod ./backend
docker images streamvault-backend:prod
# REPOSITORY               SIZE
# streamvault-backend:prod 148MB
```

### Deploying to Fly.io (Free Tier)

```bash
# Install flyctl
brew install flyctl    # macOS
# or: curl -L https://fly.io/install.sh | sh

fly auth login

# From the project root — Fly auto-detects the Dockerfiles
fly launch --name streamvault --region iad

# Set environment variables
fly secrets set BASE_URL=https://streamvault.fly.dev
fly secrets set UPLOAD_DIR=/data/uploads
fly secrets set DB_PATH=/app/streamvault.db

# Create persistent volume (videos + DB survive deploys)
fly volumes create video_data --size 5 --region iad

# Add volume mount to fly.toml:
# [mounts]
#   source = "video_data"
#   destination = "/data"

# Deploy
fly deploy
```

See `docs/ARCHITECTURE.md` Section 7 for full deployment tier breakdown ($0 → $1,000/month).

---

## Troubleshooting

### Docker: build fails immediately without compiling

**Check:** Docker Desktop is running. Run `docker ps` — if it errors, Docker isn't up.

---

### Docker: `unable to open database file`

**Cause:** `DB_PATH` is inside the `/data/` volume mount.

**Fix:** Use the default `/app/streamvault.db` which is never overwritten by a volume mount.

```bash
docker exec streamvault-backend-1 ls -la /app/
```

---

### No-Docker: `cargo run` fails with linker errors on Linux

**Fix:**
```bash
sudo apt-get install -y pkg-config libssl-dev build-essential
```

---

### No-Docker: Frontend shows "Network Error" on upload

**Cause:** Backend isn't running, or `BASE_URL` doesn't match.

**Check:**
```bash
curl http://localhost:3000/health
# Should return {"status":"ok",...}
```

Make sure the backend is running before starting the frontend dev server.

---

### Video plays but seeking is broken (jumps back to start)

**Cause:** `Accept-Ranges: bytes` header is missing, or nginx is buffering the response.

**Check:**
```bash
curl -I http://localhost/api/stream/{token}
# Must include: Accept-Ranges: bytes
```

**Fix:** Ensure `proxy_buffering off` is in `nginx.conf` for the `/api/` location.

---

### HLS badge never appears on the video card

**Cause 1:** FFmpeg is not installed.
```bash
ffmpeg -version   # if this fails, HLS is skipped — byte-range stream still works
```

**Cause 2:** FFmpeg failed on this specific file (codec unsupported, corrupt file).
```bash
# Docker
docker compose logs backend | grep -i "ffmpeg\|hls"
# Look for the ffmpeg stderr output logged at WARN level
```

**This is expected fallback behaviour.** The video is still fully watchable via byte-range streaming. HLS is an enhancement.

---

### Port 80 already in use (Docker)

```yaml
# docker-compose.yml — change the host port
ports:
  - "8080:80"    # access at http://localhost:8080
```

---

### `npm run dev` fails with module not found

```bash
cd frontend
rm -rf node_modules
npm install
npm run dev
```

---

## Known Limitations

| Limitation | Impact | Resolution path |
|---|---|---|
| Single video quality | No adaptive bitrate — slow connections may buffer | ABR encode pass (480p + 720p) — see ARCHITECTURE.md |
| No upload resume | Failed uploads restart from zero | TUS protocol implementation |
| No video expiry | Storage grows indefinitely | Add `expires_at` column + cleanup job |
| No access revocation | Tokens cannot be invalidated | HMAC-signed URLs with TTL |
| DB lost on `down -v` | Volume deletion wipes database | Host-mount: `./data/db:/app` in compose |
| No thumbnails | Video grid shows placeholder icons | `ffmpeg -vframes 1` at 5 seconds post-transcode |
| SQLite contention | Write locks above ~100 concurrent uploads | Migrate to PostgreSQL (change `DATABASE_URL`) |
| `duration_secs` always null | Metadata incomplete | FFprobe call after transcode |
| `/api/videos` lists all videos | No per-user isolation | By design — privacy is token-based obscurity |

See `docs/ARCHITECTURE.md` for resolution paths, scaling tiers, and the full roadmap.

---

*StreamVault · Rust · Axum · SQLite · FFmpeg · SvelteKit · nginx · Docker*
