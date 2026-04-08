# StreamVault — System Design

This document covers the current implementation: what was built, why, and what was
learned along the way. For the production scaling path, see [ARCHITECTURE.md](./ARCHITECTURE.md).

---

## The Problem Worth Solving First

The spec asks for time-to-stream to be prioritised over quality. That sounds simple
until you realise it creates a hard conflict: any processing step before playback
violates the primary requirement, but serving raw uploaded files produces inconsistent
seek behaviour on large videos.

The answer I landed on is a **dual-streaming protocol**. Upload completes, the raw
file is immediately streamable via HTTP Range requests. FFmpeg runs in the background
and remuxes to HLS — the player upgrades silently when it's done. The user never waits.

```
Upload completes
    │
    ├──→ HTTP byte-range stream       available: immediately
    │    raw file, Accept-Ranges: bytes, O(1) seek
    │
    └──→ tokio::spawn(ffmpeg -c:v copy)   available: ~250ms–13s later
         remux → HLS segments
         UPDATE videos SET hls_ready = TRUE
         player upgrades automatically
```

What makes this work in practice is `-c:v copy`. FFmpeg copies the bitstream verbatim
without decoding or re-encoding — it is entirely I/O-bound. On the real test videos
(see [benchmarks/RESULTS.md](../benchmarks/RESULTS.md)), a 101MB file is HLS-ready
in 623ms. A 644MB `.mov` file in 4.7s. The outliers are the long-form Blender movies
(ElephantsDream, Sintel) at 9-13 seconds — not because they're large, but because
they're 10-15 minute films with ~300-450 segment files to write. HLS time is driven
by duration, not file size.

---

## System Layout

```
                      ┌────────────────────────┐
                      │        Browser         │
                      └──────────┬─────────────┘
                                 │ HTTP :80
                      ┌──────────▼─────────────┐
                      │     nginx (alpine)      │
                      │  /api/*  → backend:3000 │
                      │  /health → backend:3000 │
                      │  /*      → index.html   │
                      │  proxy_buffering off    │
                      └──────────┬─────────────┘
                                 │
                      ┌──────────▼─────────────┐
                      │    Rust / Axum 0.7      │
                      │      :3000              │
                      └───────┬──────────┬──────┘
                              │          │
            ┌─────────────────▼──┐  ┌────▼───────────────────┐
            │  SQLite (WAL mode)  │  │  /data/uploads/         │
            │  /app/streamvault   │  │  {uuid}.mp4             │
            │  .db                │  │  hls/{token}/           │
            │  token index        │  │    playlist.m3u8        │
            └────────────────────┘  │    seg000.ts ...        │
                                    └──────────┬──────────────┘
                                               │
                                    ┌──────────▼──────────┐
                                    │  FFmpeg subprocess   │
                                    │  (tokio::spawn)      │
                                    │  -c:v copy           │
                                    │  -hls_time 2         │
                                    └─────────────────────-┘
```

**Backend** — Rust/Axum. Five source files:

- `main.rs` — router setup, AppState wiring, startup checks (creates upload dir and DB parent before anything else)
- `db.rs` — SQLitePool + four queries. No compile-time macros (explained below)
- `handlers/upload.rs` — streams multipart to disk, spawns background transcode
- `handlers/stream.rs` — HTTP Range serving, HLS playlist/segment serving, 307 fallback
- `streaming.rs` — single function that shells out to FFmpeg. Returns `Ok(())` on any failure — HLS is an enhancement, not a hard dependency

**Frontend** — SvelteKit with `adapter-static`. Builds to a static bundle served by nginx at runtime, no Node.js process. Two routes: home (upload + grid) and `/watch/:token` (player with protocol selection logic).

---

## Data Model

```sql
CREATE TABLE videos (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    token         TEXT NOT NULL UNIQUE,
    filename      TEXT NOT NULL,       -- UUID on disk, never user-controlled
    original_name TEXT NOT NULL,       -- display only, never used in file paths
    content_type  TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    duration_secs REAL,                -- NULL: not populated yet (needs FFprobe)
    width         INTEGER,             -- NULL: same reason
    height        INTEGER,             -- NULL: same reason
    hls_ready     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_videos_token ON videos(token);
```

