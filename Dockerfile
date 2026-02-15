# =============================================================================
# Grafeo Server — Multi-variant Docker build
#
# Build targets:
#   docker build --target lite     -t grafeo-server:lite .
#   docker build --target standard -t grafeo-server:standard .
#   docker build --target full     -t grafeo-server:full .
#
# Default target (no --target) builds "standard".
#
# Tiers:
#   lite     — GWP-only (gRPC), GQL + storage, no HTTP/UI
#   standard — HTTP + Studio UI + all languages + storage (default)
#   full     — HTTP + GWP + Studio + AI + auth + TLS + schemas
# =============================================================================

# --- Stage: Build the web UI ---
FROM node:22-slim AS ui-builder
WORKDIR /ui
COPY grafeo-server/client/package.json grafeo-server/client/package-lock.json* ./
RUN if [ -f package-lock.json ]; then npm ci --ignore-scripts; else npm install --ignore-scripts; fi
COPY grafeo-server/client/ .
RUN npm run build

# --- Stage: Shared Rust base ---
FROM rust:1.91-slim AS rust-base
RUN apt-get update && apt-get install -y pkg-config libssl-dev curl g++ cmake protobuf-compiler && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY grafeo/ ../grafeo/
COPY grafeo-server/Cargo.toml grafeo-server/Cargo.lock grafeo-server/build.rs ./
COPY grafeo-server/crates/ crates/
COPY grafeo-server/src/ src/

# --- Build: lite (GWP-only, GQL + storage, no HTTP/UI) ---
FROM rust-base AS build-lite
RUN mkdir -p client/dist && \
    cargo build --release --no-default-features --features "lite" && \
    strip target/release/grafeo-server

# --- Build: standard (HTTP + Studio UI, all languages, default features) ---
FROM rust-base AS build-standard
COPY --from=ui-builder /ui/dist client/dist/
RUN cargo build --release && \
    strip target/release/grafeo-server

# --- Build: full (HTTP + GWP + Studio + AI + auth + TLS + schemas) ---
FROM rust-base AS build-full
COPY --from=ui-builder /ui/dist client/dist/
RUN cargo build --release --features full && \
    strip target/release/grafeo-server

# --- Runtime: lite (GWP-only, no HTTP healthcheck) ---
FROM debian:bookworm-slim AS runtime-lite
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
VOLUME /data
EXPOSE 7687
ENTRYPOINT ["grafeo-server"]
CMD ["--host", "0.0.0.0", "--data-dir", "/data"]

# --- Runtime: shared HTTP base ---
FROM debian:bookworm-slim AS runtime-http
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
VOLUME /data
EXPOSE 7474
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -sf http://localhost:7474/health || exit 1
ENTRYPOINT ["grafeo-server"]
CMD ["--host", "0.0.0.0", "--port", "7474", "--data-dir", "/data"]

# --- Runtime: full (both ports) ---
FROM debian:bookworm-slim AS runtime-full
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
VOLUME /data
EXPOSE 7474 7687
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -sf http://localhost:7474/health || exit 1
ENTRYPOINT ["grafeo-server"]
CMD ["--host", "0.0.0.0", "--port", "7474", "--data-dir", "/data"]

# --- Final: lite ---
FROM runtime-lite AS lite
COPY --from=build-lite /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# --- Final: standard ---
FROM runtime-http AS standard
COPY --from=build-standard /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# --- Final: full ---
FROM runtime-full AS full
COPY --from=build-full /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# Default target is standard
FROM standard
