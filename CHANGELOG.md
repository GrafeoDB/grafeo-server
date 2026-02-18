# Changelog

All notable changes to grafeo-server are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.2] - 2026-02-18

### Changed

- Bumped grafeo-engine and grafeo-common to 0.5.6 (UNWIND/FOR clauses, SSSP procedure, full node/edge maps in RETURN, UTF-8 lexer fix, embedding model config, text index sync, SPARQL COPY/MOVE/ADD, performance improvements)
- Bumped gwp to 0.1.4 and migrated to builder pattern (`GqlServer::builder()`)
- **GWP TLS**: when `tls` feature is enabled, GWP (gRPC) server uses the same `--tls-cert`/`--tls-key` files as HTTP for encrypted transport
- **GWP authentication**: when `auth` feature is enabled, GWP handshake validates credentials using the same `--auth-token`/`--auth-user`/`--auth-password` as HTTP via a `GwpAuthValidator` adapter
- **GWP idle timeout**: GWP sessions are automatically reaped after `--session-ttl` seconds of inactivity (previously only HTTP sessions had TTL cleanup)
- **GWP max sessions**: `--gwp-max-sessions` CLI flag / `GRAFEO_GWP_MAX_SESSIONS` env var limits concurrent GWP sessions; new handshakes rejected with `RESOURCE_EXHAUSTED` when limit is reached (default: 0 = unlimited)
- **GWP graceful shutdown**: Ctrl-C now drains in-flight gRPC requests, stops the idle session reaper, and awaits GWP task completion before process exit (previously the GWP task was dropped without draining)
- **GWP health check**: `grpc.health.v1.Health` service served automatically on the GWP port via `tonic-health`
- **GWP tracing**: structured `tracing` spans and events on all gRPC methods, including session lifecycle, query execution, transactions, and database operations
- `AuthProvider` now derives `Clone` for cross-transport sharing
- `grafeo-gwp` crate now has `tls` and `auth` feature flags, forwarded from the workspace `tls`/`auth` features

## [0.4.1] - 2026-02-16

### Added

- **Result streaming**: query responses are now encoded and sent incrementally in batches of 1000 rows, reducing peak memory from O(rows x json_size) to O(batch_size x json_size) for the encoded output
  - `RowBatchIter` in `grafeo-service`: transport-agnostic row-batch iterator with `QueryResultExt` extension trait and `DEFAULT_BATCH_SIZE` constant
  - `StreamingQueryBody` in `grafeo-http`: `Stream` implementation producing chunked JSON byte-identical to the previous materialized `QueryResponse` serialization
  - Lazy `GrafeoResultStream` in `grafeo-gwp`: state-machine replacing the pre-built `Vec<ResultFrame>` — large results now produce multiple `Batch` frames (e.g., 2500 rows = 3 batches instead of 1)
- **13 new streaming unit tests**: grafeo-service (7: empty, exact multiple, partial final, larger-than-rows, size_hint, remaining, zero-floors-to-one), grafeo-http (3: empty JSON, materialized equality, multi-chunk), grafeo-gwp (3: empty frames, single batch, multi-batch)
- **8 GWP integration tests**: database lifecycle via GWP client — list, create, delete, get_info, query after create, delete-then-recreate, duplicate error, configure-after-delete error
- **18 per-crate unit tests**: grafeo-service (6: database CRUD, name validation, metrics, language mapping), grafeo-http (6: value encoding, query response, param conversion), grafeo-gwp (6: value conversion, roundtrip, unsupported types)
- **Per-crate CI job**: matrix job in `ci.yml` running `cargo test -p <crate>` independently for grafeo-service, grafeo-http, grafeo-gwp

### Fixed

- **Engine close barrier**: `DatabaseManager::delete()` now explicitly drops the `Arc<DatabaseEntry>` after `close()` to ensure engine resources are fully released before filesystem cleanup
- **Create-after-delete retry**: `DatabaseManager::create()` retries engine creation with 50ms backoff when resource contention is detected after a recent delete
- **`gwp_health_reports_gwp_feature` test**: `spawn_server_with_gwp()` now populates `EnabledFeatures` with `"gwp"` server feature (was empty, causing health check to report no GWP)

### Changed

- All query endpoints (`/query`, `/cypher`, `/graphql`, `/gremlin`, `/sparql`, `/sql`, `/tx/query`) now return streaming `Response<Body>` instead of `Json<QueryResponse>`; HTTP JSON format is unchanged (backward compatible)
- Batch (`/batch`) and WebSocket (`/ws`) endpoints remain materialized (deferred)
- per-crate unit tests amd seperste integrstion tests

## [0.4.0] - 2026-02-15

### Changed

