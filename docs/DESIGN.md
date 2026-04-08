# StreamVault — System Design

> This document describes the **current implementation**: how it works, why each piece was
> built the way it was, what was measured, and where the edges are.
> For the production scaling roadmap, see [ARCHITECTURE.md](./ARCHITECTURE.md).

---

## Contents

1. [The Core Problem](#1-the-core-problem)
2. [Component Map](#2-component-map)
3. [Data Model](#3-data-model)
4. [Request Flows](#4-request-flows)
5. [Design Decisions](#5-design-decisions)
6. [Performance Characteristics](#6-performance-characteristics)
7. [Edge Cases & Failure Modes](#7-edge-cases--failure-modes)
8. [Security Model](#8-security-model)
9. [Observability](#9-observability)
10. [Known Limitations](#10-known-limitations)

---

## 1. The Core Problem

The spec prioritises **time-to-stream** over video quality. This creates a direct conflict:
any processing step that runs before the video is watchable violates the primary requirement.

The central design question is: *how do you offer both instant availability and good streaming quality?*

**Answer: dual-streaming protocol.**

```
Upload completes
    │
    ├──→ HTTP byte-range stream   (available: immediately)
    │    Raw file, served with Accept-Ranges: bytes
    │    Seek support: O(1) via file offset
    │
    └──→ tokio::spawn(ffmpeg)     (available: ~1-10s later)
         Remux → HLS segments
         hls_ready = TRUE once complete
         Player upgrades silently
```

The user starts watching before FFmpeg has processed a single frame. The HLS upgrade
happens invisibly in the background.

---

## 2. Component Map

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
│   ├── +layout.svelte        Imports app.css, wraps all pages
│   ├── +page.svelte          Home: upload zone + video grid
│   └── watch/[token]/
│       └── +page.svelte      Player: HLS vs byte-range selection, polling
├── lib/components/
│   ├── UploadZone.svelte     XHR upload with progress events (not fetch)
│   ├── VideoGrid.svelte      Video card grid
│   └── Toast.svelte          Auto-dismissing notifications
```

---

## 3. Data Model

### Schema

```sql
CREATE TABLE videos (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    token         TEXT NOT NULL UNIQUE,      -- 8-char share token e.g. "a3f7bc12"
    filename      TEXT NOT NULL,             -- UUID on disk: prevents collisions + traversal
    original_name TEXT NOT NULL,             -- user's filename, for display only
    content_type  TEXT NOT NULL,             -- MIME type validated on upload
    size_bytes    INTEGER NOT NULL,
    duration_secs REAL,                      -- NULL: requires FFprobe, adds latency
    width         INTEGER,                   -- NULL: same reason
    height        INTEGER,                   -- NULL: same reason
    hls_ready     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_videos_token ON videos(token);
```

### Why `filename` ≠ `original_name`

`filename` is a server-generated UUID (`3f8a2c1d-...mp4`). It is the only value ever
used to construct a file path. `original_name` is preserved for display and never
touches the filesystem. Swapping them would introduce a path traversal vulnerability —
a user could upload a file named `../../etc/passwd`.

### SQLite Tuning

```sql
PRAGMA journal_mode=WAL;        -- readers don't block writer; writer doesn't block readers
PRAGMA synchronous=NORMAL;      -- crash-safe without full fsync cost
PRAGMA cache_size=-8000;        -- 8MB page cache (default ~2MB) to reduce disk I/O
```

WAL mode is critical here: during an upload, the writer is inserting a row while concurrent
readers may be serving the video list or metadata. Without WAL, every read blocks until the
write completes.

### File Layout

```
/data/uploads/
├── 3f8a2c1d-4b5e-6789-abcd-ef0123456789.mp4   ← raw upload (UUID filename)
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
  │  Content-Type: multipart/form-data
  ▼
nginx  [proxy_request_buffering off]
  │  Bytes pass through immediately — no nginx accumulation
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
  ├──→ INSERT INTO videos (token, filename, ...)
  │
  ├──→ HTTP 200 { token, share_url, stream_url }
  │    ↑ Video is streamable at this exact moment.
  │
  └──→ tokio::spawn(transcode_to_hls(...))
         Background. Response already sent.
         On complete: UPDATE videos SET hls_ready = TRUE
```

**Memory profile:** One chunk held in memory at a time (~64KB). A 1GB upload and a 1MB
upload use identical peak RAM. See [benchmarks/RESULTS.md](../benchmarks/RESULTS.md).

### 4.2 Byte-Range Streaming

```
Browser <video src="/api/stream/a3f7bc12">
  │  GET /api/stream/a3f7bc12
  │  Range: bytes=0-1048575
  ▼
nginx  [proxy_buffering off]
  ▼
Axum
  │  SELECT filename, content_type FROM videos WHERE token = ?
  │  fs::metadata(file_path) → file_size
  │  parse Range header → start, end
  │  File::open(file_path)
  │  file.seek(SeekFrom::Start(start))   ← O(1) seek
  │  ReaderStream::new(file.take(chunk))
  │
  └──→ HTTP 206 Partial Content
       Content-Range: bytes 0-1048575/{file_size}
       Accept-Ranges: bytes
       Cache-Control: public, max-age=3600

-- User seeks to 02:30 --
  GET /api/stream/a3f7bc12
  Range: bytes=37748736-38797311
  → file.seek(37748736) in O(1) — no re-download
```

### 4.3 HLS Playlist (with Fallback)

```
GET /api/hls/a3f7bc12/playlist.m3u8

  SELECT hls_ready FROM videos WHERE token = ?

  hls_ready = FALSE:
    HTTP 307 Temporary Redirect
    Location: /api/stream/a3f7bc12
    Cache-Control: no-store          ← must not be cached (client re-checks next poll)
    Player falls back to byte-range transparently

  hls_ready = TRUE:
    read /data/uploads/hls/a3f7bc12/playlist.m3u8
    HTTP 200
    Content-Type: application/vnd.apple.mpegurl
    Cache-Control: no-cache
```

### 4.4 Watch Page: Protocol Selection

```
onMount:
  meta = fetch /api/videos/{token}
  streamUrl = meta.hls_ready
    ? /api/hls/{token}/playlist.m3u8
    : /api/stream/{token}

every 3 seconds (while !hls_ready):
  meta = fetch /api/videos/{token}
  if meta.hls_ready:
    streamUrl = /api/hls/{token}/playlist.m3u8
    stop polling
```

The status chip updates from "Direct stream" (amber) to "HLS ready" (green) without a
page reload.

---

## 5. Design Decisions

### 5.1 Serve raw file immediately; transcode async

**Constraint:** Time-to-stream is the first priority.

**Decision:** File is streamable the moment the upload write flushes. HLS runs in
`tokio::spawn` after the HTTP response is sent.

**Rejected — transcode first, stream after:** Adds mandatory latency of seconds to
minutes before the video is watchable. Directly violates the primary constraint.

**Rejected — real-time HLS during upload (pipe stdin):** Eliminates transcoding delay
but couples upload and transcode into a single pipeline. A FFmpeg crash aborts the
upload. The decoupled approach is more resilient.

---

### 5.2 FFmpeg remux with `-c:v copy`

**Constraint:** HLS must be ready in seconds, not minutes.

**Decision:** `-c:v copy` copies the video bitstream verbatim into MPEG-TS segments
without decoding or re-encoding. The operation is I/O-bound.

```
Remux time  ≈ file_size / disk_throughput  ≈ 500MB / ~80MB/s  ≈  6-8 seconds
Re-encode   ≈ duration × encode_factor    ≈ 12min × 0.9×RT   ≈  10+ minutes
```

**Trade-off accepted:** Single output quality. No adaptive bitrate. The spec
prioritises availability over quality, so this is correct.

**Rejected — cloud transcoding (AWS MediaConvert, Cloudflare Stream):** External
dependency, per-minute cost, network round-trip, incompatible with $0 goal.

---

### 5.3 FFmpeg subprocess, not Rust bindings

**Decision:** `tokio::process::Command::new("ffmpeg")`.

**Rejected — ffmpeg-sys bindings:** A codec bug or assertion failure in a C library
would segfault the Axum process, taking down the API for all users. A subprocess crash
is isolated. `-c:v copy` makes the subprocess overhead negligible.

---

### 5.4 `DefaultBodyLimit::disable()` on upload route only

**Decision:** Per-route layer:

```rust
.route("/api/upload", post(handlers::upload::upload_video)
    .layer(DefaultBodyLimit::disable()))
```

Axum defaults to a 2MB body limit globally. Without this, files larger than 2MB fail
with a multipart parse error before the handler sees a single byte. Disabling globally
removes protection from all other routes. The 1GB limit is enforced in the handler by
counting bytes as they arrive.

---

### 5.5 Runtime SQLx queries (not compile-time macros)

**Decision:** `sqlx::query("...").bind(x).fetch_*()` with manual row mapping.

`sqlx::query_as!()` requires `DATABASE_URL` set at `cargo build` time and a live
database to validate SQL against. This always fails in Docker multi-stage build
containers. Runtime queries compile without a database.

**Trade-off accepted:** Compile-time SQL validation is lost. Column name typos become
runtime errors. Acceptable for this schema size — the loss is mitigated by integration
tests and a small number of queries.

---

### 5.6 SQLite absolute path + `?mode=rwc`

Without `mode=rwc`, SQLite requires the file to already exist. On first container start
it doesn't — connection fails. `rwc` = read/write/create.

Absolute path (`/app/streamvault.db`): relative paths resolve against process CWD,
which can differ from `WORKDIR` in some runtimes. `/app/` is never a volume mount —
any file created there survives container restarts.

---

### 5.7 SvelteKit with `adapter-static`

Outputs a fully pre-compiled static bundle. At runtime, nginx serves HTML/JS/CSS with
no Node.js process. This eliminates an entire runtime service and enables trivial CDN
deployment (just upload the build output to any CDN).

**Key config:**
- `ssr = false` in `+layout.ts` — SPA mode, client-side routing only
- `fallback: 'index.html'` — nginx serves `index.html` for all unknown paths (handles `/watch/:token` deep links)

---

### 5.8 Token generation

8 chars from `[a-z0-9]` → 36^8 ≈ **2.8 trillion** combinations.

**Rejected — UUID v4:** More entropy than needed, longer URLs.
**Rejected — Sequential integers:** Enumerable — any user could walk the ID space.

At 1,000 uploads/second, the probability of a collision per insert is ~3.5 × 10⁻⁹.
No uniqueness pre-check needed — SQLite's UNIQUE constraint catches the collision and
returns a 500 (acceptable at this traffic level).

---

## 6. Performance Characteristics

Full benchmark methodology and results: [benchmarks/RESULTS.md](../benchmarks/RESULTS.md)

### Summary Table

| Metric | Value | How it's achieved |
|---|---|---|
| Time-to-stream | < 20ms after upload | File on disk + DB row = streamable. No queue. |
| HLS ready (50MB file) | ~0.6s | `-c:v copy` is I/O-bound, not CPU-bound |
| HLS ready (500MB file) | ~8s | Linear with file size at disk throughput |
| Seek latency | O(1), ~8-15ms | `file.seek(SeekFrom::Start(offset))` |
| Upload RAM (1GB file) | ~64KB peak | Chunk-based write loop; chunk drops each iteration |
| p95 response (20 viewers) | < 35ms | Tokio async tasks, not threads |

### Why Seek Is O(1)

The HTTP Range handler does exactly this:
```rust
if start > 0 {
    file.seek(SeekFrom::Start(start)).await?;
}
```

The OS kernel locates the file offset in O(1) via the inode. There is no scan.
A seek to the middle of a 1GB file is identical in cost to a seek to byte 0.

### Why HLS Segments Are Cached Aggressively

```
Cache-Control: public, max-age=86400, immutable
```

Segments are named `seg000.ts`, `seg001.ts` — they are written once and never modified.
The `immutable` directive tells browsers and CDN edge nodes they will never change.
On a CDN deployment, any segment served once is cached at the edge and never hits origin again.

The playlist (`playlist.m3u8`) uses `Cache-Control: no-cache` — it must always be
fresh because the `hls_ready` transition can happen at any point.

---

## 7. Edge Cases & Failure Modes

### 7.1 Upload interrupted mid-stream

If the browser cancels the upload (network drop, user closes tab), Axum's multipart
loop returns an error. The partial file is deleted:

```rust
if let Err(e) = result {
    let _ = tokio::fs::remove_file(&file_path).await;
    return Err(e);
}
```

The database row is never inserted (the INSERT only runs after a successful write), so
the token doesn't exist. The partial file doesn't linger.

### 7.2 FFmpeg crashes or is not installed

`transcode_to_hls()` returns `Ok(())` on any FFmpeg failure — it never propagates the
error to the caller. The database row stays with `hls_ready=FALSE`. The video is fully
watchable via byte-range streaming — HLS is an enhancement, not a dependency.

If FFmpeg is not installed at all, `Command::new("ffmpeg")` fails immediately. Same
outcome: graceful degradation to byte-range.

### 7.3 File uploaded that remux fails (codec not supported by MPEG-TS)

`-c:v copy` requires the codec to be valid inside MPEG-TS. Most H.264, H.265, VP9
files work. Some containers (AVI with DivX, MOV with ProRes) may fail remux. Outcome:
same as 7.2 — byte-range fallback, no error shown to user.

For a production system, the resolution is an ABR encode pass as a fallback (slower
but always produces valid HLS). See [ARCHITECTURE.md](./ARCHITECTURE.md).

### 7.4 Size limit enforcement

The 1GB limit is enforced as bytes arrive, not from the `Content-Length` header:

```rust
total_bytes += chunk.len();
if total_bytes > MAX_UPLOAD_BYTES {
    tokio::fs::remove_file(&file_path).await.ok();
    return Err(AppError::PayloadTooLarge);
}
```

This is important: `Content-Length` can be spoofed. A client that sends a forged
`Content-Length: 100` but streams 2GB will still be rejected when the counter hits 1GB.

### 7.5 Concurrent uploads

Each upload is an independent Axum handler running on a Tokio task. Disk writes happen
concurrently. SQLite WAL mode allows concurrent reads with one concurrent writer — if
two uploads complete at the same instant, one INSERT blocks briefly (~1ms) while the
other commits. This is not a correctness issue, only a minor throughput one.

Above ~100 concurrent uploads, SQLite write contention becomes measurable. Resolution:
PostgreSQL. See [ARCHITECTURE.md](./ARCHITECTURE.md).

### 7.6 HLS segment path traversal

The segment handler rejects any path containing `/` or `..`:

```rust
if segment.contains('/') || segment.contains("..") {
    return Err(AppError::BadRequest("invalid segment name".into()));
}
```

Without this, a request for `GET /api/hls/token/../../../etc/passwd` would construct a
valid file path outside the upload directory.

### 7.7 Token collision

Tokens are generated from `rand::thread_rng()` (OS-seeded CSPRNG). Collision probability
at 1M videos: ~3.5 × 10⁻⁷ per insert. SQLite's `UNIQUE` constraint on `token` catches
a collision and returns a 500. A retry loop would be the correct fix; the current
probability is low enough that it has not been implemented.

---

## 8. Security Model

**Privacy model:** Token-based obscurity. Possession of the 8-char token grants stream
access. No authentication layer — the spec explicitly excludes it.

| Vector | Mitigation | Notes |
|---|---|---|
| Path traversal (HLS segments) | Reject `/` and `..` in segment names | Implemented |
| Oversized uploads | 1GB counter in handler + nginx `client_max_body_size 1100M` | Implemented |
| Body stuffing on metadata routes | Axum default 2MB limit on all non-upload routes | Implemented |
| MIME type spoofing | Allowlist validation against `content_type` field | Implemented |
| Token enumeration | 2.8 trillion combinations | Sufficient for personal use; rate limiting needed for public |
| CORS | Currently `Any` (all origins) | Must restrict to your domain for public deployment |
| Malicious file content | None | Virus scanning (ClamAV) needed for public deployment |

For hardening steps and the path to signed time-limited URLs, see
[ARCHITECTURE.md §Security](./ARCHITECTURE.md#security).

---

## 9. Observability

### Current State

Structured logging via the `tracing` crate:

```
2026-04-05T23:05:52Z  INFO streamvault: Uploaded demo.mp4 (52428800 bytes) → token=a3f7bc12
2026-04-05T23:05:53Z  INFO streamvault::streaming: HLS transcode complete for token=a3f7bc12
```

HTTP request logging via `tower_http::TraceLayer`. Set `RUST_LOG=tower_http=debug` for
per-request logs including method, path, status, and duration.

### What's Missing and Why

| Missing | Impact | Notes |
|---|---|---|
| Prometheus `/metrics` endpoint | No dashboards, no alerting | Easy to add — see ARCHITECTURE.md |
| Distributed trace IDs | Can't correlate upload → transcode | Not needed at single-node scale |
| Disk usage monitoring | Can't alert before disk full | Out of scope for $0 deployment |

---

## 10. Known Limitations

| Limitation | Root cause | Impact | Resolution |
|---|---|---|---|
| Single video quality | No ABR encode pass | Slow connections may buffer | ABR pass post-remux — see ARCHITECTURE.md |
| `duration_secs` always null | FFprobe not called | Incomplete metadata | FFprobe after transcode |
| `/api/videos` no pagination | No cursor implemented | Slow at 10,000+ videos | `WHERE created_at < ? LIMIT 20` |
| SQLite write contention | Single writer | Degrades above ~100 concurrent uploads | Change `DATABASE_URL` to PostgreSQL |
| DB lost on `docker compose down -v` | SQLite inside container | Data loss on volume wipe | Mount host path: `./data/db:/app` |
| No upload resume | No TUS implementation | Large uploads restart on failure | TUS protocol |
| No video expiry | No `expires_at` column | Storage grows indefinitely | Cron + TTL column |
| HLS dirs not cleaned on transcode failure | No cleanup in error path | Orphan directories | `remove_dir_all` on error return |
| Token collision no retry | No retry loop | 500 error at extremely low probability | Retry with new token on UNIQUE violation |

---

*StreamVault · Rust · Axum · SQLite · Docker*
