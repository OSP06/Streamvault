# StreamVault — Complete Architecture & System Design

> Private Video Streaming Service · Rust · Axum · SQLite · HLS · Docker

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [System Architecture](#2-system-architecture)
3. [Core Design Decisions](#3-core-design-decisions--rationale)
4. [Full Technology Stack](#4-full-technology-stack)
5. [Alternative Approaches & Trade-offs](#5-alternative-approaches--trade-offs)
6. [Deployment Architectures](#6-deployment-architectures)
7. [Horizontal Scaling Strategies](#7-horizontal-scaling-strategies)
8. [Optimization with $1,000/Month Budget](#8-optimization-roadmap-with-1000month-budget)
9. [Platform Comparisons](#9-platform-comparisons)
10. [Security Considerations](#10-security-considerations)
11. [Known Limitations & Future Work](#11-known-limitations--future-work)
12. [Summary](#12-architecture-summary)

---

## 1. Executive Summary

StreamVault is a minimal, anonymous video streaming service designed around a single priority: getting a video from upload to playable as fast as possible. Every architectural decision flows from this principle.

The system is built in three layers: a Rust/Axum HTTP backend that handles uploads and streaming, a vanilla JavaScript single-page frontend served by nginx, and a SQLite metadata store. There is no authentication layer, no transcoding on the hot path, and no external service dependencies for the core flow.

**The key insight:** serving a raw video file with HTTP byte-range support is both the simplest and the fastest path to streaming. A browser's native `<video>` element handles buffering and seeking automatically once byte-range responses are in place. HLS transcoding runs in the background to improve adaptive playback quality — it is an enhancement, not a requirement.

| Property | Value |
|---|---|
| Purpose | Anonymous private video upload and streaming |
| Primary stack | Rust (Axum 0.7), Vanilla JS frontend, SQLite via SQLx |
| Max upload size | 1 GB per video |
| Supported formats | MP4, WebM, MOV, AVI, MKV, MPEG-TS, MPEG |
| Authentication | None — anonymous uploads, token-based sharing |
| Time-to-stream | < 1 second after upload completes (HTTP Range mode) |
| HLS ready time | Seconds (background remux, no re-encode) |
| Deployment | Docker Compose — single command startup |
| Free tier cost | $0/month (Fly.io + Tigris Object Storage) |
| $1,000/month budget | Full production stack with CDN, managed DB, auto-scaling |

---

## 2. System Architecture

### 2.1 High-Level Component Map

```
┌─────────────────────────────────────────────────────────┐
│                        Browser                          │
└───────────────────┬─────────────────────────────────────┘
                    │ HTTP / HTTPS
┌───────────────────▼─────────────────────────────────────┐
│                  nginx (alpine)                         │
│  • Serves static index.html (SPA)                       │
│  • Proxies /api/* → backend:3000                        │
│  • proxy_buffering off (critical for streaming)         │
│  • client_max_body_size 1100M                           │
└───────────────────┬─────────────────────────────────────┘
                    │ Internal Docker network
┌───────────────────▼─────────────────────────────────────┐
│              Rust / Axum 0.7 (backend)                  │
│  • POST /api/upload  — multipart stream to disk         │
│  • GET  /api/stream/:token — HTTP Range streaming       │
│  • GET  /api/hls/:token/* — HLS segment serving         │
│  • GET  /api/videos[/:token] — metadata API             │
│  • GET  /health — Docker healthcheck endpoint           │
└──────────┬─────────────────────┬───────────────────────-┘
           │                     │
┌──────────▼──────────┐ ┌────────▼────────────────────────┐
│  SQLite (via SQLx)  │ │  Docker Volume /data/uploads    │
│  • videos table     │ │  • raw uploaded video files     │
│  • token index      │ │  • hls/<token>/*.ts segments    │
│  • hls_ready flag   │ │  • hls/<token>/playlist.m3u8   │
└─────────────────────┘ └─────────────────────────────────┘
                    │
        ┌───────────▼───────────┐
        │  FFmpeg (subprocess)  │
        │  • -c:v copy remux    │
        │  • 2-second segments  │
        │  • runs in background │
        └───────────────────────┘
```

| Component | Technology & Role |
|---|---|
| API Server | Rust + Axum 0.7 — upload ingestion, byte-range streaming, HLS segment serving, metadata API |
| Frontend | Vanilla JS SPA — upload form with XHR progress, video player, shareable watch page |
| Reverse Proxy | nginx alpine — TLS termination, static file serving, API proxy with buffering disabled |
| Metadata Store | SQLite via SQLx — video tokens, filenames, content types, HLS readiness flag |
| File Storage | Local filesystem (Docker volume) — raw uploaded files + HLS segment directories |
| Transcoder | FFmpeg subprocess — background HLS remux (`-c:v copy`), runs after upload completes |
| Orchestration | Docker Compose — two services (backend + frontend), one named volume for video data |

---

### 2.2 Full Request Lifecycle

#### Upload Path

```
Browser
  │
  │  POST /api/upload (multipart/form-data)
  ▼
nginx
  │  proxy_request_buffering off → bytes flow through immediately
  ▼
Axum (DefaultBodyLimit::disable() on this route)
  │  Multipart stream → write chunks to disk (~64KB at a time)
  │  Track total_bytes → reject if > 1GB
  │
  ├─→ SQLite: INSERT video row (token, filename, content_type, size_bytes)
  │
  ├─→ Return JSON { token, share_url, stream_url } ← VIDEO IS PLAYABLE NOW
  │
  └─→ tokio::spawn → FFmpeg HLS transcode (background, non-blocking)
```

1. Browser sends multipart/form-data POST to `/api/upload`
2. nginx forwards bytes immediately — `proxy_request_buffering off` prevents nginx from buffering the full body
3. Axum streams chunks directly to disk — peak memory is ~64KB regardless of file size
4. Once write flushes, Axum generates a token, inserts metadata into SQLite, and returns the share URL. **The video is now streamable.**
5. `tokio::spawn` fires the FFmpeg HLS transcode — the upload response is already sent, user doesn't wait

#### Streaming Path (HTTP Range)

```
Browser <video src="/api/stream/:token">
  │
  │  GET /api/stream/:token
  │  Range: bytes=0-1048575
  ▼
Axum
  │  Look up filename in SQLite
  │  Open file, seek to byte offset
  │  Stream exactly requested bytes via ReaderStream
  │
  └→  206 Partial Content
      Content-Range: bytes 0-1048575/524288000
      Accept-Ranges: bytes
```

1. Browser opens `/watch/:token`, JS fetches `/api/videos/:token` for metadata
2. `<video>` element points at `/api/stream/:token`, browser sends `Range: bytes=0-` header
3. Axum seeks to the requested offset and streams exactly the requested bytes
4. When the user seeks to an unloaded position, browser sends a new Range request — Axum seeks and streams from there, no re-download

#### HLS Path (Post-Transcode)

```
FFmpeg (background)
  │  -i input.mp4 -c:v copy -c:a aac -hls_time 2
  │
  └→  /data/uploads/hls/:token/
        ├── playlist.m3u8
        ├── seg000.ts
        ├── seg001.ts
        └── seg002.ts ...

Axum: UPDATE videos SET hls_ready = TRUE WHERE token = ?

Browser: GET /api/hls/:token/playlist.m3u8
  └→  #EXTM3U
      #EXT-X-TARGETDURATION:2
      seg000.ts
      seg001.ts ...
```

---

### 2.3 Data Flow — All Endpoints

| Endpoint | Input | Output & Notes |
|---|---|---|
| `POST /api/upload` | multipart/form-data, `video` field | JSON: token, share_url, stream_url. File written to `/data/uploads/` |
| `GET /api/stream/:token` | `Range: bytes=N-M` header | 206 Partial Content, video bytes. Full seek support. |
| `GET /api/videos` | — | JSON array of all video metadata |
| `GET /api/videos/:token` | token path param | JSON: single video metadata + stream URLs |
| `GET /api/hls/:token/playlist.m3u8` | token | m3u8 playlist. 307 redirect to `/stream/` if HLS not ready yet. |
| `GET /api/hls/:token/:segment` | token + segment name | MPEG-TS segment bytes. `Cache-Control: public, max-age=86400` |
| `GET /health` | — | `{"status":"ok"}` — used by Docker healthcheck |

---

## 3. Core Design Decisions & Rationale

### 3.1 HTTP Range Streaming Before HLS

**Decision:** Serve the raw uploaded file immediately via HTTP Range requests. Treat HLS as an async enhancement, not a prerequisite.

**Why:** The primary requirement is time-to-stream. Transcoding — even a fast remux — takes time. HTTP Range requests are natively supported by every browser's `<video>` element since HTML5. The video is playable the instant the upload write flushes. Zero additional processing required.

**Trade-off:** Seeking to an unloaded position in a large file causes a new network request. With HLS, seeking is instant because segments are pre-chunked. This is acceptable for the demo tier because most private videos are watched linearly.

**Alternative considered:** Transcode first, stream after (YouTube's approach). Rejected because it adds mandatory latency before the video is watchable — the exact opposite of the primary requirement.

---

### 3.2 DefaultBodyLimit::disable() on Upload Route

**Decision:** Explicitly disable Axum's default body size limit on the `/api/upload` route only.

**Why:** Axum defaults to a **2MB body limit** for all routes. Any video larger than 2MB fails with a multipart parse error before the handler even runs. The limit is disabled at the route level so all other routes retain the 2MB default as a security measure. The handler enforces the 1GB limit itself by tracking `total_bytes` during streaming.

```rust
.route("/api/upload", post(handlers::upload::upload_video)
    .layer(DefaultBodyLimit::disable()))
```

---

### 3.3 FFmpeg `-c:v copy` (Remux, No Re-encode)

**Decision:** Use FFmpeg with `-c:v copy` for HLS transcoding — remux without decoding/encoding the video stream.

**Why:** Re-encoding is CPU-intensive and slow. A 1GB H.264 video re-encoded at 1080p takes 10-30 minutes on a shared-CPU instance. Remuxing the same file into HLS segments takes **seconds** — it's purely I/O bound. The video bitstream is copied as-is into MPEG-TS containers.

**Trade-off:** If the source uses an exotic codec (AV1, VP9 in MKV), some browsers may not decode the HLS segments. For the target use case (MP4/H.264 — the dominant upload format), this is not an issue. The raw byte-range stream remains available as fallback.

**FFmpeg command used:**
```bash
ffmpeg -i input.mp4 \
  -c:v copy \          # no re-encode — copies video bitstream as-is
  -c:a aac \           # re-encode audio to AAC (universal HLS compatibility)
  -hls_time 2 \        # 2-second segments
  -hls_list_size 0 \   # keep all segments in playlist (VOD mode)
  -hls_segment_type mpegts \
  -hls_segment_filename seg%03d.ts \
  -f hls playlist.m3u8
```

---

### 3.4 SQLite for Metadata

**Decision:** Use SQLite as the metadata store, accessed via SQLx with runtime (non-macro) queries.

**Why:** The metadata schema is trivial — one table, one index. SQLite in WAL mode handles concurrent reads without blocking writes. It requires zero external dependencies, making local development and free-tier deployment identical. The SQLx driver is database-agnostic — switching to PostgreSQL is a one-line change in `DATABASE_URL`.

**Why runtime queries (not `query_as!` macros):** SQLx compile-time macros require a live database connection during `cargo build`. This fails inside Docker build containers where no database is running. Runtime queries with manual row mapping eliminate this constraint entirely.

```rust
// ❌ Compile-time macro — requires DATABASE_URL at build time
let row = sqlx::query_as!(Video, "SELECT * FROM videos WHERE token = ?", token)

// ✅ Runtime query — works anywhere
let row = sqlx::query("SELECT * FROM videos WHERE token = ?")
    .bind(token)
    .fetch_optional(&pool)
    .await?;
```

---

### 3.5 Token-Based Sharing (No Auth)

**Decision:** Generate an 8-character alphanumeric token as the sole sharing credential. No accounts, sessions, or JWTs.

**Why:** The requirement explicitly excludes authentication. 36^8 ≈ **2.8 trillion combinations** makes brute force infeasible for a private service. The token appears in the URL path (`/watch/:token`) making it naturally shareable.

**Security note:** This is security-by-obscurity, not access control. For production use requiring true privacy, layer time-limited signed URLs (e.g. CloudFront signed URLs) on top.

---

### 3.6 Direct-to-Disk Upload Streaming

**Decision:** Stream multipart chunks directly to disk without buffering in application memory.

**Why:** A 1GB file cannot be held in RAM on small free-tier instances (256-512MB total RAM). Axum's Multipart API exposes an async stream interface. Writing chunks as they arrive keeps memory usage at **O(chunk_size) ≈ 64KB** regardless of file size.

```rust
while let Some(chunk) = field.chunk().await? {
    total_bytes += chunk.len();
    if total_bytes > MAX_UPLOAD_BYTES { /* reject */ }
    file.write_all(&chunk).await?;  // directly to disk
}
```

---

### 3.7 SQLite URL Format: `sqlite://path?mode=rwc`

**Decision:** Use `?mode=rwc` in the SQLite connection URL.

**Why:** Without `mode=rwc`, SQLite's default mode is read/write on an **existing file only**. If the file doesn't exist (first startup, fresh container), the connection fails with `unable to open database file`. The `rwc` flag means **r**ead/**w**rite/**c**reate — SQLite creates the file if it doesn't exist.

```
sqlite:///app/streamvault.db?mode=rwc
```

---

## 4. Full Technology Stack

### 4.1 Backend (Rust)

| Library | Version | Purpose |
|---|---|---|
| **Rust** | 1.85+ | Language — memory-safe, zero-cost abstractions, no GC pauses |
| **Axum** | 0.7 | Async HTTP framework built on hyper/tokio. Extractors, routing, middleware |
| **Tokio** | 1.x | Async runtime — multi-threaded work-stealing scheduler, `spawn()` for background tasks |
| **SQLx** | 0.7 | Async SQL toolkit — SQLite driver, runtime queries, connection pooling |
| **Tower-HTTP** | 0.5 | CORS middleware, request tracing via `TraceLayer` |
| **tokio-util** | 0.7 | `ReaderStream` — streams file bytes as HTTP response body |
| **UUID** | 1.x | Generating unique filenames for uploaded video files |
| **Serde / serde_json** | 1.x | JSON serialization for all API responses |
| **anyhow** | 1.x | Ergonomic error handling with context chaining |
| **futures** | 0.3 | `StreamExt` trait for async iteration over multipart chunks |
| **rand** | 0.8 | Generating 8-character alphanumeric share tokens |
| **tracing / tracing-subscriber** | 0.1 / 0.3 | Structured logging with env-filter for log level control |
| **FFmpeg** | System binary | HLS remux via subprocess — not a Rust dependency |

### 4.2 Frontend

| Technology | Purpose |
|---|---|
| **Vanilla JavaScript** | SPA routing via History API `pushState`, upload logic, video player |
| **XMLHttpRequest** | Upload with real progress events (`fetch()` API does not expose upload progress) |
| **HTML5 `<video>`** | Native video playback — byte-range seeking built in to all modern browsers |
| **CSS Custom Properties** | Design token system for consistent dark UI theming |
| **Google Fonts** | Space Grotesk (UI sans-serif) + IBM Plex Mono (code/metadata display) |
| **nginx alpine** | Serves `index.html`, proxies `/api/*` to backend, streaming-optimised config |

### 4.3 Infrastructure

| Tool | Purpose |
|---|---|
| **Docker** | Containerises both services — `rust:latest` builder + runtime image |
| **Docker Compose** | Orchestrates backend + frontend, manages named volume for video data |
| **Docker named volume** | Persists uploaded files and HLS segments across container restarts |
| **nginx** | `client_max_body_size 1100M`, `proxy_buffering off`, `proxy_request_buffering off` |

### 4.4 Key nginx Configuration Explained

```nginx
location /api/ {
    proxy_pass http://backend:3000;
    proxy_buffering off;              # stream response bytes to client immediately
    proxy_request_buffering off;      # stream request body to backend immediately
    client_max_body_size 1100M;       # allow up to 1.1GB uploads
    proxy_read_timeout 3600s;         # 1-hour timeout for large uploads
    proxy_send_timeout 3600s;
}
```

`proxy_buffering off` is critical — without it nginx would buffer the entire video file in memory before sending it to the browser, destroying the streaming behaviour.

---

## 5. Alternative Approaches & Trade-offs

### 5.1 Alternative Languages / Frameworks

| Runtime | Throughput | Memory | Dev Speed | Verdict |
|---|---|---|---|---|
| **Rust / Axum** *(current)* | Best — zero-copy I/O, no GC | ~5MB idle | Slowest — strict compiler | ✅ Correct choice for I/O-heavy streaming |
| Go / Fiber | Excellent — goroutines | ~20MB idle | Fast | Good alternative, simpler concurrency model |
| Node.js / Fastify | Good — event loop | ~60MB idle | Fastest | Acceptable but GC pauses visible under load |
| Python / FastAPI | Poor — GIL limits threads | ~80MB idle | Fast | Wrong tool for high-concurrency streaming |
| Elixir / Phoenix | Excellent — BEAM | ~30MB idle | Medium | Great concurrency model, niche skillset |

Rust is correct here because the bottleneck is I/O, not business logic. ~5MB idle RAM means it runs on the smallest free-tier instances.

---

### 5.2 Alternative Storage Backends

| Storage | Pros | Cons |
|---|---|---|
| **Local disk** *(current)* | Zero setup, no cost, lowest latency | Tied to one VM, no redundancy, blocks horizontal scaling |
| Cloudflare R2 | Zero egress cost, S3-compatible, global CDN | $0.015/GB storage, SDK integration needed |
| AWS S3 | Most mature, 11-nine durability, presigned URLs | $0.023/GB storage + $0.09/GB egress — expensive for video |
| Backblaze B2 | $0.006/GB storage, free egress via Cloudflare | Less ecosystem tooling than S3 |
| MinIO (self-hosted) | S3-compatible, fully self-hosted, free | Full ops burden, no managed redundancy |

**Recommendation for production:** Cloudflare R2. Zero egress cost fundamentally changes the economics of video hosting — 1TB served costs $0 in egress vs ~$92 on S3.

---

### 5.3 Alternative Streaming Protocols

| Protocol | Strengths | Weaknesses |
|---|---|---|
| **HTTP Range** *(current)* | Instant availability, zero processing, universal browser support | No adaptive bitrate, seek overhead on large files |
| **HLS** *(async, current)* | Adaptive bitrate possible, CDN-cacheable segments, native iOS/Safari | Requires FFmpeg, 2-10s delay before available |
| MPEG-DASH | Open standard, finer ABR control, HDR support | Poor Safari support, complex manifests |
| WebRTC | Ultra-low latency (<500ms), peer-to-peer capable | Not designed for VOD, signaling server required |
| Progressive Download | Simple — just serve the file | No seeking, no ABR, wastes bandwidth on partial views |

StreamVault uses **both** Range and HLS — Range for immediate availability, HLS for enhanced playback once transcoding completes.

---

### 5.4 Alternative Database Options

| Database | Best For | Avoid When |
|---|---|---|
| **SQLite** *(current)* | Single-node, simple schema, free tier, <1000 writes/sec | Multi-writer distributed setup |
| PostgreSQL | Multi-node, complex queries, full ACID | Overkill for simple metadata, $15+/mo managed |
| Turso (libSQL) | Edge SQLite with replication, low global latency | Large datasets, complex joins |
| DynamoDB | Hyper-scale writes, serverless billing | Complex queries, high cost at low scale |
| PlanetScale | MySQL-compatible, schema branching | Adds migration workflow complexity |

---

### 5.5 HLS Segment Duration Trade-offs

The current setting is `hls_time 2` (2-second segments). This is a deliberate balance:

| Segment Duration | Seek Latency | HTTP Requests | Buffer Depth |
|---|---|---|---|
| 1 second | Fastest | Very high | 3 requests = 3s |
| **2 seconds** *(current)* | Fast | Moderate | 3 requests = 6s |
| 4 seconds | Moderate | Low | 3 requests = 12s |
| 10 seconds | Slow | Very low | 3 requests = 30s |

2 seconds is the HLS default and the right choice for VOD content with interactive seeking.

---

## 6. Deployment Architectures

### 6.1 Current: Local Docker Compose

```
Host Machine
├── docker compose up --build
├── streamvault-frontend (nginx:alpine, port 80)
│   └── serves index.html + proxies /api/*
└── streamvault-backend (rust:latest)
    ├── /app/streamvault.db  (SQLite, in-container)
    └── /data/uploads/       (Docker volume: video_data)
```

| Property | Value |
|---|---|
| Startup | `docker compose up --build` |
| Stop + wipe | `docker compose down -v` |
| DB location | `/app/streamvault.db` inside backend container |
| Video storage | Docker named volume `video_data` → `/data/uploads` |
| Cost | $0 — runs on any machine with Docker |

---

### 6.2 Free Tier Cloud Deployment ($0/month)

```
Internet
    │
    ▼ HTTPS
Cloudflare (free)
    │ DNS + DDoS + SSL
    ├──────────────────────────────────────────────┐
    │ /                                            │ /api/*
    ▼                                              ▼
Cloudflare Pages (free)              Fly.io App (free tier)
index.html SPA                       Rust/Axum binary
                                          │
                                     ┌────▼────┐  ┌──────────┐
                                     │ SQLite  │  │  Tigris  │
                                     │ Fly vol │  │  5GB free│
                                     └─────────┘  └──────────┘
```

| Layer | Service | Free Allowance |
|---|---|---|
| Compute (API) | Fly.io | 3x shared-CPU-1x 256MB VMs, always-on |
| Object Storage | Tigris (Fly.io native) | 5GB storage, S3-compatible |
| Database | SQLite on Fly volume | 1GB persistent volume |
| Frontend CDN | Cloudflare Pages | Unlimited bandwidth, global edge |
| TLS & DNS | Cloudflare | Free SSL, DDoS protection, analytics |
| Total | | **$0/month** |

> ⚠️ **Limitation at this tier:** Axum proxies video bytes through the Fly.io instance. Fly charges ~$0.02/GB egress beyond the free allowance. For frequently-watched videos, implement presigned S3 URLs to redirect the browser directly to Tigris.

---

### 6.3 Production Deployment ($20-50/month)

Targets: 100-500 concurrent viewers, up to 1TB storage, global audience.

```
Internet
    │
    ▼ HTTPS
Cloudflare (free plan)
    │
    ├── Static SPA → Cloudflare Pages (free)
    │
    └── /api/* → Fly.io Load Balancer
                      │
              ┌───────┴────────┐
              ▼                ▼
         Axum Node 1      Axum Node 2
              │                │
              └───────┬────────┘
                      │
         ┌────────────┴────────────┐
         ▼                         ▼
    Turso (libSQL)           Cloudflare R2
    Edge database            Zero-egress storage
    (replicated globally)    + CDN delivery
```

| Layer | Service | Monthly Cost |
|---|---|---|
| Compute | Fly.io 2x shared-4-CPU 2GB RAM | ~$14 |
| Object Storage | Cloudflare R2 (500GB) | ~$7.50 |
| Database | Turso free tier | $0 |
| CDN | Cloudflare free plan | $0 |
| Total | | **~$22-50/month** |

---

### 6.4 $1,000/Month Production Stack

Targets: 1,000-10,000 concurrent viewers, multi-region, full observability, dedicated transcode workers.

```
Internet
    │
Cloudflare Pro ($20/mo) — WAF, DDoS, Bot Protection
    │
    ├── Cloudflare Pages — SPA frontend (free)
    │
    └── Cloudflare Load Balancing
              │
    ┌─────────┼─────────┐
    ▼         ▼         ▼
 API (iad) API (lhr) API (nrt)     ← 3 regions on Fly.io
    │         │         │
    └────┬────┴────┬────┘
         │         │
    Neon Postgres  Cloudflare R2
    ($69/mo)       ($150/mo, 10TB)
    Serverless,    Zero egress,
    autoscaling    global CDN

    Separate transcode workers (4x) ← pull from Redis job queue
    Upstash Redis ($30/mo) ← job queue + retry logic

    Datadog / Grafana ($100/mo) ← metrics, dashboards
    Sentry ($26/mo) ← error tracking
```

| Item | Monthly Cost |
|---|---|
| Fly.io — 4x API nodes (3 regions) | ~$200 |
| Fly.io — 4x transcode workers | ~$120 |
| Cloudflare R2 (10TB storage) | ~$150 |
| Neon PostgreSQL (serverless) | ~$69 |
| Upstash Redis (job queue) | ~$30 |
| Cloudflare Pro (WAF + DDoS) | ~$20 |
| Sentry Team (error tracking) | ~$26 |
| Grafana Cloud (metrics) | ~$100 |
| GitHub Actions CI/CD | ~$16 |
| Misc (staging, backups, DNS) | ~$50 |
| **Total** | **~$781/month** |

$219/month headroom remains for spikes, new regions, or additional storage.

---

## 7. Horizontal Scaling Strategies

### 7.1 Making the API Stateless

The Axum service is already stateless by design. All persistent state lives in the database and object storage. To run multiple nodes:

- **Replace SQLite → PostgreSQL or Turso** — both support concurrent connections from multiple nodes
- **Replace local disk → S3/R2** — all nodes read/write from the same object store
- **Add a load balancer** — Fly.io built-in proxy, Cloudflare Load Balancing, or AWS ALB

No session state, no shared memory, no file locks between nodes. The only coordination is through the database.

---

### 7.2 Direct-to-S3 Upload (Bypass the API for Uploads)

At high upload volume, every byte flows through the API node. The solution is **presigned PUT URLs**:

```
1. Browser → POST /api/upload/init
              Axum generates presigned S3 PUT URL + pending token
              Returns { uploadUrl, token }

2. Browser → PUT {uploadUrl}  (direct to S3/R2, bypassing Axum entirely)
              Upload throughput = S3 throughput (effectively unlimited)

3. Browser → POST /api/upload/complete?token=...
              Axum marks video as active, spawns HLS transcode
              Returns { share_url, stream_url }
```

Result: unlimited upload throughput, zero API node bandwidth consumed for uploads.

---

### 7.3 Presigned Download URLs (Bypass the API for Streaming)

Currently every video byte flows through Axum during playback. At 500 concurrent viewers at 5Mbps, that's 2.5Gbps through the API nodes.

```
Before (current):
Browser → Axum → reads file → streams bytes to browser

After (presigned URLs):
Browser → GET /api/videos/:token
          Axum generates presigned R2 GET URL (15-min TTL)
          Returns { ..., presigned_url }
Browser → <video src="{presigned_url}">
          Video bytes flow: R2 → Cloudflare CDN → Browser
          Axum handles zero video bytes at playback time
```

---

### 7.4 Transcode Worker Pool

At high upload volume, background `tokio::spawn` tasks saturate the API node's CPU. The scale-out path:

```
Current:
  Upload → tokio::spawn(ffmpeg) on API node

Scaled:
  Upload → push job to Redis queue
  
  Worker nodes (separate fleet):
    loop {
      job = redis.pop("transcode_queue")
      run_ffmpeg(job.input_path)
      upload_segments_to_r2(job.token)
      db.mark_hls_ready(job.token)
    }
```

Workers scale independently from API nodes — add workers when transcode queue depth grows.

---

### 7.5 CDN Caching for HLS Segments

HLS segments are immutable once written — perfect CDN cache targets:

- `Cache-Control: public, max-age=86400` on segment responses (already set in code)
- `Cache-Control: no-cache` on `.m3u8` playlist (must not be cached — already set)
- With Cloudflare R2 + CDN, segments are served from the nearest edge — zero origin load after first request

---

### 7.6 Scaling Decision Tree

```
Current load → Action needed

< 50 viewers     → Docker Compose on local machine ($0)
50-500 viewers   → Fly.io + R2 + Turso ($20-50/mo)
500-2000 viewers → Presigned URLs + 2 API nodes + Postgres ($80-150/mo)
2000-10000       → Worker pool + job queue + 4+ nodes + multi-region ($400-600/mo)
10000+           → Kubernetes + global CDN + dedicated transcode fleet ($1000+/mo)
```

---

## 8. Optimization Roadmap with $1,000/Month Budget

### Priority 1 — Object Storage + CDN (~$150/mo, highest ROI)

Migrate from local disk to Cloudflare R2 with presigned URLs.

**Impact:** Horizontal scaling unblocked, zero egress cost, global low-latency delivery, 11-nine durability.

**Code changes needed:**
```rust
// Add to Cargo.toml
aws-sdk-s3 = "1"  // R2 is S3-compatible

// Upload: write to R2 instead of local disk
let upload = s3_client.put_object()
    .bucket(&bucket)
    .key(&key)
    .body(ByteStream::from(file_bytes))
    .send().await?;

// Stream: generate presigned URL instead of proxying bytes
let presigned = s3_client.get_object()
    .bucket(&bucket)
    .key(&key)
    .presigned(PresigningConfig::expires_in(Duration::from_secs(900))?)
    .await?;
```

---

### Priority 2 — Adaptive Bitrate HLS ($0 extra, software change)

Add multi-rendition HLS output to the transcode pipeline:

```bash
# Current: single quality
ffmpeg -i input.mp4 -c:v copy -hls_time 2 -f hls playlist.m3u8

# With ABR: three quality levels
ffmpeg -i input.mp4 \
  -map 0:v -map 0:a -c:v copy -c:a aac \
    -hls_time 2 -hls_segment_filename "1080p_%03d.ts" -f hls 1080p.m3u8 \
  -map 0:v -map 0:a -vf scale=-2:720 -b:v 2500k -c:a aac \
    -hls_time 2 -hls_segment_filename "720p_%03d.ts" -f hls 720p.m3u8 \
  -map 0:v -map 0:a -vf scale=-2:480 -b:v 1000k -c:a aac \
    -hls_time 2 -hls_segment_filename "480p_%03d.ts" -f hls 480p.m3u8

# Master playlist (written by code)
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080
1080p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
720p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480
480p.m3u8
```

Players automatically switch renditions based on network conditions.

---

### Priority 3 — PostgreSQL ($69/mo)

```bash
# One environment variable change
DATABASE_URL=postgres://user:pass@neon.tech/streamvault
```

SQLx's query API is identical for SQLite and PostgreSQL — no code changes beyond the URL. Benefits: multi-node writes, connection pooling, point-in-time recovery, full-text search.

---

### Priority 4 — Transcode Worker Separation ($150/mo)

Extract `tokio::spawn(ffmpeg)` into a separate worker service with a Redis job queue. API nodes drop to minimal CPU — they only handle metadata and URL generation.

---

### Priority 5 — Observability ($126/mo)

- **Sentry** ($26/mo) — full stack traces, performance monitoring, release tracking
- **Grafana Cloud** ($100/mo) — upload success rate, stream latency p50/p95/p99, transcode queue depth, storage usage trends

---

### Priority 6 — Video Thumbnails ($0, software change)

```bash
# Extract thumbnail at 5 seconds into the video
ffmpeg -i input.mp4 -ss 5 -vframes 1 -f image2 thumb.jpg
```

Store alongside HLS segments in R2. Serve at `/api/thumb/:token`. Replaces the current play-button placeholder in the video grid with actual video previews.

---

### Priority 7 — Upload Resume (TUS Protocol, $0)

For large files on unreliable connections, implement the [TUS resumable upload protocol](https://tus.io/). If an upload fails at 900MB, the user resumes from 900MB — not from zero. The `tus-axum` crate provides a drop-in implementation.

---

### Budget Summary

| Priority | Change | Cost | Impact |
|---|---|---|---|
| 1 | R2 + presigned URLs | $150/mo | Horizontal scaling, zero egress, CDN |
| 2 | ABR HLS | $0 | Consistent playback on slow connections |
| 3 | PostgreSQL (Neon) | $69/mo | Multi-node writes, durability |
| 4 | Transcode workers | $150/mo | Independent scaling, queue depth control |
| 5 | Sentry + Grafana | $126/mo | Full observability, error tracking |
| 6 | Thumbnails | $0 | Better video grid UX |
| 7 | TUS resume | $0 | Better UX for large uploads |
| Infrastructure | 4 API nodes + misc | ~$286/mo | Multi-region, reliability |
| **Total** | | **~$781/mo** | |

---

## 9. Platform Comparisons

### 9.1 StreamVault vs YouTube

| Dimension | YouTube | StreamVault |
|---|---|---|
| Upload processing | Mandatory transcoding to 10+ resolutions before playable (minutes-hours) | Playable immediately via byte-range, HLS in seconds |
| Privacy | All uploads indexed even if "unlisted" — discoverable | No index, token-only access, no platform data collection |
| Authentication | Google account required to upload | Fully anonymous |
| File size limit | 256GB / 12 hours | 1GB (configurable in code) |
| Codec output | VP9 / AV1 (re-encoded) | Original codec preserved |
| Cost to operator | $0 (ad-supported) | $0-$1,000/mo depending on scale |
| Use case | Public content discovery | Private one-click sharing |

---

### 9.2 StreamVault vs Vimeo

| Dimension | Vimeo | StreamVault |
|---|---|---|
| Pricing | $7-75/month for meaningful storage | $0 free tier |
| Transcoding | Cloud transcoding with review workflow | Immediate byte-range + background HLS |
| Privacy controls | Domain restriction, password protection, review pages | Token-only (simpler but functional) |
| Player | Polished branded player with chapters, CTAs | Native browser `<video>` element |
| API | Full REST API with OAuth | Minimal REST API, no OAuth |
| Self-hostable | No | Yes — full source, Docker deployment |

---

### 9.3 StreamVault vs AWS MediaConvert + S3 + CloudFront

| Dimension | AWS Stack | StreamVault |
|---|---|---|
| Setup complexity | IAM roles, S3 buckets, CloudFront distributions, MediaConvert jobs, Lambda triggers — days | `docker compose up` — minutes |
| Cost at low scale | ~$5-30/month even for light use (MediaConvert + S3 + egress) | $0 |
| Cost at scale (1TB/mo egress) | ~$92 egress + MediaConvert + S3 | ~$9 (R2, zero egress) |
| Transcoding quality | Broadcast-quality, multi-codec, HDR, watermarking, captions | `-c:v copy` remux only |
| Reliability | 99.99% SLA, multi-AZ, automatic failover | Single-region by default |
| Vendor lock-in | High — proprietary APIs throughout | Low — portable Docker containers, S3-compatible storage |

---

### 9.4 StreamVault vs Cloudflare Stream

| Dimension | Cloudflare Stream | StreamVault on R2 |
|---|---|---|
| Pricing model | $5/1,000 min stored + $1/1,000 min delivered | Flat $0.015/GB storage, zero egress |
| 1,000 hours stored | $300/month | ~$9/month (600GB × $0.015) |
| Setup | REST API, automatic transcoding and HLS generation | Self-hosted, full control |
| Player | Managed player with token protection, signed URLs built-in | Native `<video>`, manual token system |
| Transcoding | Automatic multi-bitrate in minutes | `-c:v copy` in seconds, single bitrate |
| Best for | Teams wanting managed infrastructure with SLA | Engineers wanting control and lower cost |

---

### 9.5 Cost Comparison at Scale

| Service | 100GB storage + 1TB/mo bandwidth |
|---|---|
| YouTube | $0 (but public, ad-supported) |
| Vimeo Pro | ~$50/mo + storage limits |
| AWS S3 + CloudFront | ~$100/mo (egress-dominated) |
| Cloudflare Stream | ~$180/mo (per-minute pricing) |
| Vimeo Enterprise | ~$900/mo |
| **StreamVault on R2** | **~$17/mo** ($1.50 storage + $0 egress + $15 compute) |

---

## 10. Security Considerations

### 10.1 Current Protections

| Protection | Implementation |
|---|---|
| Path traversal prevention | HLS segment names checked for `/` and `..` — rejected with 400 |
| Content-type validation | Whitelist of MIME types. Extension-based inference if browser sends `application/octet-stream` |
| Upload size enforcement | Dual: nginx `client_max_body_size 1100M` + Axum handler tracks bytes, rejects at 1GB |
| Body limit on other routes | `DefaultBodyLimit::disable()` applied only to `/api/upload` — all other routes keep 2MB default |
| Token entropy | 36^8 ≈ 2.8 trillion combinations — brute force at 1,000 req/sec takes ~88 years |
| CORS | Currently permissive (`Any`) — restrict to your domain in production |

### 10.2 Production Hardening Recommendations

- **Rate limiting** — `tower-governor` crate: 10 uploads/hour per IP to prevent abuse
- **Signed URLs with expiry** — Time-limited presigned URLs instead of persistent tokens for true access control
- **Virus scanning** — ClamAV on uploaded files before they are served (videos can contain embedded malicious content)
- **Content Security Policy** — CSP headers in nginx to prevent XSS via the frontend
- **Expiry TTL** — Scheduled job to delete videos older than N days from storage and database
- **HTTPS enforcement** — nginx redirects HTTP → HTTPS with HSTS headers in production

---

## 11. Known Limitations & Future Work

| Limitation | Impact | Resolution |
|---|---|---|
| Single bitrate HLS | Users on slow connections see buffering | Add 720p + 480p re-encode passes to transcode pipeline |
| No video thumbnails | Video grid shows placeholder icons | `ffmpeg -vframes 1` thumbnail extraction in transcode pipeline |
| SQLite single-writer | >100 concurrent uploads may see contention | Migrate to PostgreSQL (one env var change) |
| No upload resume | Failed large uploads must restart | Implement TUS protocol or S3 multipart upload |
| No expiry/cleanup | Storage grows unboundedly | Scheduled cleanup job for videos older than N days |
| No video duration | Duration is NULL in database | Parse from FFmpeg output during transcode |
| DB lost on `down -v` | `docker compose down -v` wipes the database | Mount a host directory for `/app/streamvault.db` |
| No search | Videos only findable by token | Add full-text search on `original_name` in PostgreSQL |
| FFmpeg unavailable | HLS silently skips, byte-range only | This is intentional graceful degradation |
| No access revocation | Once shared, a token cannot be invalidated | Add a `revoked` boolean column and check it on each request |

---

## 12. Architecture Summary

StreamVault achieves its primary goal — minimal time-to-stream — through a deliberate separation of concerns:

- **Upload path** writes to disk and returns immediately
- **Streaming path** reads and seeks with zero transcoding dependency
- **HLS path** improves playback quality asynchronously without blocking either

The architecture has clean upgrade seams at every layer:

```
SQLite          → PostgreSQL      (one environment variable)
Local disk      → S3 / R2         (add object storage client)
Single node     → Multi-node      (remove local disk dependency)
tokio::spawn    → Worker pool     (add message queue)
Byte-range only → ABR HLS         (add FFmpeg re-encode pass)
```

At **$0/month** on Fly.io free tier, it handles personal and team use comfortably.  
At **$1,000/month** with R2, Neon, dedicated workers, and full observability, it handles tens of thousands of concurrent viewers.

The code doesn't change between those two tiers — only the infrastructure it runs on.

> **Core Principle:** *"Make it work on the smallest instance first, with clean seams where paid services plug in."*  
> Every component in StreamVault can be independently upgraded without touching the others.

---

*StreamVault — Built with Rust + Axum + SQLite + Docker*
