# StreamVault — Architecture & Scaling

This document covers what it would take to run StreamVault in production — the two
scaling blockers in the current implementation, how to resolve them, and the broader
decisions around storage, compute, and delivery at each tier.

For how the current system works, see [DESIGN.md](./DESIGN.md).

---

## Where the Current Architecture Breaks

The single-container deployment works well for its intended scope. Two things prevent
running multiple API nodes, and they're worth being explicit about:

**Local disk storage.** Uploaded videos live on a Docker volume attached to one
container. A second API node can't read files the first wrote. This blocks every
form of horizontal scaling. The fix is replacing `File::create` / `File::open` in
`handlers/upload.rs` and `handlers/stream.rs` with S3/R2 calls — the rest of the
codebase (router, models, database, token system) is unchanged.

**SQLite single writer.** WAL mode handles concurrent reads fine, but there's only
one writer at a time. Above roughly 100 concurrent uploads, write contention becomes
measurable. SQLite also can't be accessed across machines. The fix is changing
`DATABASE_URL` from `sqlite:///app/...` to `postgres://...` — SQLx abstracts the
driver and all four queries in `db.rs` are standard SQL that runs on both without
modification.

Both are environment-level changes, not code rewrites. The application doesn't know
which storage backend or database it's talking to.

---

## Deployment Tiers

### Tier 0 — Current ($0)

```
docker compose up --build
```

SQLite, local disk, two containers. Works for demos and personal use. Not suitable
for anything with persistence requirements — `docker compose down -v` wipes everything.

---

### Tier 1 — Free Cloud ($0/mo)

Replace local disk with object storage, deploy to Fly.io.

| Component | Service |
|---|---|
| API | Fly.io (3 shared VMs, free tier) |
| Storage | Tigris (Fly.io native, S3-compatible, 5GB free) |
| Frontend | Cloudflare Pages (static bundle, unlimited bandwidth) |
| DNS + SSL | Cloudflare (free) |

Code changes: add an S3-compatible client crate, replace `File::create` with
`s3.put_object`, replace `File::open` with `s3.get_object` as a streaming response.
SQLite stays on a Fly volume. The Axum process is still in the data path for all
video bytes at this tier — acceptable for low traffic, costs ~$0.02/GB beyond free egress.

---

### Tier 2 — Production-Ready ($20–50/mo)

The key change here is getting Axum **out of the video data path** using presigned URLs.

```
Tier 1:  Browser → Axum → S3 → Axum → Browser   (Axum handles every byte)

Tier 2:  Browser → GET /api/videos/{token}
         Axum generates presigned URL (15-min TTL)
         Browser → <video src=presigned_url> → R2 → CDN → Browser
                   (Axum handles zero video bytes)
```

With Cloudflare R2, egress from R2 to Cloudflare CDN is free. A platform serving
terabytes of video monthly pays only storage cost ($0.015/GB), not bandwidth.
This is the main reason R2 is chosen over S3 — S3 egress at streaming scale is
$0.09/GB, which compounds quickly.

Estimated cost: ~$14/mo compute (Fly.io) + ~$7.50/mo storage (R2, 500GB) = ~$22/mo.

The same presigned URL pattern applies to uploads at this tier: instead of the browser
POSTing to Axum which writes to S3, Axum generates a presigned PUT URL and the browser
uploads directly to R2. Axum handles zero upload bytes.

```
POST /api/upload/init  →  Axum: insert pending row, return presigned_put_url + token
PUT {presigned_put_url}  →  browser uploads directly to R2
POST /api/upload/complete  →  Axum: mark row active, spawn transcode job
```

---

### Tier 3 — Scaled ($100–300/mo)

Two additions on top of Tier 2:

**PostgreSQL** (Neon serverless, ~$69/mo) replaces SQLite. Multi-writer, replication,
managed backups. One environment variable change.

**Decoupled transcode workers** replace the in-process `tokio::spawn`. The problem
with in-process transcoding at scale isn't the remux (which is I/O-light) — it's that
if ABR encoding is added later, 50 simultaneous encode jobs will thrash the API node's
CPU and degrade response latency for everyone else. The fix:

```
API node:
  upload_handler → redis.rpush("transcode_queue", { token, s3_key })

Worker (separate fleet):
  job = redis.blpop("transcode_queue")
  s3.download(job.s3_key, /tmp/input)
  ffmpeg -i /tmp/input [transcode args] /tmp/hls/
  s3.upload_dir(/tmp/hls/, "hls/{token}/")
  db.mark_hls_ready(job.token)
```

Workers are right-sized for CPU. API nodes are right-sized for I/O. Failed jobs
re-enqueue with backoff; jobs that fail repeatedly go to a dead-letter queue for
inspection. This also gives visibility into queue depth and transcode duration — things
that are currently invisible.

The other thing this unlocks is **ABR encoding**. With `-c:v copy` (current), a viewer
on a 1Mbps connection watching a 4Mbps source will buffer. On a worker fleet:

