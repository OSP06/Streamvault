# StreamVault — Architecture & Scaling

> This document answers: **how would you run this in production?**
> It covers deployment tiers, horizontal scaling, cost modelling, technology alternatives,
> and the path from a single Docker container to a multi-region streaming platform.
>
> For the current implementation — how it works, what was measured, design decisions —
> see [DESIGN.md](./DESIGN.md).

---

## Contents

1. [Executive Summary](#1-executive-summary)
2. [What Blocks Horizontal Scaling Today](#2-what-blocks-horizontal-scaling-today)
3. [Deployment Tiers](#3-deployment-tiers)
4. [Scaling the Upload Path](#4-scaling-the-upload-path)
5. [Scaling the Transcode Path](#5-scaling-the-transcode-path)
6. [Scaling the Streaming Path](#6-scaling-the-streaming-path)
7. [Technology Alternatives](#7-technology-alternatives)
8. [The Production Stack ($781/mo)](#8-the-production-stack-781mo)
9. [Security Hardening](#9-security-hardening)
10. [Observability at Scale](#10-observability-at-scale)
11. [Real-World Streaming Platform Concerns](#11-real-world-streaming-platform-concerns)

---

## 1. Executive Summary

StreamVault's current architecture runs on two Docker containers. The design was
deliberately made to scale out without code rewrites:

- **Storage** is the only horizontal scaling blocker — replace local disk with S3/R2 (one function swap per handler)
- **Database** can be swapped from SQLite to PostgreSQL by changing `DATABASE_URL` (SQLx abstracts the driver; query syntax is identical)
- **Transcode** can be moved from in-process `tokio::spawn` to a Redis job queue + worker fleet by changing one call site

The Axum router, all handler logic, the token system, HTTP Range implementation, and
nginx config are **unchanged** across all deployment tiers. The application does not
know what infrastructure tier it runs on.

```
Tier 0 (current):  1 container  ·  SQLite  ·  local disk   ·  $0/mo
Tier 1:            Fly.io       ·  SQLite  ·  Tigris S3     ·  $0/mo
Tier 2:            Fly.io       ·  SQLite  ·  R2 + CDN      ·  $20-50/mo
Tier 3:            3+ nodes     ·  Postgres·  R2 + CDN      ·  $100-300/mo
Production:        Multi-region ·  Postgres·  R2 + Cloudflare·  ~$781/mo
```

---

## 2. What Blocks Horizontal Scaling Today

Only two things prevent running multiple API nodes:

### Blocker 1: Local Disk Storage

Uploaded videos live on a Docker named volume attached to one container. A second API
node cannot read files written by the first.

**Fix:** Replace `File::create` / `File::open` with an S3/R2 client. The token system,
database schema, and all handlers are unchanged. The change is confined to two files:
`handlers/upload.rs` (write) and `handlers/stream.rs` (read).

### Blocker 2: SQLite Single Writer

WAL mode allows concurrent reads, but only one writer at a time. Above ~100 concurrent
uploads, write contention becomes measurable. SQLite also cannot be accessed by multiple
processes on different machines.

**Fix:** Change `DATABASE_URL` from `sqlite:///app/...` to `postgres://...`. SQLx
abstracts the driver — all four queries in `db.rs` use standard SQL that runs unchanged
on PostgreSQL. No query rewrite needed.

---

## 3. Deployment Tiers

### Tier 0 — Local / Demo ($0/mo)

```bash
docker compose up --build
```

SQLite. Local disk volume. Single container. Good for development and demos.
Not suitable for production — no persistence guarantees, no redundancy.

---

### Tier 1 — Free Cloud ($0/mo)

| Component | Service | Details |
|---|---|---|
| API | Fly.io | 3 shared VMs, always-on, free |
| Storage | Tigris (Fly.io native) | S3-compatible, 5GB free |
| Frontend SPA | Cloudflare Pages | Unlimited bandwidth, free |
| DNS + SSL | Cloudflare | Free |

**Code changes from Tier 0:**
1. Add `aws-sdk-s3` (or `object_store`) crate
2. Replace `File::create` in upload handler with `S3::put_object`
3. Replace `File::open` in stream handler with `S3::get_object` (streaming response)
4. SQLite remains — Fly volume for persistence

**Limitation at this tier:** Axum proxies all video bytes (upload and playback). Fly.io
charges ~$0.02/GB egress beyond free allowance. Acceptable for low traffic.

---

### Tier 2 — Small Production ($20-50/mo)

**Key architectural change: presigned URLs for delivery.**

```
Tier 1 (Axum in the data path):
  Browser ──→ Axum ──→ S3 ──→ Axum ──→ Browser

Tier 2 (Axum out of the data path):
  Browser ──→ GET /api/videos/{token}
  Axum generates presigned_url (15-min TTL)
  Browser ──→ <video src=presigned_url> ──→ R2 ──→ CDN ──→ Browser
```

Axum now handles zero video bytes at playback time. All streaming bandwidth is
served by Cloudflare R2 at zero egress cost.

**Cost estimate:**
- Fly.io compute: ~$14/mo (1 shared-cpu-4x, 2GB)
- Cloudflare R2: ~$7.50/mo (500GB storage, zero egress)
- Total: ~$22/mo

---

### Tier 3 — Production-Ready ($100-300/mo)

Changes from Tier 2:

- **SQLite → PostgreSQL** (Neon serverless, ~$69/mo) — multi-writer, replication, managed backups
- **`tokio::spawn` transcode → Redis job queue + worker** — decouple API from CPU-heavy transcode
- **3+ API nodes** behind Fly.io load balancer — horizontal API scaling
- **Cloudflare Pro** — WAF, edge rate limiting, DDoS protection

---

## 4. Scaling the Upload Path

### Current: Axum Proxies the Upload

```
Browser ──(1GB body)──→ nginx ──→ Axum ──→ disk
```

One API node receives the full file. Streaming write (`proxy_request_buffering off`,
`64KB chunks`) keeps RAM low, but network bandwidth and file descriptors are consumed
by the API process.

### Scaled: Presigned S3 PUT

Axum exits the upload data path entirely:

```
Step 1: Browser ──→ POST /api/upload/init
          Axum: generate token, insert pending row in DB
          Return: { presigned_put_url (15-min TTL), token }

Step 2: Browser ──→ PUT {presigned_put_url}  (direct to R2)
          Axum handles zero upload bytes
          R2 enforces size limit via Content-Length header
          Upload throughput: limited by R2, not API node count

Step 3: Browser ──→ POST /api/upload/complete?token={token}
          Axum: mark row active, enqueue transcode job
```

**Benefits:**
- API nodes are not bandwidth-bottlenecked during uploads
- Upload throughput scales with R2, not with API node count
- Multipart uploads (R2 supports up to 5GB parts) become trivial

---

## 5. Scaling the Transcode Path

### Current: In-Process tokio::spawn

```
upload_handler
  └──→ tokio::spawn(ffmpeg subprocess)
         Runs on API process
         CPU spike during transcode
```

**Problem at scale:** FFmpeg with `-c:v copy` is I/O-light, but an ABR encode pass
(720p + 480p) is CPU-heavy. 50 simultaneous encodes on an API node thrash the CPU,
degrading response latency for all other requests.

**Second problem:** No visibility. If 500 videos are queued for transcode, there is no
way to know. No progress, no retry, no dead-letter queue.

### Scaled: Redis Job Queue + Worker Fleet

```
API node (I/O-light):
  upload_handler ──→ redis.rpush("transcode_queue", {
      token,
      input_key: "uploads/{uuid}.mp4",   ← S3 key, not local path
      output_prefix: "hls/{token}/"
  })
  Return HTTP 200 immediately

Worker fleet (CPU-heavy):
  loop {
      job = redis.blpop("transcode_queue", timeout=5)
      s3.download(job.input_key, /tmp/{uuid}.mp4)
      ffmpeg -i /tmp/{uuid}.mp4 \
        -c:v libx264 -crf 23 -preset fast ...  ← now re-encoding, not remux
        -c:v copy ...                           ← or still remux for speed
      s3.upload_dir(/tmp/hls/{token}/, job.output_prefix)
      db.mark_hls_ready(job.token)
  }
```

**Benefits:**
- Workers are right-sized for CPU (more cores, no memory needed)
- API nodes are right-sized for I/O (low CPU, fast network)
- Job visibility: queue depth, worker utilisation, failure rate all observable
- Retry: failed jobs re-enqueue automatically (Redis LPUSH on failure)
- Dead-letter queue: jobs that fail N times go to `transcode_failed` for inspection

### ABR Encoding at Scale

With a worker fleet, ABR (Adaptive Bitrate) encoding becomes viable:

```
Input: 1280x720 H.264 @ 4Mbps

Output renditions:
  720p:  -vf scale=1280:720  -b:v 2500k   ← high quality
  480p:  -vf scale=854:480   -b:v 1000k   ← medium quality
  360p:  -vf scale=640:360   -b:v 500k    ← low / mobile

Master playlist (HLS):
  #EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
  720p/playlist.m3u8
  #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=854x480
  480p/playlist.m3u8
```

The player automatically selects the rendition that fits the available bandwidth.
Buffering events drop significantly on slow connections.

---

## 6. Scaling the Streaming Path

### Current: Axum Serves Video Bytes

Every byte-range request hits the Axum process. The server is in the hot path for
all video delivery bandwidth.

### Scaled: CDN-First Delivery

With presigned URLs (Tier 2+), Axum exits the streaming data path:

```
GET /api/videos/{token}
  Axum: look up token, generate presigned_url (TTL: 15min)
  Return: { stream_url: "https://r2-cdn.example.com/{key}?sig=...&expires=..." }

Browser → <video src={stream_url}>
  Player → Cloudflare R2 (or CDN edge)
  Axum handles zero streaming bytes
```

**HLS segments at CDN:**
- Segments are immutable: `Cache-Control: public, max-age=86400, immutable`
- First request per segment hits R2 origin
- All subsequent requests served from CDN edge (sub-millisecond)
- R2 egress cost: $0 (Cloudflare-to-Cloudflare is free)

---

## 7. Technology Alternatives

### Runtime

| | Rust / Axum | Go / Fiber | Node.js / Fastify | Python / FastAPI |
|---|---|---|---|---|
| Memory idle | ~5 MB | ~15 MB | ~50 MB | ~70 MB |
| GC pauses during streaming | None | Occasional | Occasional | Occasional |
| Throughput | Highest | High | Good | Adequate |
| Build time | 3-5 min (cold) | 30 sec | 10 sec | 5 sec |
| Ecosystem for video tooling | Limited | Good | Excellent | Good |

Rust's advantage at streaming scale: no GC pauses means no latency spikes for the
50th viewer. At 500 concurrent viewers, Rust uses ~50MB RSS. Equivalent Node.js: ~300MB.

### Storage

| | Local disk (current) | Cloudflare R2 | AWS S3 |
|---|---|---|---|
| Setup | Zero | SDK + credentials | SDK + IAM |
| Storage cost | $0 | $0.015/GB | $0.023/GB |
| Egress cost | $0 (local) | **$0** | $0.09/GB |
| Horizontal scaling | Blocked | Unlimited | Unlimited |
| Durability | Single disk | 11 nines | 11 nines |

R2 is chosen over S3 at streaming scale because **egress is free**. A platform serving
10TB/month in video saves ~$900/mo in egress alone compared to S3.

### Database

| | SQLite (current) | PostgreSQL | Turso |
|---|---|---|---|
| Setup | Zero | Managed instance or self-host | Sign up + connection string |
| Cost | $0 | $15-70/mo managed | $0 free tier |
| Concurrent writes | Single writer | Multiple | Multiple (SQLite replication) |
| Horizontal scaling | Blocked | Full | Full |
| Migration from current | — | Change `DATABASE_URL` | Change `DATABASE_URL` |

SQLx abstracts the driver. All queries in `db.rs` use standard SQL compatible with
both SQLite and PostgreSQL. The migration is one environment variable.

### Streaming Protocol

| | HTTP Range (current) | HLS async (current) | DASH |
|---|---|---|---|
| Time-to-stream | Zero (instant) | Seconds (background) | Seconds |
| Adaptive bitrate | No | Yes (with ABR encode) | Yes |
| Seek performance | O(1) file seek | Instant within buffered segment | Instant |
| Browser support | All | All + native Safari | All except Safari |
| CDN cacheability | Partial | Full (immutable segments) | Full |
| DRM support | No | Yes (FairPlay, Widevine) | Yes |

StreamVault uses both: Range for immediacy, HLS for quality. The player picks
automatically based on `hls_ready`.

---

## 8. The Production Stack ($781/mo)

Handles 5,000-10,000 concurrent viewers, multi-region, full observability.

### Infrastructure

| Component | Service | Monthly Cost |
|---|---|---|
| API nodes (3 regions: iad, lhr, nrt) | Fly.io 4× dedicated-CPU-2x 4GB | ~$200 |
| Transcode workers | Fly.io 4× shared-CPU-4x 8GB | ~$120 |
| Video storage | Cloudflare R2 (10TB, zero egress) | ~$150 |
| Database | Neon PostgreSQL (serverless) | ~$69 |
| Job queue | Upstash Redis | ~$30 |
| CDN + WAF | Cloudflare Pro | ~$20 |
| Error tracking | Sentry Team | ~$26 |
| Metrics + dashboards | Grafana Cloud | ~$100 |
| CI/CD | GitHub Actions | ~$16 |
| Misc (staging, backups) | — | ~$50 |
| **Total** | | **~$781/mo** |

### Code Changes Required

| Change | Effort | Impact |
|---|---|---|
| S3/R2 storage client | Medium | Enables horizontal scaling |
| Presigned download URLs | Small (10 lines) | Removes Axum from video data path |
| PostgreSQL (`DATABASE_URL`) | Trivial | Multi-writer, durability |
| Redis job queue + worker binary | Medium | Independent transcode scaling |
| ABR HLS (720p + 480p) | Small | Adaptive bitrate on slow connections |
| Video thumbnails | Small | Better UI in video grid |
| `duration_secs` via FFprobe | Small | Populated metadata |

### What Does Not Change

The Axum router, all handler logic, database schema, token system, HTTP Range
implementation, and nginx config are **unchanged**. The application does not know
what infrastructure tier it runs on.

---

## 9. Security Hardening

### Current Posture

Token-based obscurity. The share token is the sole access primitive. See
[DESIGN.md §Security](./DESIGN.md#8-security-model) for current mitigations.

### Rate Limiting (add `tower-governor`)

```rust
.route("/api/upload", post(upload_video)
    .layer(DefaultBodyLimit::disable())
    .layer(GovernorLayer {
        config: Arc::new(GovernorConfig::default()
            .per_millisecond(100)
            .burst_size(5))
    }))
```

### CORS Restriction

```rust
CorsLayer::new()
    .allow_origin("https://stream.yourdomain.com"
        .parse::<HeaderValue>().unwrap())
```

### Signed Time-Limited URLs (path from obscurity to real access control)

```
Current: token is permanent — possession = indefinite access

With signed URLs:
  GET /api/videos/{token}
  Axum: HMAC-SHA256(secret_key, token + expires_at)
  Return: stream_url = "...?sig={hmac}&expires={unix_ts}"

  On each streaming request:
    verify HMAC(token + expires_at) == sig
    verify expires_at > now()
    return 403 if either fails
```

This transforms access from obscurity to cryptographic control — without requiring
user accounts.

### Virus Scanning (ClamAV post-upload)

```rust
let result = Command::new("clamdscan").arg(&file_path).status().await?;
if !result.success() {
    tokio::fs::remove_file(&file_path).await?;
    return Err(AppError::BadRequest("File rejected".into()));
}
```

Insert between the upload write and the DB INSERT.

---

## 10. Observability at Scale

### Prometheus Metrics

```toml
# Cargo.toml
prometheus = "0.13"
```

```rust
// Add to router
.route("/metrics", get(metrics_handler))
```

Useful counters: `uploads_total`, `upload_bytes_total`, `stream_requests_total`,
`transcode_success_total`, `transcode_failure_total`, `transcode_duration_seconds`,
`transcode_queue_depth` (from Redis).

### Distributed Tracing

Each upload generates a `trace_id`. Pass it as a header to the FFmpeg job and include
it in all DB queries. Enables correlating "why did this video's HLS fail?" across
log lines from different services.

### Alerting Rules

| Alert | Threshold | Action |
|---|---|---|
| Disk usage | > 80% | Page on-call |
| Transcode queue depth | > 100 jobs | Scale worker fleet |
| Upload error rate | > 5% over 5min | Page on-call |
| p95 stream latency | > 500ms | Investigate CDN / origin |

---

## 11. Real-World Streaming Platform Concerns

These are gaps between the current implementation and what a production streaming
service would need. Each is a documented decision, not an oversight.

### Upload Resumability (TUS Protocol)

A 1GB upload on a mobile connection will fail. Without resumability, the user
restarts from zero. TUS protocol provides resumable uploads via a standardised
`PATCH` interface. Implementation would replace the current multipart endpoint.

### Content-Addressable Deduplication

If two users upload the same file, the current system stores it twice and generates
two tokens. At scale, deduplication (SHA-256 hash of the file, stored in the DB,
shared storage key) reduces storage costs significantly.

### Access Revocation

Current tokens are permanent. For a real sharing service, users need to be able to
delete their videos and invalidate the link. This requires:
1. A `DELETE /api/videos/{token}` endpoint
2. Delete the file from storage
3. Delete the DB row (or mark `deleted=TRUE` for audit trail)
4. If using signed URLs: tokens naturally expire; if using permanent tokens, the DB
   lookup will return 404 after deletion

### Video Expiry

Add `expires_at DATETIME` to the schema. A cron job (or Fly.io scheduled machine)
runs `DELETE FROM videos WHERE expires_at < NOW()` and removes the associated files.

### Adaptive Bitrate Without Re-Encode

The current `-c:v copy` approach produces single-bitrate HLS. A viewer on a 1Mbps
connection watching a 4Mbps-bitrate video will buffer. True ABR requires:
1. A re-encode pass (accept: slow, only viable on worker fleet)
2. Or a bitrate ladder derived from the source (if source is already low bitrate, no
   downgrade needed — check with FFprobe before deciding to re-encode)

### Thumbnail Generation

```bash
ffmpeg -i input.mp4 -ss 00:00:05 -vframes 1 thumbnail.jpg
```

Run after transcode completes. Store in same directory as HLS segments. Return
`thumbnail_url` in the video metadata API response.

---

*StreamVault · Rust · Axum · SQLite → PostgreSQL · Docker → Fly.io · Cloudflare R2*
