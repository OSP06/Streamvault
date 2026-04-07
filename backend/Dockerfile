# ── Stage 1: Build ────────────────────────────────────────────────────────────
# We use the full Rust image only for compilation. It is never shipped.
FROM rust:1.86-slim AS builder

# System deps needed to compile (openssl, pkg-config for sqlx)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── Dependency caching trick ──────────────────────────────────────────────────
# Copy only the manifest first and build a fake main.rs.
# Docker caches this layer. On source-only changes, cargo does NOT
# re-download or re-compile dependencies — only the app binary rebuilds.
# Without this, every `docker compose up --build` after a source change
# recompiles ~80 crates from scratch (~5 min → ~15 sec).
COPY Cargo.toml ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release 2>/dev/null || true
# Remove the fake binary artifacts so the real source triggers a full relink
RUN rm -f target/release/streamvault target/release/deps/streamvault*

# Now copy real source and do the real build
COPY src ./src
RUN cargo build --release

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
# debian:bookworm-slim is ~90MB vs rust:latest ~1.5GB.
# The compiled Rust binary links against glibc — bookworm provides it.
# ffmpeg is the only runtime dependency beyond the binary itself.
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y \
    ca-certificates \
    ffmpeg \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/streamvault ./streamvault

# Environment defaults — all can be overridden in docker-compose.yml
ENV UPLOAD_DIR=/data/uploads
ENV DB_PATH=/app/streamvault.db
ENV BASE_URL=http://localhost
ENV BIND_ADDR=0.0.0.0:3000
ENV RUST_LOG=streamvault=info

EXPOSE 3000

# curl is installed above specifically for this healthcheck
HEALTHCHECK --interval=10s --timeout=5s --start-period=15s --retries=5 \
    CMD curl -f http://localhost:3000/health || exit 1

ENTRYPOINT ["./streamvault"]