`filename` and `original_name` are kept separate deliberately. `filename` is a
server-generated UUID and is the only value that ever touches the filesystem.
`original_name` is what the user called the file — it's shown in the UI and never
used to construct a path. If I used `original_name` for disk I/O, a file named
`../../etc/passwd` would be a path traversal.

The `duration_secs`, `width`, and `height` columns are nullable. They require
an FFprobe call, which adds latency to the upload response if done synchronously.
They're in the schema because they're the right fields to have, not because they're
populated — a follow-up FFprobe after HLS completes would fill them in without any
schema change.

**SQLite tuning:**
```sql
PRAGMA journal_mode=WAL;       -- concurrent reads don't block writes
PRAGMA synchronous=NORMAL;     -- crash-safe, without the full fsync cost
PRAGMA cache_size=-8000;       -- 8MB page cache vs 2MB default
```

WAL is critical here because during an upload write (INSERT), concurrent readers are
serving the video list and metadata. Without WAL, those reads block on the write lock.

**File layout on disk:**
```
/data/uploads/
├── 3f8a2c1d-4b5e-6789-abcd-ef0123456789.mp4
└── hls/
    └── a3f7bc12/
        ├── playlist.m3u8
        ├── seg000.ts
        └── seg001.ts
```

---

## Request Flows

### Upload

```
Browser  POST /api/upload  (multipart/form-data)
  │
  ▼
nginx  [proxy_request_buffering off]
  │   bytes stream through — nginx never buffers the body
  ▼
Axum  [DefaultBodyLimit::disable() on this route only]
  │
  │  validate MIME type against allowlist
  │  generate: UUID filename + 8-char token
  │  open file on disk
  │
  │  loop ~64KB chunks:
  │    total_bytes += chunk.len()
  │    if total_bytes > 1GB → delete partial file, return 413
  │    write chunk to disk
  │  flush + close
  │
  ├── INSERT INTO videos (token, filename, ...)
  │
  ├── HTTP 200 { token, share_url, stream_url }
  │          ↑ video is streamable right here
  │
  └── tokio::spawn(transcode_to_hls(...))
            runs concurrently, response already sent
            on success: UPDATE videos SET hls_ready = TRUE
            on failure: logs warning, hls_ready stays FALSE
```

The 1GB check counts actual bytes received, not `Content-Length`. A client that lies
about content length and streams more than 1GB gets cut off at 1GB regardless.

### Byte-Range Streaming

```
GET /api/stream/a3f7bc12
Range: bytes=37748736-38797311

  SELECT filename, content_type FROM videos WHERE token = ?
  File::open(file_path)
  file.seek(SeekFrom::Start(37748736))    ← O(1), no scan
  ReaderStream::new(file.take(1048576))

  → HTTP 206 Partial Content
    Content-Range: bytes 37748736-38797311/{file_size}
    Accept-Ranges: bytes
    Cache-Control: public, max-age=3600
```

Seek to any offset in a 644MB file costs the same as seek to byte 0 — the OS kernel
resolves the inode offset in constant time. Measured across 10 random seeks per file:
average 1.6–3.4ms, flat from 5MB to 644MB (see benchmarks).

### HLS Playlist (with Fallback)

```
GET /api/hls/a3f7bc12/playlist.m3u8

  hls_ready = FALSE →
    HTTP 307 Temporary Redirect
    Location: /api/stream/a3f7bc12
    Cache-Control: no-store       ← critical: must not be cached
    player follows redirect, plays byte-range

  hls_ready = TRUE →
    HTTP 200, Content-Type: application/vnd.apple.mpegurl
    Cache-Control: no-cache       ← playlist must always be fresh
```

The `Cache-Control: no-store` on the 307 is important. Without it, a browser can
cache the redirect and never re-check after HLS becomes ready — the player stays on
byte-range forever. I hit this during testing and fixed it.

### Watch Page Protocol Selection

On load, the player fetches video metadata and picks a stream URL:

