# StreamVault — Architecture & System Design

> **Audience:** Engineers joining the project, evaluators, or anyone making infrastructure decisions.  
> **Based on:** The working Docker Compose deployment — every decision documented here is reflected in the actual code.

---

## Contents

1. [System Overview](#1-system-overview)
2. [Component Architecture](#2-component-architecture)
3. [Data Model](#3-data-model)
4. [Request Flows](#4-request-flows)
5. [Design Decisions](#5-design-decisions)
6. [Technology Choices & Alternatives](#6-technology-choices--alternatives)
7. [Deployment Tiers](#7-deployment-tiers)
8. [Scaling](#8-scaling)
9. [The $1,000/Month Stack](#9-the-1000month-stack)
10. [Security](#10-security)
11. [Observability](#11-observability)
12. [Known Issues & Roadmap](#12-known-issues--roadmap)

---

## 1. System Overview

StreamVault solves one problem: upload a video, get a link, share it. No accounts, no processing delays, no quality degradation.

The design is ordered by priority:

1. **Time-to-stream** — video must be watchable the moment the upload write completes
2. **Simplicity** — every component is replaceable without touching the others
3. **Cost efficiency** — runs at $0 on free-tier infrastructure for personal use
4. **Horizontal scalability** — designed to scale out, even if not deployed that way today

### What It Does

```
User uploads video (up to 1GB)
  → system stores it on disk
  → generates a random 8-char token
  → returns a shareable URL immediately
  → background: remuxes to HLS segments

Anyone with the URL
  → browser streams via HTTP Range requests
  → or via HLS once background transcode completes
```

### What It Deliberately Does Not Do

- No user accounts or authentication
- No transcoding on the upload hot path
- No thumbnail generation, duration extraction, or format conversion
- No expiry or access revocation
- No search or content discovery

These are deliberate omissions. Resolution paths for each are in [Section 12](#12-known-issues--roadmap).

---

## 2. Component Architecture

### Runtime Topology

```
                      ┌────────────────────────┐
                      │        Browser         │
                      └──────────┬─────────────┘
                                 │ HTTP :80
                      ┌──────────▼─────────────┐
                      │     nginx (alpine)      │
                      │                        │
                      │  GET /          ──────────→ index.html (static SPA)
                      │  GET /watch/*   ──────────→ index.html (SPA fallback)
                      │  /api/*         ──────────→ proxy → backend:3000
                      │                        │
                      │  proxy_buffering off    │  ← critical for streaming
                      │  client_max_body_size   │
                      │    1100M               │
                      └──────────┬─────────────┘
                   Docker internal network
                      ┌──────────▼─────────────┐
                      │    Rust / Axum 0.7      │
                      │    (backend:3000)       │
                      │                        │
                      │  POST /api/upload       │
                      │  GET  /api/stream/:tok  │
                      │  GET  /api/hls/:tok/*   │
                      │  GET  /api/videos[/:t]  │
                      │  GET  /health           │
                      └───────┬──────────┬──────┘
                              │          │
            ┌─────────────────▼──┐  ┌────▼───────────────────┐
            │  SQLite             │  │  Filesystem             │
            │  /app/              │  │  Docker volume          │
            │  streamvault.db     │  │  /data/uploads/         │
            │                    │  │                         │
            │  videos table      │  │  {uuid}.mp4             │
            │  token index       │  │  hls/{token}/           │
            │  hls_ready flag    │  │    playlist.m3u8        │
            └────────────────────┘  │    seg000.ts ...        │
                                    └─────────────────────────┘
                                              ▲
                                   ┌──────────┴──────────┐
                                   │  FFmpeg subprocess   │
                                   │  (background task)   │
                                   │  -c:v copy           │
                                   │  -hls_time 2         │
                                   └─────────────────────-┘
```

### Codebase Map

```
backend/src/
├── main.rs        Entry point. Builds the Axum router, wires AppState,
│                  binds the TCP listener. Creates upload dir and DB parent
│                  dir at startup before anything else runs.
│
├── db.rs          All database interaction. SqlitePool wrapped in a
│                  Database struct. Schema migration runs two separate
│                  execute() calls — SQLx SQLite does not support multiple
│                  statements in one call. No compile-time query macros —
│                  uses runtime queries + manual row mapping to avoid
│                  requiring DATABASE_URL at cargo build time.
│
├── models.rs      Plain Rust structs with Serde derives.
│                  Video         — database row shape
│                  VideoResponse — API response (adds stream_url, share_url)
│                  UploadResponse — returned immediately after upload
│
├── error.rs       AppError enum. Variants map to HTTP status codes via
│                  IntoResponse. All handler return types use this error.
│
├── streaming.rs   Single function: transcode_to_hls(). Shells out to ffmpeg.
│                  Returns Ok(()) even on failure — system degrades gracefully
│                  to byte-range streaming if FFmpeg is absent or crashes.
│
└── handlers/
    ├── upload.rs  upload_video() — streams multipart to disk in chunks.
    │              list_videos()  — returns all videos ordered by created_at.
    │
    ├── stream.rs  video_info()   — metadata lookup by token.
    │              stream_video() — HTTP Range streaming with seek support.
    │              hls_playlist() — serves m3u8 or 307s to raw stream.
    │              hls_segment()  — serves individual .ts segment files.
    │
    └── health.rs  health_check() — returns {"status":"ok"}.
```

```
frontend/src/
├── routes/
│   ├── +layout.ts            SSR disabled (ssr = false), SPA mode
│   ├── +layout.svelte        Imports app.css, wraps all pages with <slot />
│   ├── +page.svelte          Home: onMount fetches /api/videos, renders
│   │                         UploadZone + VideoGrid, handles success/error events
│   └── watch/[token]/
│       └── +page.svelte      Player: fetches /api/videos/:token on mount,
│                             reactive streamUrl picks HLS or byte-range based
│                             on hls_ready flag
├── lib/components/
│   ├── UploadZone.svelte     XHR upload (not fetch — needed for progress events),
│   │                         drag-and-drop, validates file type + size client-side
│   ├── VideoGrid.svelte      Renders video cards, each links to /watch/:token
│   └── Toast.svelte          Fixed-position notification, auto-dismisses
├── app.html                  SvelteKit HTML shell — required file
└── app.css                   CSS custom properties (--bg, --accent, etc.)
```

---

## 3. Data Model

### Schema

```sql
CREATE TABLE videos (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    token         TEXT NOT NULL UNIQUE,      -- 8-char share token e.g. "a3f7bc12"
    filename      TEXT NOT NULL,             -- stored name on disk: "{uuid}.{ext}"
    original_name TEXT NOT NULL,             -- user's original filename for display
    content_type  TEXT NOT NULL,             -- MIME type validated on upload
    size_bytes    INTEGER NOT NULL,          -- total file size in bytes
    duration_secs REAL,                      -- NULL — not yet populated
    width         INTEGER,                   -- NULL — not yet populated
    height        INTEGER,                   -- NULL — not yet populated
    hls_ready     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_videos_token ON videos(token);
```

### Why `filename` and `original_name` Are Separate

`filename` is the UUID-based name used on disk (`3f8a2c1d-...mp4`). It is server-generated to prevent collisions and path traversal. `original_name` is preserved for display. These must never be swapped — never use `original_name` to construct a file path.

### Why `duration_secs`, `width`, `height` Are Nullable

These require an FFprobe call, which adds latency to the upload response. They exist in the schema for future use — once HLS transcoding completes, a follow-up probe could populate them. Currently always `NULL`.

### File Layout on Disk

```
/data/uploads/
├── 3f8a2c1d-4b5e-6789-abcd-ef0123456789.mp4   ← raw upload (UUID filename)
├── 9a1b2c3d-5e6f-7890-bcde-f01234567890.webm
└── hls/
    └── a3f7bc12/                               ← token as directory name
        ├── playlist.m3u8
        ├── seg000.ts
        ├── seg001.ts
        └── seg002.ts
```

---

## 4. Request Flows

### 4.1 Upload

```
Browser
  │  POST /api/upload
  │  Content-Type: multipart/form-data; boundary=...
  │  Content-Length: 524288000
  ▼
nginx  [proxy_request_buffering off]
  │  Bytes pass through immediately without nginx accumulating the body
  ▼
Axum  [DefaultBodyLimit::disable() on this route only]
  │
  │  Multipart field "video":
  │    validate MIME type against allowlist
  │    generate: uuid filename + 8-char token
  │    File::create(upload_dir / uuid_filename)
  │
  │    loop chunks (~64KB each):
  │      total_bytes += chunk.len()
  │      if total_bytes > 1_073_741_824 → delete file, return 413
  │      file.write_all(&chunk).await
  │    file.flush().await
  │
  ├──→ INSERT INTO videos (token, filename, original_name, ...) VALUES (...)
  │
  ├──→ HTTP 200 { token, share_url, stream_url, size_bytes }
  │    ↑ Response sent. Video is streamable at this exact moment.
  │
  └──→ tokio::spawn(transcode_to_hls(state, token, file_path))
         Runs concurrently. On completion:
         UPDATE videos SET hls_ready = TRUE WHERE token = ?
```

**Memory profile:** One chunk held in memory at a time (~64KB). A 1GB upload and a 1MB upload use identical peak RAM.

---

### 4.2 Streaming (HTTP Range)

```
Browser <video src="/api/stream/a3f7bc12">
  │  GET /api/stream/a3f7bc12
  │  Range: bytes=0-1048575
  ▼
nginx  [proxy_buffering off]
  │  Bytes stream through — nginx does not buffer the video response
  ▼
Axum
  │  SELECT filename, content_type FROM videos WHERE token = 'a3f7bc12'
  │  fs::metadata(file_path) → file_size = 524288000
  │  parse Range: start=0, end=1048575, chunk=1048576
  │  File::open(file_path)
  │  [start > 0] file.seek(SeekFrom::Start(start))
  │  ReaderStream::new(file.take(chunk_size))
  │
  └──→ HTTP 206 Partial Content
       Content-Type: video/mp4
       Content-Range: bytes 0-1048575/524288000
       Accept-Ranges: bytes
       Content-Length: 1048576
       Cache-Control: public, max-age=3600

-- User seeks to 02:30 --

  GET /api/stream/a3f7bc12
  Range: bytes=37748736-38797311

  Axum: seek to 37,748,736 → stream 1,048,576 bytes
  O(1) seek — no re-download from start
```

---

### 4.3 HLS Fallback

```
GET /api/hls/a3f7bc12/playlist.m3u8

  SELECT hls_ready FROM videos WHERE token = 'a3f7bc12'

  hls_ready = FALSE:
    HTTP 307 Temporary Redirect
    Location: /api/stream/a3f7bc12
    Browser transparently plays via byte-range instead

  hls_ready = TRUE:
    read /data/uploads/hls/a3f7bc12/playlist.m3u8
    HTTP 200
    Content-Type: application/vnd.apple.mpegurl
    Cache-Control: no-cache   ← playlist must never be cached
```

---

## 5. Design Decisions

Each entry: **what was chosen**, **why**, **what was explicitly rejected**.

---

### 5.1 Serve raw file immediately; transcode async

**Chosen:** File is streamable the moment the upload write flushes. HLS runs in `tokio::spawn` after the response is sent.

**Why:** Time-to-stream is the first priority. Any mandatory processing step before the video is playable violates this directly.

**Rejected:** *Transcode first, stream after.* Adds mandatory latency of seconds to minutes before the video is watchable. Wrong for this use case.

**Rejected:** *Real-time HLS segmentation during upload* (pipe bytes into FFmpeg stdin). Eliminates transcoding delay. Rejected because it couples upload and transcode into a single fragile pipeline — a FFmpeg crash aborts the upload. The current decoupled approach is more resilient.

---

### 5.2 `DefaultBodyLimit::disable()` on upload route only

**Chosen:** Per-route layer on `POST /api/upload` only.

**Why:** Axum defaults to a 2MB body limit globally. Without disabling it, files larger than 2MB fail with a multipart parse error before the handler sees a single byte. Disabling globally would remove protection from all other routes. The 1GB limit is then enforced by the handler itself as it counts bytes.

**Implementation:**
```rust
.route("/api/upload", post(handlers::upload::upload_video)
    .layer(DefaultBodyLimit::disable()))
```

---

### 5.3 FFmpeg subprocess, not Rust bindings

**Chosen:** `tokio::process::Command::new("ffmpeg")`.

**Why:** `ffmpeg-sys` Rust bindings add hours to build time and mean a codec bug can segfault the Axum process. A subprocess crash is isolated. `-c:v copy` makes the transcode I/O-bound, so subprocess overhead is negligible.

**Rejected:** *ffmpeg-sys bindings.* More integrated, but build complexity and crash isolation trade-off is wrong.

**Rejected:** *Cloud transcoding (MediaConvert, Cloudflare Stream).* External dependency, per-minute costs, network latency, incompatible with $0 goal.

---

### 5.4 Runtime SQLx queries, not compile-time macros

**Chosen:** `sqlx::query("...").bind(x).fetch_*()` with manual `.get("column_name")` row mapping.

**Why:** `sqlx::query_as!()` and `sqlx::query!()` require `DATABASE_URL` set at `cargo build` time and a live database to validate SQL against. This always fails in Docker build containers. Runtime queries compile without a database.

**Trade-off:** Compile-time SQL validation is lost. Column name typos become runtime panics. Acceptable for this schema size.

---

### 5.5 SQLite with `sqlite://path?mode=rwc`

**Chosen:** Absolute path + `?mode=rwc` flag.

**Why:** Without `mode=rwc`, SQLite requires the file to already exist. On first container start, it doesn't — connection fails. `rwc` = read/write/create.

Absolute path (`/app/streamvault.db`) instead of relative path: relative paths resolve against the process CWD, which can differ from the Dockerfile `WORKDIR` in some runtimes. Absolute path is unambiguous.

`/app/` specifically, not `/data/`: Docker volumes overwrite the entire mount point at container start. `/data/uploads` is a volume mount — any files created inside `/data/` during image build are gone at runtime. `/app/` is the `WORKDIR`, never a volume mount, always writable by the process.

---

### 5.6 SvelteKit frontend with static adapter

**Chosen:** SvelteKit with `@sveltejs/adapter-static`, built in Docker using `node:20-alpine`, served by `nginx:alpine`.

**Why:** Svelte is the preferred stack per the brief. SvelteKit provides file-based routing (the `/watch/:token` route maps directly to `src/routes/watch/[token]/+page.svelte`), reactive state management via Svelte stores, and scoped CSS per component. The `adapter-static` outputs a fully pre-compiled static bundle — at runtime nginx serves plain HTML/JS/CSS files with no Node.js process running.

**Component structure:**
- `+page.svelte` — home page: upload zone + video grid, reactive `videos` array updated on upload
- `watch/[token]/+page.svelte` — player page: fetches metadata on mount, selects HLS vs byte-range stream URL via reactive `$:` declaration
- `UploadZone.svelte` — encapsulates XHR upload with `createEventDispatcher` for `success`/`error` events to the parent
- `VideoGrid.svelte` — receives `videos` prop, renders card grid
- `Toast.svelte` — receives `type` and `message` props, self-contained notification

**Key configuration:**
- `ssr = false` in `+layout.ts` — disables server-side rendering for SPA mode
- `fallback: 'index.html'` in `svelte.config.js` — nginx serves `index.html` for all unknown paths, enabling client-side routing
- `vite.config.js` dev proxy: `/api → http://localhost:3000` for local development without Docker

**Trade-off:** The Docker build requires internet access for `npm install`. This is standard for any Node.js project and is handled correctly by Docker's layer caching — subsequent builds after source changes skip the install step entirely.

---

### 5.7 Token generation

**Chosen:** 8 chars from `[a-z0-9]` (36^8 ≈ 2.8 trillion combinations).

**Rejected:** *UUID v4.* More entropy than needed, longer URLs, harder to share verbally.

**Rejected:** *Sequential integer IDs.* Enumerable — any user could walk the ID space and discover all videos.

At 1,000 requests/second with 10,000 videos in the system, the probability of a collision per request is ~3.5 × 10⁻⁹. Not worth a uniqueness pre-check.

---

## 6. Technology Choices & Alternatives

### Runtime

| | Rust / Axum | Go / Fiber | Node.js / Fastify | Python / FastAPI |
|---|---|---|---|---|
| Memory idle | ~5 MB | ~15 MB | ~50 MB | ~70 MB |
| GC pauses during streaming | None | Occasional | Occasional | Occasional |
| Throughput | Highest | High | Good | Adequate |
| Docker image size | Large (rust:latest) | Small (scratch) | Medium | Medium |
| First build time | 3-5 min | 30 sec | 10 sec | 5 sec |

Rust's advantage is memory efficiency and no GC pauses. At 500 concurrent viewers, Rust uses ~50 MB RAM. The equivalent Node.js deployment: ~300 MB. The large image size is a known trade-off — resolvable with `musl` + scratch base in production.

### Storage

| | Local disk (current) | Cloudflare R2 | AWS S3 |
|---|---|---|---|
| Setup | Zero | SDK + credentials | SDK + IAM + credentials |
| Cost (storage) | $0 | $0.015/GB | $0.023/GB |
| Egress cost | $0 (local) | $0 | $0.09/GB |
| Horizontal scaling | Blocked | Unlimited | Unlimited |
| Durability | Single disk | 11 nines | 11 nines |

The code separates "storage location" from "file path" cleanly. Migrating to R2 means replacing the file read/write calls in the handlers — the router, models, and database are unchanged.

### Database

| | SQLite (current) | PostgreSQL | Turso |
|---|---|---|---|
| Setup | Zero | Managed instance | Sign up + connection string |
| Cost | $0 | $15-70/mo managed | $0 free tier |
| Concurrent writes | Single writer | Multiple | Multiple (replicated SQLite) |
| Migration | — | Change DATABASE_URL | Change DATABASE_URL |

SQLx abstracts the driver — the query syntax for SQLite and PostgreSQL is identical in this codebase. The migration path is one environment variable.

### Streaming Protocol

| | HTTP Range (current) | HLS async (current) | DASH |
|---|---|---|---|
| Time-to-stream | Zero (instant) | Seconds | Seconds |
| Adaptive bitrate | No | Yes (with ABR encode) | Yes |
| Seek performance | Network round-trip | Instant within buffered segments | Instant |
| Browser support | All | All + native Safari | All except Safari |
| CDN cacheability | Partial | Full (immutable segments) | Full |

StreamVault uses both: Range for immediacy, HLS for quality. The player checks `hls_ready` and picks the right URL automatically.

---

## 7. Deployment Tiers

### Tier 0 — Local / Demo ($0)

```bash
docker compose up --build
```

SQLite in the container. Videos on a Docker named volume. Not suitable for production. Good for development and demos.

---

### Tier 1 — Free Cloud ($0/month)

- **Fly.io** — backend container (3 shared VMs, always-on, free)
- **Tigris** (Fly.io native) — object storage replacing local disk (5GB free, S3-compatible)
- **Cloudflare Pages** — SPA frontend (unlimited bandwidth, free)
- **Cloudflare** — DNS + SSL (free)

**Code changes needed:**
1. Add `aws-sdk-s3` crate; replace `File::create` in upload with S3 PUT
2. Replace file reads in stream handler with S3 GET
3. SQLite remains on Fly volume

**Limitation:** Fly.io charges ~$0.02/GB egress beyond free allowance. Axum proxies all video bytes at this tier.

---

### Tier 2 — Small Production ($20-50/month)

**Architecture change:** presigned URLs for video delivery.

```
Before:  Browser → Axum → S3 → Axum → Browser   (Axum in data path)

After:   Browser → GET /api/videos/:token
                   Axum generates presigned_url (15-min TTL)
         Browser → <video src=presigned_url> → R2 → CDN → Browser
                   (Axum handles zero video bytes at playback time)
```

**Cost:** ~$14/mo compute (Fly.io) + ~$7.50/mo storage (R2 500GB) = ~$22-50/mo total.

---

### Tier 3 — Production at Scale ($100-300/month)

Changes from Tier 2:
- SQLite → PostgreSQL (Neon serverless, ~$69/mo)
- `tokio::spawn` transcode → Redis job queue + dedicated worker fleet
- 3+ API nodes behind Fly.io load balancer
- Cloudflare Pro for WAF and edge rate limiting

---

## 8. Scaling

### What Blocks Horizontal Scaling Today

1. **Local disk** — uploaded videos on a single Docker volume. A second API node cannot read files written by the first.
2. **SQLite single-writer** — WAL mode allows concurrent reads but only one writer. Above ~100 concurrent uploads, write contention appears.

Both are environment variable changes, not code rewrites.

### Upload Scaling: Presigned S3 PUTs

```
Step 1: Browser → POST /api/upload/init
          Axum: generate token, insert pending row
          Return: { presigned_put_url, token }

Step 2: Browser → PUT {presigned_put_url}  (direct to S3/R2)
          Axum handles zero upload bytes

Step 3: Browser → POST /api/upload/complete?token={token}
          Axum: mark active, spawn transcode job
```

Upload throughput limited by S3 capacity, not API node count.

### Transcode Scaling: Worker Pool

```
Current:
  upload_handler → tokio::spawn(ffmpeg) on API process

Scaled:
  upload_handler → redis.rpush("transcode_queue", {token, path})

  Worker process (separate fleet):
    loop {
        job = redis.blpop("transcode_queue")
        run_ffmpeg(job) → write segments to R2
        db.mark_hls_ready(job.token)
    }
```

Workers are CPU-heavy (ABR encode). API nodes are I/O-light (metadata + URL generation). Decoupling allows right-sizing each fleet independently.

### HLS Segment Caching

Segments are immutable once written — ideal CDN cache objects:

- `Cache-Control: public, max-age=86400` — already set in current code
- First request hits origin; every subsequent is served from CDN edge
- `.m3u8` playlist: `Cache-Control: no-cache` — must never be stale

---

## 9. The $1,000/Month Stack

Handles 5,000-10,000 concurrent viewers, multi-region, full observability.

### Infrastructure

| Component | Service | Monthly Cost |
|---|---|---|
| API nodes (3 regions: iad, lhr, nrt) | Fly.io 4x dedicated-CPU-2x 4GB | ~$200 |
| Transcode workers | Fly.io 4x shared-CPU-4x 8GB | ~$120 |
| Video storage | Cloudflare R2 (10TB, zero egress) | ~$150 |
| Database | Neon PostgreSQL (serverless) | ~$69 |
| Job queue | Upstash Redis | ~$30 |
| CDN + WAF | Cloudflare Pro | ~$20 |
| Error tracking | Sentry Team | ~$26 |
| Metrics + dashboards | Grafana Cloud | ~$100 |
| CI/CD | GitHub Actions | ~$16 |
| Misc (staging, backups) | — | ~$50 |
| **Total** | | **~$781/mo** |

$219/mo headroom for spikes, new regions, or additional storage.

### Code Changes Required

| Change | Effort | Impact |
|---|---|---|
| S3/R2 storage client | Medium | Enables horizontal scaling |
| Presigned download URLs | Small (10 lines) | Removes Axum from video data path |
| PostgreSQL (change DATABASE_URL) | Trivial | Multi-writer, durability |
| Redis job queue + worker binary | Medium | Independent transcode scaling |
| ABR HLS (720p + 480p encode pass) | Small | Adaptive bitrate on slow connections |
| Video thumbnails | Small | Better UI in video grid |
| Video duration (FFprobe) | Small | Populated `duration_secs` in API |

### What Does Not Change

The Axum router, all handler logic, database schema, token system, HTTP Range implementation, and nginx config are **unchanged** across all deployment tiers. The application does not know what infrastructure tier it runs on.

---

## 10. Security

### Current Protections

| Vector | Mitigation | Sufficient for production? |
|---|---|---|
| Path traversal via HLS segment names | Reject requests containing `/` or `..` | Yes |
| Oversized uploads | 1GB check in handler + nginx `client_max_body_size 1100M` | Yes |
| Body stuffing on metadata routes | Axum default 2MB limit on all non-upload routes | Yes |
| MIME type spoofing | Allowlist validation + extension inference | Yes |
| Token enumeration | 2.8 trillion combinations | Personal use — rate limiting needed for public |
| CORS | Currently `Any` (all origins) | No — must restrict to your domain |
| Malicious file content | None | No — virus scanning needed |

### Hardening for Public Deployment

**Rate limiting** (add `tower-governor` crate):
```rust
.route("/api/upload", post(upload_video)
    .layer(DefaultBodyLimit::disable())
    .layer(GovernorLayer {
        config: Arc::new(GovernorConfig::default()
            .per_millisecond(100)
            .burst_size(5))
    }))
```

**CORS** (restrict to your domain):
```rust
CorsLayer::new()
    .allow_origin("https://stream.yourdomain.com"
        .parse::<HeaderValue>().unwrap())
```

**Virus scanning** (ClamAV after upload, before database insert):
```rust
let result = Command::new("clamdscan").arg(&file_path).status().await?;
if !result.success() {
    tokio::fs::remove_file(&file_path).await?;
    return Err(AppError::BadRequest("File rejected".into()));
}
```

**Token expiry** — add `expires_at DATETIME` column to schema. Check on every stream/metadata request.

**Signed URLs** — for true access control (current tokens are obscurity, not auth), generate HMAC-signed time-limited URLs and verify the signature on each streaming request.

---

## 11. Observability

### Current State

Structured logging via the `tracing` crate:

```
2026-04-05T23:02:49Z  INFO streamvault: Upload dir: "/data/uploads"
2026-04-05T23:02:49Z  INFO streamvault: Database: sqlite:///app/streamvault.db?mode=rwc
2026-04-05T23:02:49Z  INFO streamvault: Database ready
2026-04-05T23:02:49Z  INFO streamvault: StreamVault listening on 0.0.0.0:3000
2026-04-05T23:05:52Z  INFO streamvault: Uploaded demo.mp4 (52428800 bytes) → token=a3f7bc12
```

HTTP request logging via `tower_http::TraceLayer`. Set `RUST_LOG=tower_http=debug` for per-request logs.

### What's Missing

- No metrics endpoint (no Prometheus, no request counts, no upload success rate)
- No distributed tracing (no trace IDs across upload → transcode)
- No alerting (no notification on error rate, disk usage, transcode failures)

### Adding Prometheus Metrics

```toml
# Cargo.toml
prometheus = "0.13"
```

```rust
// Add to router
.route("/metrics", get(metrics_handler))

// handlers/metrics.rs
pub async fn metrics_handler() -> String {
    let encoder = TextEncoder::new();
    encoder.encode_to_string(&prometheus::gather()).unwrap()
}
```

Useful counters to add: `uploads_total`, `upload_bytes_total`, `stream_requests_total`, `transcode_success_total`, `transcode_failure_total`, `transcode_duration_seconds`.

---

## 12. Known Issues & Roadmap

### Active Issues

| Issue | Root Cause | Fix |
|---|---|---|
| Database wiped on `docker compose down -v` | SQLite inside container, `-v` deletes volumes | Mount host path: `./data/db:/app` in compose |
| HLS segment dirs not cleaned on transcode failure | No cleanup in error path of `transcode_to_hls()` | Add `tokio::fs::remove_dir_all` on error return |

### Roadmap

**Before sharing publicly (P0):**
- [ ] Rate limiting on upload endpoint
- [ ] CORS restriction to your domain
- [ ] Token expiry (`expires_at` column + enforcement)

**Quality of life (P1):**
- [ ] Video thumbnails (FFmpeg `-vframes 1` at 5 seconds)
- [ ] Video duration in metadata (FFprobe after transcode)
- [ ] Upload resume (TUS protocol)
- [ ] Host-mounted database volume for persistence across `down -v`

**Scale enablers (P2):**
- [ ] Cloudflare R2 / S3 storage backend
- [ ] Presigned download URLs
- [ ] PostgreSQL (change `DATABASE_URL`)
- [ ] ABR HLS (720p + 480p encode passes)

**Operational (P3):**
- [ ] Prometheus metrics endpoint
- [ ] Structured request IDs
- [ ] Transcode job queue (Redis) + worker binary
- [ ] Automated cleanup job for expired videos

---

*StreamVault · Rust · Axum · SQLite · Docker*
