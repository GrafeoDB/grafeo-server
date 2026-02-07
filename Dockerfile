# --- Stage 1: Build the web UI ---
FROM node:22-slim AS ui-builder
WORKDIR /ui
COPY client/package.json client/package-lock.json* ./
RUN if [ -f package-lock.json ]; then npm ci --ignore-scripts; else npm install --ignore-scripts; fi
COPY client/ .
RUN npm run build

# --- Stage 2: Build the Rust binary ---
FROM rust:1.91-slim AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev curl && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

# Copy built UI so rust-embed can include it
COPY --from=ui-builder /ui/dist client/dist/

# Build in release mode
RUN cargo build --release && strip target/release/grafeo-server

# --- Stage 3: Minimal runtime image ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/grafeo-server /usr/local/bin/grafeo-server

VOLUME /data

EXPOSE 7474

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -sf http://localhost:7474/health || exit 1

ENTRYPOINT ["grafeo-server"]
CMD ["--host", "0.0.0.0", "--port", "7474", "--data-dir", "/data"]