```
meta.hls_ready ? /api/hls/{token}/playlist.m3u8 : /api/stream/{token}
```

While `hls_ready=false`, it polls the metadata endpoint every 3 seconds. When it
flips, the `streamUrl` reactive variable updates and the player switches to HLS without
a page reload. The status chip goes from amber ("Direct stream") to green ("HLS ready").

---

## Design Decisions

**Why serve raw file immediately and transcode async?**
Because any mandatory processing step before playback violates time-to-stream.
The alternative — transcode first, stream after — would mean waiting minutes for large
files. Real-time HLS during upload (piping to FFmpeg stdin) was also considered but
rejected: it couples upload and transcode into one pipeline, so a FFmpeg crash aborts
the upload. The decoupled approach means a transcode failure is an invisible background
degradation, not an upload failure.

**Why `-c:v copy` instead of re-encoding?**
Re-encoding at a reasonable quality (`libx264 medium`) takes roughly as long as the
video's duration. A 15-minute film takes 15+ minutes to encode. Remuxing with
`-c:v copy` copies the bitstream verbatim — it's I/O-bound, not CPU-bound. The cost
is single output quality (no adaptive bitrate). Given the spec explicitly prioritises
speed over quality, this is the right trade-off. ABR encoding belongs on a dedicated
worker fleet, not the upload hot path — see ARCHITECTURE.md.

**Why FFmpeg subprocess instead of Rust bindings (`ffmpeg-sys`)?**
Isolation. The Rust ffmpeg bindings link against C libraries. A codec assertion failure
or a bad input file can segfault the whole Axum process and take down the API for every
active request. A subprocess crash is contained — FFmpeg dies, the handler logs a
warning, the video falls back to byte-range. Also, `ffmpeg-sys` adds significant build
complexity and compile time, which matters in Docker multi-stage builds.

**Why `DefaultBodyLimit::disable()` only on the upload route?**
Axum applies a 2MB body limit globally by default. Without disabling it on the upload
route, any file larger than 2MB fails with a multipart parse error before the handler
sees a single byte. But disabling it globally removes protection from every other route.
The solution is a per-route layer:

```rust
.route("/api/upload", post(handlers::upload::upload_video)
    .layer(DefaultBodyLimit::disable()))
```

The 1GB limit is then enforced manually in the handler as bytes arrive.

**Why runtime SQLx queries instead of `query!` macros?**
`sqlx::query!()` and `sqlx::query_as!()` validate SQL at compile time by connecting
to a real database. That requires `DATABASE_URL` to be set during `cargo build`, which
always fails in Docker multi-stage build containers where the database doesn't exist at
build time. Runtime queries compile without a database. The downside is losing
compile-time SQL validation — typos become runtime panics rather than build errors.
Acceptable for four simple queries on a small schema.

**Why SQLite path as `sqlite:///app/streamvault.db?mode=rwc`?**
Two things worth noting here. Without `mode=rwc`, SQLite requires the file to already
exist — on a fresh container start it doesn't, and the connection fails. The `rwc` flag
creates it if absent. The absolute path matters too: relative paths resolve against the
process CWD, which can differ from `WORKDIR` depending on runtime. I put the database
at `/app/` specifically because `/data/` is a Docker volume mount — anything written
inside a volume mount during image build is gone at container start. `/app/` is the
workdir, never mounted, always writable.

**Why SvelteKit with `adapter-static`?**
Because at runtime there's no Node.js process — just nginx serving HTML/JS/CSS files.
This eliminates a runtime service, reduces the attack surface, and means the frontend
can be deployed to any CDN by copying the build output. SvelteKit's file-based routing
maps cleanly to the two routes needed: `/` (home) and `/watch/[token]` (player).

**Token format: 8 chars `[a-z0-9]`**
36^8 ≈ 2.8 trillion combinations. Long enough that brute-force enumeration is
impractical; short enough to share verbally. UUIDs were considered but they're longer
than needed and ugly in URLs. Sequential integers were rejected outright — any user
could walk the ID space and discover all videos.

---

## What the Benchmarks Showed

