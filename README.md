# StreamVault

A minimal private video streaming service. Upload any video file (up to 1GB) and receive a shareable link for immediate streaming — no account required.

**Stack:** Rust (Axum) · Svelte (SvelteKit) · SQLite · FFmpeg (optional HLS)

---

## Quick Start

### Docker (recommended)

```bash
docker compose up --build
# Open http://localhost
```

### Local Development

**Backend**
```bash
cd backend
cargo run
# Listens on :3000
```

**Frontend**
```bash
cd frontend
npm install
npm run dev
# Listens on :5173, proxies /api → :3000
```

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `BIND_ADDR` | `0.0.0.0:3000` | Server bind address |
| `UPLOAD_DIR` | `./uploads` | Directory for stored video files |
| `DATABASE_URL` | `sqlite:./streamvault.db` | SQLite database path |
| `BASE_URL` | `http://localhost:3000` | Public base URL (used in share links) |
| `RUST_LOG` | `streamvault=info` | Log level |

---

## API Reference

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/upload` | Upload video (multipart, field: `video`) |
| `GET` | `/api/videos` | List all uploaded videos |
| `GET` | `/api/videos/:token` | Get video metadata |
| `GET` | `/api/stream/:token` | HTTP byte-range streaming |
| `GET` | `/api/hls/:token/playlist.m3u8` | HLS playlist (post-transcode) |
| `GET` | `/api/hls/:token/:segment` | HLS segment |
| `GET` | `/health` | Health check |

---

## Architecture Overview

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for:
- Core design decisions
- Free vs. paid deployment architectures
- Technology trade-offs
- Horizontal scaling strategies

---

## How It Works

1. **Upload** — The browser streams the file to Axum via multipart. Axum writes in chunks (never buffers > 1MB at once). A UUID-based token is generated and stored in SQLite.

2. **Immediate streaming** — As soon as the upload write completes, the file is servable. The `/api/stream/:token` endpoint implements HTTP Range requests (RFC 7233), allowing the browser's native `<video>` element to seek and buffer independently.

3. **Background HLS transcode** — A `tokio::spawn` task shells out to `ffmpeg -c:v copy` (remux only, no re-encode) to produce 2-second `.ts` segments. This typically completes in seconds rather than minutes. Once ready, the player switches to HLS for consistent adaptive buffering.

4. **Share link** — `http://host/watch/:token` is the shareable URL. No authentication is required to view.
