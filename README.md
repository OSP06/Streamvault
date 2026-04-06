# StreamVault

A minimal private video streaming service. Upload any video file up to 1 GB and receive a token-based shareable link for immediate in-browser streaming — no account required, no transcoding delay.

**Stack:** Rust · Axum 0.7 · SQLite (SQLx) · FFmpeg · nginx · Docker

---

## Contents

- [Quick Start](#quick-start)
- [Project Structure](#project-structure)
- [Environment Variables](#environment-variables)
- [API Reference](#api-reference)
- [How It Works](#how-it-works)
- [Development Setup](#development-setup)
- [Building for Production](#building-for-production)
- [Troubleshooting](#troubleshooting)
- [Known Limitations](#known-limitations)

---

## Quick Start

**Requirements:** [Docker Desktop](https://www.docker.com/products/docker-desktop/) installed and running.

```bash
# Clone or unzip the project
cd streamvault

# First run — builds Rust binary (~5 min), subsequent runs use cache (~15 sec)
docker compose up --build
```

Wait for this line in the output:

```
backend-1  | INFO streamvault: StreamVault listening on 0.0.0.0:3000
```

Then open **http://localhost** in your browser.

> **Note on first build time:** The Rust compiler downloads and compiles all dependencies from scratch on the first build. This is a one-time cost — Docker caches the compiled dependencies layer, so subsequent builds after source changes take ~15 seconds.

### Stopping

```bash
# Stop containers, keep uploaded videos
docker compose down

# Stop containers AND delete all uploaded videos and the database
docker compose down -v
```

---

## Project Structure

```
streamvault/
├── backend/                    # Rust/Axum API server
│   ├── src/
│   │   ├── main.rs             # App entry point, router, AppState
│   │   ├── db.rs               # SQLite connection pool and all queries
│   │   ├── models.rs           # Video, VideoResponse, UploadResponse structs
│   │   ├── error.rs            # AppError enum → HTTP response mapping
│   │   ├── streaming.rs        # Background FFmpeg HLS transcode
│   │   └── handlers/
│   │       ├── mod.rs
│   │       ├── upload.rs       # POST /api/upload, GET /api/videos
│   │       ├── stream.rs       # GET /api/stream/:token, /api/hls/*
│   │       └── health.rs       # GET /health
│   ├── Cargo.toml
│   └── Dockerfile
├── frontend/
│   ├── index.html              # Complete SPA — no build step required
│   ├── nginx.conf              # Reverse proxy config
│   └── Dockerfile
├── docs/
│   └── ARCHITECTURE.md         # Full system design document
├── docker-compose.yml
├── .gitignore
└── README.md
```

### Why a Single `index.html` Frontend?

The frontend is a single self-contained HTML file with no build step, no npm, and no node_modules. This was a deliberate choice: SvelteKit and similar frameworks require `npm install` during Docker build, which pulls packages from the internet and introduced repeated build failures in CI-like environments. A single HTML file with vanilla JS has zero dependencies, builds in milliseconds, and is fully functional.

---

## Environment Variables

All variables have defaults suitable for local development. Override in `docker-compose.yml` for production.

| Variable | Default | Description |
|---|---|---|
| `BIND_ADDR` | `0.0.0.0:3000` | TCP address the Axum server binds to |
| `UPLOAD_DIR` | `/data/uploads` | Directory where uploaded video files are stored |
| `DB_PATH` | `/app/streamvault.db` | Absolute path to the SQLite database file |
| `BASE_URL` | `http://localhost` | Public base URL — used to construct share links returned by the API |
| `RUST_LOG` | `streamvault=info` | Log level filter. Use `streamvault=debug,tower_http=debug` for verbose output |

### Important Notes on `DB_PATH`

The database path must be **absolute** and the **parent directory must be writable** by the process. The application calls `create_dir_all` on the parent at startup, so intermediate directories are created automatically.

Do **not** place `DB_PATH` inside a Docker volume mount path (e.g. `/data/streamvault.db`). Docker volumes mount as empty directories at container start, wiping any files created during the image build. The default `/app/streamvault.db` is safe because `/app` is the `WORKDIR` — it is never overwritten by a volume mount.

### Important Notes on `BASE_URL`

This value is embedded in the `share_url` and `stream_url` fields of API responses. Set it to the public-facing URL in production:

```yaml
# docker-compose.yml (production)
BASE_URL: https://stream.yourdomain.com
```

---

## API Reference

All endpoints return JSON. Errors return `{"error": "message"}` with an appropriate HTTP status code.

### `POST /api/upload`

Upload a video file. Accepts `multipart/form-data` with a single field named `video`.

**Body limit:** None on this endpoint (Axum's default 2MB limit is explicitly disabled). The handler enforces the 1GB limit internally.

**Accepted MIME types:** `video/mp4`, `video/webm`, `video/quicktime`, `video/x-msvideo`, `video/x-matroska`, `video/mp2t`, `video/mpeg`, `video/ogg`, `application/octet-stream` (auto-detected from extension).

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

**Error responses:**
- `400 Bad Request` — no `video` field in form, or read error during upload
- `413 Payload Too Large` — file exceeds 1 GB
- `415 Unsupported Media Type` — file extension/MIME type not in allowlist

---

### `GET /api/videos`

List all uploaded videos, ordered by upload time descending. Returns up to 100 results.

**Response `200 OK`:** Array of video objects (same shape as `/api/videos/:token`).

---

### `GET /api/videos/:token`

Get metadata for a single video by its share token.

**Response `200 OK`:**
```json
{
  "token": "a3f7bc12",
  "original_name": "demo.mp4",
  "content_type": "video/mp4",
  "size_bytes": 52428800,
  "duration_secs": null,
  "hls_ready": false,
  "created_at": "2026-04-05 23:02:49",
  "stream_url": "http://localhost/api/stream/a3f7bc12",
  "share_url": "http://localhost/watch/a3f7bc12"
}
```

**Error responses:**
- `404 Not Found` — token does not exist

---

### `GET /api/stream/:token`

Stream a video file with full HTTP byte-range support (RFC 7233). This is the primary streaming endpoint — the video is available here immediately after upload completes.

**Request headers (optional):**
```
Range: bytes=0-1048575
```

**Response `206 Partial Content`** (or `200 OK` if no Range header):
```
Content-Type: video/mp4
Content-Range: bytes 0-1048575/52428800
Accept-Ranges: bytes
Cache-Control: public, max-age=3600
```

The browser's native `<video>` element uses this endpoint automatically — range requests enable seeking to any position without re-downloading.

**Error responses:**
- `404 Not Found` — token does not exist

---

### `GET /api/hls/:token/playlist.m3u8`

Returns the HLS playlist once background transcoding is complete. If transcoding is still in progress, responds with `307 Temporary Redirect` to `/api/stream/:token` so the player falls back to byte-range streaming automatically.

**Response `200 OK`:** `application/vnd.apple.mpegurl` content with `Cache-Control: no-cache`.

---

### `GET /api/hls/:token/:segment`

Serve an individual HLS `.ts` segment file.

**Response `200 OK`:** `video/mp2t` content with `Cache-Control: public, max-age=86400`.

Segment names are sanitised — requests containing `/` or `..` are rejected with `400`.

---

### `GET /health`

Liveness check used by the Docker healthcheck.

**Response `200 OK`:**
```json
{
  "status": "ok",
  "service": "streamvault",
  "version": "0.1.0"
}
```

---

## How It Works

### Upload → Instant Streaming

The critical design decision is that the video is **streamable immediately after the upload write completes** — no transcoding is required to watch it. Here's the sequence:

```
1. Browser sends multipart/form-data POST to /api/upload

2. nginx forwards bytes immediately — proxy_request_buffering off
   prevents nginx from buffering the full body in memory

3. Axum streams chunks directly to disk in ~64KB increments.
   Peak memory usage is O(chunk_size), not O(file_size).
   A 1GB upload uses ~64KB of RAM throughout.

4. Once the write flushes to disk, Axum:
   a. Generates an 8-character random alphanumeric token
   b. Inserts a metadata row into SQLite
   c. Returns the share URL in the HTTP response
   ↑ The video is now fully streamable at this point.

5. tokio::spawn fires the FFmpeg HLS transcode in the background.
   The upload response has already been sent — the user doesn't wait.
```

### Byte-Range Streaming

The `/api/stream/:token` handler implements HTTP Range requests (RFC 7233):

```
Browser <video> element sends:
  GET /api/stream/a3f7bc12
  Range: bytes=0-1048575

Axum:
  1. Looks up filename in SQLite by token
  2. Opens the file, seeks to byte offset 0
  3. Streams exactly 1,048,576 bytes using ReaderStream
  4. Returns 206 Partial Content with Content-Range header

When the user seeks to 00:02:30:
  Browser sends: Range: bytes=37748736-
  Axum seeks to offset 37,748,736 and streams from there
  No re-download from the beginning
```

### HLS Background Transcode

After upload, a background task runs:

```bash
ffmpeg -i /data/uploads/{uuid}.mp4 \
  -c:v copy \                    # no re-encode — copies bitstream as-is
  -c:a aac \                     # re-encode audio for HLS compatibility
  -hls_time 2 \                  # 2-second segments
  -hls_list_size 0 \             # VOD mode — keep all segments
  -hls_segment_type mpegts \
  -hls_segment_filename /data/uploads/hls/{token}/seg%03d.ts \
  -f hls /data/uploads/hls/{token}/playlist.m3u8
```

`-c:v copy` is the key flag. It remuxes without decoding/encoding the video track — this is I/O bound and completes in seconds rather than minutes. Once done, `hls_ready` is set to `TRUE` in the database. If FFmpeg is not installed or fails, the system silently falls back to byte-range streaming — it is an enhancement, not a dependency.

### Share Token

The token is 8 characters from the set `[a-z0-9]` (36 characters). That gives 36^8 ≈ 2.8 trillion combinations. At 1,000 requests/second, exhausting the space would take ~88 years. Tokens are generated using a thread-local `rand::Rng` seeded by the OS.

---

## Development Setup

**Requirements:**
- [Rust](https://rustup.rs) (stable, 1.85+)
- [Node.js](https://nodejs.org) is **not** required — the frontend has no build step

### Backend

```bash
cd backend

# First run downloads and compiles all dependencies (~3 min)
cargo run

# With verbose logging
RUST_LOG=streamvault=debug,tower_http=debug cargo run

# Run tests (none currently, placeholder)
cargo test
```

The backend listens on `http://localhost:3000` by default.

### Frontend

The frontend is a static `index.html` file. Serve it with any static file server that proxies `/api/*` to the backend:

```bash
cd frontend

# Option 1: Python (no install)
python3 -m http.server 8080
# Then manually proxy API calls — open http://localhost:8080
# (API calls will fail without a proxy — use Option 2)

# Option 2: npx serve with proxy (requires Node.js)
npx serve -l 8080
```

Or just use Docker Compose for local development — it wires everything up correctly.

### Database

The SQLite database is created automatically at `DB_PATH` on first startup. To inspect it:

```bash
# Inside the running container
docker exec -it streamvault-backend-1 sqlite3 /app/streamvault.db

# Useful queries
sqlite> SELECT token, original_name, size_bytes, hls_ready FROM videos;
sqlite> SELECT COUNT(*) FROM videos;
```

### Adding a Feature — File Walkthrough

| What you want to change | Where to look |
|---|---|
| Add a new API endpoint | `src/main.rs` (route registration) + new handler in `src/handlers/` |
| Change database schema | `src/db.rs` (`migrate()` function) |
| Change what upload metadata is stored | `src/models.rs` + `src/db.rs` (`insert_video`) |
| Change HLS transcode settings | `src/streaming.rs` (FFmpeg args) |
| Change frontend UI | `frontend/index.html` |
| Change nginx config | `frontend/nginx.conf` |

---

## Building for Production

### Docker Image

The backend Dockerfile uses a two-stage build:

1. **Builder stage** (`rust:latest`) — compiles the binary. The dependency compilation is cached in a separate layer from source compilation, so only changed source files trigger a recompile.

2. **Runtime stage** (`rust:latest`) — copies only the compiled binary and installs `ffmpeg`. Using the same base image as the builder ensures GLIBC version compatibility.

```bash
# Build production image
docker build -t streamvault-backend:prod ./backend

# Check final image size
docker images streamvault-backend:prod
```

### Deploying to Fly.io (Free Tier)

```bash
# Install flyctl
brew install flyctl

# Authenticate
fly auth login

# From the project root
fly launch --name streamvault-api --region iad

# Set environment variables
fly secrets set BASE_URL=https://streamvault-api.fly.dev
fly secrets set UPLOAD_DIR=/data/uploads
fly secrets set DB_PATH=/app/streamvault.db

# Create a persistent volume for uploaded videos
fly volumes create video_data --size 5

# Deploy
fly deploy
```

See `docs/ARCHITECTURE.md` for full deployment architecture at every scale tier.

---

## Troubleshooting

### `docker compose up --build` fails on first run

**Symptom:** Build fails immediately without attempting compilation.

**Check:** Make sure Docker Desktop is running. `docker ps` should return without error.

---

### Backend exits with `unable to open database file`

**Cause:** The `DB_PATH` directory does not exist or is not writable.

**Fix:** The application creates parent directories automatically. If it's still failing, check that `DB_PATH` is not inside a Docker volume mount path. Use the default `/app/streamvault.db` or any path outside `/data/`.

```bash
# Check what the container sees
docker exec streamvault-backend-1 ls -la /app/
```

---

### Upload fails with `Error parsing multipart/form-data`

**Cause:** Axum's default 2MB body limit was hit. This is fixed in the current code with `DefaultBodyLimit::disable()` on the upload route. If you're seeing this, you may be running an old build.

**Fix:**
```bash
docker compose down -v
docker compose up --build
```

---

### Video plays but seeking is broken

**Cause:** The `Accept-Ranges: bytes` header is missing from stream responses, or nginx is buffering the response.

**Check:**
```bash
curl -I http://localhost/api/stream/{token}
# Should include: Accept-Ranges: bytes
```

**Fix:** Ensure `proxy_buffering off` is set in `nginx.conf` for the `/api/` location block.

---

### HLS never becomes ready

**Cause:** FFmpeg is not installed in the runtime container, or the transcode is failing silently.

**This is expected behaviour.** If FFmpeg fails, the system falls back to byte-range streaming. HLS is an enhancement, not a requirement. Check the logs:

```bash
docker compose logs backend | grep -i hls
docker compose logs backend | grep -i ffmpeg
```

---

### Port 80 is already in use

**Fix:** Change the port mapping in `docker-compose.yml`:

```yaml
ports:
  - "8080:80"   # Access at http://localhost:8080
```

---

## Known Limitations

| Limitation | Impact |
|---|---|
| Single video quality | No adaptive bitrate — slow connections may buffer |
| No upload resume | Failed uploads must restart from zero |
| No video expiry | Storage grows indefinitely — no cleanup job |
| No access revocation | Tokens cannot be invalidated after sharing |
| DB lost on `down -v` | Volume deletion wipes the database |
| No thumbnails | Video grid shows placeholder icons |
| SQLite single-writer | Contention above ~100 concurrent uploads |
| No video duration stored | `duration_secs` is always `null` in API responses |

See `docs/ARCHITECTURE.md` for resolution paths for each of these.