Measured against real videos in `Demo_Testing/`. Full results in [benchmarks/RESULTS.md](../benchmarks/RESULTS.md).

**Time-to-stream** came out at 1.1–7.3ms across all files from 5.3MB to 644MB. The
variation is OS scheduling noise — there's no size dependency at all. The architecture
works as intended here.

**HLS ready time** was the more interesting result. Small clips (5–100MB) were ready
in 250–720ms. The 644MB `.mov` file was ready in 4.7s. But the 162–182MB Blender
open movies took 9–13 seconds. That's counterintuitive until you look at what FFmpeg
is actually doing: writing 300–450 individual segment files for 10-15 minute films.
HLS time scales with video duration, not file size. This has a direct implication for
production sizing — transcode workers should be allocated based on expected video length.

**Seek latency** averaged 1.6–3.4ms across all files. The Sintel outlier (73.9ms max)
happened while its own FFmpeg transcode was still writing segments to the same Docker
volume — concurrent I/O contention. On a properly separated architecture (API reads
from CDN, transcode writes to object storage), this doesn't occur.

**20 concurrent viewers** on Sintel (182MB): p95 = 131.6ms, max = 133.2ms. Tight
distribution, no starvation. Tokio's async tasks handle concurrent reads well.

---

## Edge Cases Worth Noting

**Interrupted upload:** If the connection drops mid-upload, the multipart loop errors.
The partial file is deleted before the error returns. The DB INSERT only runs on
success, so no orphan token is created.

**FFmpeg not installed:** `transcode_to_hls()` returns `Ok(())` on any failure, including
the binary not existing. HLS is an enhancement. The system works fine without it.

**Codecs that can't remux to MPEG-TS:** Most H.264/H.265 files work with `-c:v copy`.
Some containers (AVI with DivX, older ProRes variants) fail remux. Same outcome as
above — byte-range fallback, no visible error.

**Size limit bypass via spoofed Content-Length:** Not possible. The counter increments
per actual chunk, not from headers.

**Path traversal on HLS segments:** The segment handler rejects any name containing `/`
or `..`. A request for `../../../etc/passwd` returns 400.

**Token collision:** Probability ~3.5 × 10⁻⁷ at 1M videos. SQLite's UNIQUE constraint
catches it and returns a 500. Low enough that a retry loop isn't implemented, but it's
the correct next step.

---

## Security Model

Privacy here is token-based obscurity — possession of the token grants stream access.
This is a deliberate choice given the spec excludes authentication. The token space
makes brute-force impractical for personal use, but for a public deployment, rate
limiting on the streaming endpoint would be necessary.

What is implemented:
- Path traversal protection on HLS segment names
- 1GB limit enforced on actual bytes (not Content-Length)
- Axum's default 2MB limit on all non-upload routes
- MIME type validation against an explicit allowlist
- `filename` stored separately from `original_name` — user input never touches file paths

What is not implemented and would be needed for public use:
- Rate limiting on upload and stream endpoints
- CORS restriction (currently `Any`)
- Signed time-limited URLs for proper access control
- Content scanning for malicious files

---

## Known Limitations

| Limitation | Impact | Next step |
|---|---|---|
| Single output quality (no ABR) | Slow connections may buffer on high-bitrate sources | ABR encode pass on worker fleet |
| `duration_secs` always null | Incomplete metadata | FFprobe call after transcode |
| No upload resume | Failed large uploads restart from zero | TUS protocol |
| SQLite single writer | Contention above ~100 concurrent uploads | Change `DATABASE_URL` to PostgreSQL |
| No video expiry | Storage grows indefinitely | `expires_at` column + cleanup job |
| `/api/videos` no pagination | Slow query at 10K+ videos | Cursor-based: `WHERE created_at < ? LIMIT 20` |
| DB wiped on `docker compose down -v` | Data loss on volume deletion | Mount `./data/db:/app` in compose |
| HLS dirs not cleaned on transcode failure | Orphan directories accumulate | `remove_dir_all` in error path |

---

*StreamVault · Rust · Axum · SQLite · FFmpeg · SvelteKit · Docker*