```
Input: 1280x720 H.264 @ 4Mbps

Output:
  720p: -vf scale=1280:720 -b:v 2500k
  480p: -vf scale=854:480  -b:v 1000k
  360p: -vf scale=640:360  -b:v 500k

Master playlist references all three renditions; player picks based on bandwidth.
```

Re-encoding is slow (real-time or slower for complex footage) — it only makes sense
on a dedicated worker, not the API hot path.

---

## Technology Choices

### Runtime

The short version: Rust/Axum was chosen because streaming is fundamentally I/O-bound
and the two things that hurt most at scale are GC pauses and memory footprint per
connection.

| | Rust/Axum | Go/Fiber | Node.js/Fastify |
|---|---|---|---|
| Memory at idle | ~5 MB | ~15 MB | ~50 MB |
| GC pauses during streaming | None | Occasional | Occasional |
| Build time (cold) | 3–5 min | ~30s | ~10s |

At 500 concurrent viewers, Rust uses ~50MB RSS. The equivalent Node.js deployment
would be ~300MB. The build time cost is real — cold Docker builds take 3-5 minutes —
but subsequent builds after source changes use cached dependency layers and take ~15s.

### Storage

| | Local disk | Cloudflare R2 | AWS S3 |
|---|---|---|---|
| Setup | Zero | SDK + credentials | SDK + IAM + credentials |
| Storage | $0 | $0.015/GB | $0.023/GB |
| Egress | $0 (local) | **$0** | $0.09/GB |
| Scales horizontally | No | Yes | Yes |

R2 egress is free because Cloudflare-to-Cloudflare traffic isn't metered. For a
video platform where most cost is delivery bandwidth, this is the right default.

### Database

| | SQLite | PostgreSQL | Turso |
|---|---|---|---|
| Setup | Zero | Managed or self-hosted | Sign up + connection string |
| Cost | $0 | ~$15–70/mo | $0 free tier |
| Concurrent writes | Single writer | Multiple | Multiple (SQLite replication) |
| Migration from current | — | Change `DATABASE_URL` | Change `DATABASE_URL` |

The migration is a single environment variable because SQLx abstracts the driver.
All queries in `db.rs` use standard SQL that runs on both without changes.

### Streaming Protocol

The current system uses both protocols — byte-range for immediacy, HLS for quality:

| | HTTP Range | HLS |
|---|---|---|
| Available | Immediately after upload | ~250ms–13s later (varies with duration) |
| Adaptive bitrate | No | Yes (with ABR encode) |
| Seek performance | O(1) file seek | Instant within buffered segment |
| CDN cacheability | Partial | Full (immutable segments) |
| Browser support | All | All + native Safari |

DASH was considered instead of HLS. HLS was chosen because Safari requires it for
native playback, and it's simpler to implement. DASH offers marginally better
efficiency and broader DRM support, but neither matters for this use case.

---

## Security Path

Currently: token-based obscurity. The token is the only access control.

For a public deployment, the evolution is:

**Rate limiting** — add `tower-governor` layer on upload and stream endpoints to
limit per-IP request rate. Prevents both upload abuse and token enumeration.

**CORS restriction** — currently `Any`. Change to your domain in `main.rs`.

**Signed time-limited URLs** — the current tokens are permanent. The correct
production access control model (without full authentication) is HMAC-signed URLs:

```
GET /api/videos/{token}
→ Axum: sign(token + expires_at, secret_key) → presigned_url with TTL
→ On each stream request: verify signature, verify not expired
```

This gives cryptographic access control without user accounts. Current tokens become
an internal identifier; the signed URL is what gets shared.

---

## Observability Gaps

The current system logs uploads and HLS completion via `tracing`. What's missing
for any production use:

- **Metrics:** No Prometheus endpoint. Adding one means adding ~20 lines
  (`prometheus` crate + `/metrics` handler) and counters for uploads, stream
  requests, transcode outcomes, and queue depth.
- **Transcode visibility:** Currently a background `tokio::spawn` with no way to
  know queue depth or failure rate. Moves to the worker queue model at Tier 3.
- **Disk usage:** No alerting before the volume fills. At Tier 1+, object storage
  makes this a non-issue.

---

## Things Not Built That a Real Platform Would Need

**Upload resumability.** A 644MB upload on a flaky mobile connection will fail and
restart from zero. TUS protocol solves this with a standardised `PATCH` interface and
server-side offset tracking. It's a meaningful chunk of work but well-understood.

**Video expiry.** Storage grows indefinitely. Adding `expires_at DATETIME` to the
schema and a scheduled cleanup job is straightforward — the schema is already set up
to accept it without migration.

**Access revocation.** Current tokens are permanent and can't be invalidated without
deleting the database row. A `DELETE /api/videos/{token}` endpoint that removes the
file, the HLS directory, and the DB row is a small addition. If signed URLs are
implemented, expiry handles this automatically.

**Deduplication.** The same file uploaded twice creates two tokens and two copies on
disk. Content-addressable storage (SHA-256 of the file → shared storage key) would
save storage but adds meaningful complexity. Worth it at scale, not at this stage.

---

*StreamVault · Rust · Axum · SQLite → PostgreSQL · Docker → Fly.io · Cloudflare R2*