- **Workspace architecture**: restructured from a single crate into a Cargo workspace with 5 member crates:
  - `grafeo-service`: transport-agnostic core: query execution, session management, database operations, metrics, auth, rate limiting, schema loading. Zero HTTP/gRPC dependencies.
  - `grafeo-http`: REST API transport adapter: axum routes, middleware (auth, rate limit, request ID), OpenAPI/Swagger, TLS, error mapping. All HTTP-specific code isolated here.
  - `grafeo-gwp`: GQL Wire Protocol transport adapter: `GqlBackend` impl, value encoding, gRPC serving. Takes `ServiceState` directly (no HTTP dependency).
  - `grafeo-studio`: embedded web UI: rust-embed static file serving, SPA routing. Independent of transport layer.
  - `grafeo-bolt`: placeholder for future Bolt v5 wire protocol (Neo4j driver compatibility).
- **Root binary crate** reduced to 3 files: `main.rs` (transport composition), `config.rs` (CLI), `lib.rs` (re-exports for tests)
- **`EnabledFeatures`** moved from compile-time `cfg!()` checks in HTTP routes to a data struct populated by the binary crate and passed through `AppState`; eliminates phantom feature flags on library crates
- **`GrafeoBackend`** now takes `ServiceState` directly instead of `AppState`, fully decoupling GWP from HTTP
- **`grafeo_http::serve()`** convenience function wraps `axum::serve` with `ConnectInfo<SocketAddr>`; binary crate no longer depends on axum directly
- Version bumped to 0.4.0
- `gwp` dependency updated to 0.1.2, `tonic` updated to 0.14, grafeo-engine/common updated to 0.5.4
- 59 integration tests (renumbered after workspace refactor)

## [0.3.0] - 2026-02-14

### Added

- **GQL Wire Protocol (GWP)**: binary gRPC wire protocol on port 7687 (feature-gated: `gwp`, default on)
  - Full `GqlBackend` implementation bridging `gwp` crate to grafeo-engine
  - Persistent sessions with database switching via `SessionProperty::Graph`
  - Bidirectional value conversion between `grafeo_common::Value` and `gwp::types::Value`
  - Streaming `ResultStream` with header, row batch, and summary frames
  - Transaction support (begin, commit, rollback) via `spawn_blocking`
  - `--gwp-port` CLI flag / `GRAFEO_GWP_PORT` env var (default: 7687)
- **4 new integration tests**: GWP session lifecycle, query execution, transaction commit, health feature detection
- **Dual-port serving**: HTTP on :7474 + GWP (gRPC) on :7687, sharing the same `AppState`
- **`gwp` Docker variant**: GQL-only + GWP wire protocol, no UI, lightweight image for microservices
- **4 Docker variants**: lite, gwp, standard (default), full

### Changed

- GWP is introduced as an opt-in protocol alongside HTTP in this release. Once stability is proven, GWP will become the standard protocol from 0.4.x onwards, with the `gwp` Docker variant promoted to the recommended deployment for wire-protocol clients.

- Bumped version to 0.3.0
- `gwp` added to default features and `full` preset
- `/health` endpoint now reports `"gwp"` in server features
- Dockerfile exposes both ports 7474 and 7687
- New dependencies: `gwp` 0.1, `tonic` 0.12 (both feature-gated)
- 63 integration tests total (4 new GWP tests)

## [0.2.4] - 2026-02-13

### Added

- **CALL procedure support**: 22+ built-in graph algorithms (PageRank, BFS, WCC, Dijkstra, Louvain, etc.) accessible via `CALL grafeo.<algorithm>() YIELD ...` through all query endpoints
- **`POST /sql` endpoint**: SQL/PGQ convenience endpoint for Property Graph Queries with graph pattern matching and CALL procedures
- **SQL/PGQ language dispatch**: `language: "sql-pgq"` supported in `/query` and `/batch` endpoints
- **SQL/PGQ metrics tracking**: per-language Prometheus counters for `sql-pgq` queries
- **8 new integration tests**: CALL procedure listing, PageRank via GQL, WCC via Cypher, unknown procedure error, SQL/PGQ endpoint, language field dispatch, OpenAPI path, metrics tracking

### Changed

- Bumped grafeo-engine to 0.5.3, grafeo-common to 0.5.3
- OpenAPI description updated to mention SQL/PGQ and CALL procedures
- 68 integration tests total

## [0.2.3] - 2026-02-12

### Fixed

- **Docker `full` build**: added `g++` and `cmake` to builder stage for native C++ dependencies (`onig_sys`, `aws-lc-sys`, `ort_sys`) that require `libstdc++` at link time

## [0.2.2] - 2026-02-12

