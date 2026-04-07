FROM rust:latest AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs
RUN cargo build --release || true
RUN rm -rf src target/release/deps/streamvault*
COPY src ./src
RUN cargo build --release

FROM rust:latest AS runtime
RUN apt-get update && apt-get install -y ca-certificates ffmpeg && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/streamvault ./streamvault
ENV UPLOAD_DIR=/data/uploads
ENV DB_PATH=/app/streamvault.db
ENV BASE_URL=http://localhost
ENV BIND_ADDR=0.0.0.0:3000
ENV RUST_LOG=streamvault=info
EXPOSE 3000
ENTRYPOINT ["./streamvault"]
