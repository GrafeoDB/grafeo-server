# =============================================================================
# Grafeo Server — Multi-variant Docker build
#
# Build targets:
#   docker build --target lite     -t grafeo-server:lite .
#   docker build --target standard -t grafeo-server:standard .
#   docker build --target full     -t grafeo-server:full .
#
# Default target (no --target) builds "standard".
# =============================================================================

# --- Stage: Build the web UI ---
FROM node:22-slim AS ui-builder
WORKDIR /ui
COPY client/package.json client/package-lock.json* ./
RUN if [ -f package-lock.json ]; then npm ci --ignore-scripts; else npm install --ignore-scripts; fi
COPY client/ .
RUN npm run build

# --- Stage: Shared Rust base ---
FROM rust:1.91-slim AS rust-base
RUN apt-get update && apt-get install -y pkg-config libssl-dev curl && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock* build.rs ./
COPY src/ src/

# --- Build: lite (no UI, GQL + core storage only) ---
FROM rust-base AS build-lite
RUN mkdir -p client/dist && \
    cargo build --release --no-default-features --features "gql,storage" && \
    strip target/release/grafeo-server

# --- Build: standard (with UI, default features) ---
FROM rust-base AS build-standard
COPY --from=ui-builder /ui/dist client/dist/
RUN cargo build --release && \
    strip target/release/grafeo-server

# --- Build: full (with UI, all features) ---
FROM rust-base AS build-full
COPY --from=ui-builder /ui/dist client/dist/
RUN cargo build --release --features full && \
    strip target/release/grafeo-server

# --- Shared runtime base ---
FROM debian:bookworm-slim AS runtime-base
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
VOLUME /data
EXPOSE 7474
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -sf http://localhost:7474/health || exit 1
ENTRYPOINT ["grafeo-server"]
CMD ["--host", "0.0.0.0", "--port", "7474", "--data-dir", "/data"]

# --- Final: lite ---
FROM runtime-base AS lite
COPY --from=build-lite /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# --- Final: standard (default) ---
FROM runtime-base AS standard
COPY --from=build-standard /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# --- Final: full ---
FROM runtime-base AS full
COPY --from=build-full /build/target/release/grafeo-server /usr/local/bin/grafeo-server

# Default target is standard
FROM standard