### Added

- **Multi-variant Docker images**: three build targets: `lite` (no UI, GQL + storage only), `standard` (with UI, default features), and `full` (with UI, all features including auth and TLS)
- **Docker build fix**: corrected `Dockerfile.dockerignore` paths for current build context (was referencing parent-directory layout)

## [0.2.1] - 2026-02-11

### Added

- **Feature flags**: Cargo features for optional functionality: `auth`, `tls`, `json-schema`, `full` (enables all)
- **Authentication** (feature: `auth`): bearer token (`Authorization: Bearer <token>` / `X-API-Key`), HTTP Basic auth, constant-time comparison via `subtle`; `/health`, `/metrics`, and `/studio/` exempt from auth
- **Rate limiting**: per-IP fixed-window rate limiter with configurable max requests and window duration; `X-Forwarded-For` aware; background cleanup of stale entries
- **Structured logging**: `--log-format json` option for machine-parseable structured log output alongside the default human-readable `pretty` format
- **Batch query endpoint**: `POST /batch` executes multiple queries in a single request within an implicit transaction; atomic rollback on any failure
- **WebSocket streaming**: `GET /ws` for interactive query execution over persistent connections; JSON-tagged protocol with query, result, error, ping/pong message types; request correlation via optional `id` field
- **TLS support** (feature: `tls`): built-in HTTPS via `rustls` with `--tls-cert` and `--tls-key` CLI options; ring crypto provider; manual accept loop preserving `ConnectInfo` for IP-based middleware
- **CORS hardening**: deny cross-origin by default (no headers sent); explicit opt-in via `--cors-origins`; wildcard `"*"` supported with warning
- **Request ID tracking**: `X-Request-Id` header on all responses; echoes client-provided ID or generates a UUID
- **Apache-2.0 LICENSE file**
- **9 new integration tests**: WebSocket query, ping/pong, bad message, auth-required WebSocket; rate limiting enforcement and disabled-when-zero; batch query tests

### Changed

- **Route modularization**: split monolithic `routes.rs` into `routes/query.rs`, `routes/transaction.rs`, `routes/database.rs`, `routes/batch.rs`, `routes/system.rs`, `routes/websocket.rs`, `routes/helpers.rs`, `routes/types.rs`
- **TRY DRY**: consolidated 4 near-identical query handlers into shared `execute_auto_commit`; inlined single-use batch helper; removed dead `contains` guard in system resources; extracted `total_active_sessions()` to `DatabaseManager`
- **Build-time engine version**: `build.rs` extracts grafeo-engine version from `Cargo.lock` at compile time (`GRAFEO_ENGINE_VERSION` env var), eliminating hardcoded version strings
- Default features changed from `["owl-schema", "rdfs-schema", "json-schema"]` to `["owl-schema", "rdfs-schema"]`; `json-schema` now opt-in
- Authentication is now feature-gated behind `auth`; users who don't need auth get a simpler build
- `--cors-origins` default changed from allowing the dev server origin to denying all cross-origin requests
- New dependencies: `futures-util` (WebSocket streams)
- New optional dependencies: `subtle` (auth), `tokio-rustls`, `rustls`, `rustls-pemfile`, `hyper`, `hyper-util` (TLS)
- New dev dependencies: `tokio-tungstenite` (WebSocket tests)
- 60 integration tests total (with `--features auth`), 51 without auth

## [0.2.0] - 2026-02-08

### Added

- **Database creation options**: `POST /db` now accepts `database_type`, `storage_mode`, `options`, `schema_file`, and `schema_filename` fields
- **Database types**: LPG (default), RDF, OWL Schema, RDFS Schema, JSON Schema
- **Storage mode**: per-database in-memory or persistent (mixed mode supported)
- **Resource configuration**: memory limit, WAL durability mode, backward edges toggle, thread count
- **Schema file uploads**: base64-encoded OWL/RDFS/JSON Schema files parsed and loaded on creation (feature-gated: `owl-schema`, `rdfs-schema`, `json-schema`)
- **`GET /system/resources` endpoint**: returns system RAM, allocated memory, available disk, compiled database types, and resource defaults
- **`DatabaseMetadata`**: creation-time metadata stored alongside each database entry
- **Studio UI: CreateDatabaseDialog**: full-form modal with database type radio group, storage mode toggle, memory slider, WAL/durability controls, backward edges toggle, thread selector, and schema file upload
- **Studio UI: database type badges**: non-LPG databases show a type badge (RDF, OWL, RDFS, JSON) in the sidebar list
- **9 new integration tests**: database creation with options, RDF database creation, persistent rejection without data-dir, system resources endpoint, resource allocation tracking, database info metadata, WAL durability options, invalid durability validation, OpenAPI path verification

### Changed

- Bumped grafeo-engine to 0.4.3, grafeo-common to 0.4.3
- `DatabaseSummary` response now includes `database_type` field
- `DatabaseInfoResponse` now includes `database_type`, `storage_mode`, `memory_limit_bytes`, `backward_edges`, `threads` fields
- DatabasePanel sidebar replaced inline name+create input with "New Database" button opening CreateDatabaseDialog
- New dependencies: `sysinfo` (system resource detection), `base64` (schema file decoding)
- Optional dependencies: `sophia_turtle`, `sophia_api`, `sophia_inmem` (OWL/RDFS parsing), `jsonschema` (JSON Schema validation)

## [0.1.2] - 2026-02-07

### Fixed

- Docker publish workflow tag extraction

### Changed

- Version bump for Docker Hub publish retry

## [0.1.1] - 2026-02-07

### Fixed

- Switched from path dependencies to crates.io versions (grafeo-engine 0.4.0, grafeo-common 0.4.0)
- Dockerfile updated to work without engine source in build context
- ESLint config added for client (`client/eslint.config.js`)
- Rust build and clippy lint fixes
- Code formatting (`cargo fmt`)

### Changed

- Improved metrics module with expanded Prometheus-compatible output
- Refined error types and auth middleware
- Updated integration tests for multi-database support

## [0.1.0] - 2026-02-07

Initial release.

### Added

- **HTTP API** (axum) with JSON request/response on port 7474
  - `POST /query`: auto-commit query execution
  - `POST /cypher`: Cypher convenience endpoint
  - `POST /graphql`: GraphQL convenience endpoint
  - `POST /gremlin`: Gremlin convenience endpoint
  - `POST /sparql`: SPARQL convenience endpoint
  - `POST /tx/begin`, `/tx/query`, `/tx/commit`, `/tx/rollback`: explicit transactions via `X-Session-Id` header
  - `GET /health`: server health check with version and uptime
  - `GET /metrics`: Prometheus-compatible metrics endpoint
- **Multi-database support**
  - `GET /db`: list all databases
  - `POST /db`: create a named database
  - `DELETE /db/{name}`: delete a database
  - `GET /db/{name}`: database info
  - `GET /db/{name}/stats`: detailed statistics (memory, disk, counts)
  - `GET /db/{name}/schema`: labels, edge types, property keys
  - Default database always exists and cannot be deleted
  - Persistent mode auto-discovers existing databases on startup
  - Migration from single-file to multi-database directory layout
- **Multi-language query dispatch**: GQL (default), Cypher, GraphQL, Gremlin, SPARQL
- **Session management**: DashMap-based concurrent session registry with configurable TTL and background cleanup
- **Bearer token authentication**: optional `GRAFEO_AUTH_TOKEN` env var, exempt endpoints (`/health`, `/studio/`)
- **Per-query timeouts**: global `GRAFEO_QUERY_TIMEOUT` with per-request override via `timeout_ms`
- **Request ID tracking**: `X-Request-Id` header on all responses
- **Embedded web UI** (rust-embed) at `/studio/`
  - CodeMirror 6 query editor with multi-language syntax highlighting
  - Tabbed query sessions with history
  - Table view for query results
  - Graph visualization (Sigma.js + Graphology) with force-directed layout
  - Node detail panel for inspecting properties
  - Database sidebar with create/delete/select
  - Keyboard shortcuts with help overlay
  - Status bar with connection info and timing
- **OpenAPI documentation** (utoipa + Swagger UI) at `/api/docs/`
- **Configuration** via CLI args and `GRAFEO_*` environment variables
  - `--host`, `--port`, `--data-dir`, `--session-ttl`, `--cors-origins`, `--query-timeout`, `--auth-token`, `--log-level`
- **Docker support**
  - 3-stage Dockerfile: Node (UI build) -> Rust (binary build) -> debian-slim (runtime)
  - Multi-arch images (amd64, arm64) published to Docker Hub
  - docker-compose.yml for quick start
- **CI/CD** (GitHub Actions)
  - `ci.yml`: fmt, clippy, docs, test (3 OS), security audit, client lint/build, Docker build
  - `publish.yml`: Docker Hub publish on `v*` tags
- **Pre-commit hooks** (prek): fmt, clippy, deny, typos
- **Integration test suite**: health, query, Cypher, transactions, multi-database CRUD, error cases, UI redirect, auth

[Unreleased]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.2...HEAD
[0.4.2]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/GrafeoDB/grafeo-server/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.4...v0.3.0
[0.2.4]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/GrafeoDB/grafeo-server/releases/tag/v0.1.0
