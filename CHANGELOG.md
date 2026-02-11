# Changelog

All notable changes to grafeo-server are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-02-11

### Added

- **Feature flags** — Cargo features for optional functionality: `auth`, `tls`, `json-schema`, `full` (enables all)
- **Authentication** (feature: `auth`) — bearer token (`Authorization: Bearer <token>` / `X-API-Key`), HTTP Basic auth, constant-time comparison via `subtle`; `/health`, `/metrics`, and `/studio/` exempt from auth
- **Rate limiting** — per-IP fixed-window rate limiter with configurable max requests and window duration; `X-Forwarded-For` aware; background cleanup of stale entries
- **Structured logging** — `--log-format json` option for machine-parseable structured log output alongside the default human-readable `pretty` format
- **Batch query endpoint** — `POST /batch` executes multiple queries in a single request within an implicit transaction; atomic rollback on any failure
- **WebSocket streaming** — `GET /ws` for interactive query execution over persistent connections; JSON-tagged protocol with query, result, error, ping/pong message types; request correlation via optional `id` field
- **TLS support** (feature: `tls`) — built-in HTTPS via `rustls` with `--tls-cert` and `--tls-key` CLI options; ring crypto provider; manual accept loop preserving `ConnectInfo` for IP-based middleware
- **CORS hardening** — deny cross-origin by default (no headers sent); explicit opt-in via `--cors-origins`; wildcard `"*"` supported with warning
- **Request ID tracking** — `X-Request-Id` header on all responses; echoes client-provided ID or generates a UUID
- **AGPL-3.0-or-later LICENSE file**
- **9 new integration tests** — WebSocket query, ping/pong, bad message, auth-required WebSocket; rate limiting enforcement and disabled-when-zero; batch query tests

### Changed

- **Route modularization** — split monolithic `routes.rs` into `routes/query.rs`, `routes/transaction.rs`, `routes/database.rs`, `routes/batch.rs`, `routes/system.rs`, `routes/websocket.rs`, `routes/helpers.rs`, `routes/types.rs`
- **TRY DRY** — consolidated 4 near-identical query handlers into shared `execute_auto_commit`; inlined single-use batch helper; removed dead `contains` guard in system resources; extracted `total_active_sessions()` to `DatabaseManager`
- **Build-time engine version** — `build.rs` extracts grafeo-engine version from `Cargo.lock` at compile time (`GRAFEO_ENGINE_VERSION` env var), eliminating hardcoded version strings
- Default features changed from `["owl-schema", "rdfs-schema", "json-schema"]` to `["owl-schema", "rdfs-schema"]` — `json-schema` now opt-in
- Authentication is now feature-gated behind `auth` — users who don't need auth get a simpler build
- `--cors-origins` default changed from allowing the dev server origin to denying all cross-origin requests
- New dependencies: `futures-util` (WebSocket streams)
- New optional dependencies: `subtle` (auth), `tokio-rustls`, `rustls`, `rustls-pemfile`, `hyper`, `hyper-util` (TLS)
- New dev dependencies: `tokio-tungstenite` (WebSocket tests)
- 60 integration tests total (with `--features auth`), 51 without auth

## [0.2.0] - 2026-02-08

### Added

- **Database creation options** - `POST /db` now accepts `database_type`, `storage_mode`, `options`, `schema_file`, and `schema_filename` fields
- **Database types** - LPG (default), RDF, OWL Schema, RDFS Schema, JSON Schema
- **Storage mode** - per-database in-memory or persistent (mixed mode supported)
- **Resource configuration** - memory limit, WAL durability mode, backward edges toggle, thread count
- **Schema file uploads** - base64-encoded OWL/RDFS/JSON Schema files parsed and loaded on creation (feature-gated: `owl-schema`, `rdfs-schema`, `json-schema`)
- **`GET /system/resources` endpoint** - returns system RAM, allocated memory, available disk, compiled database types, and resource defaults
- **`DatabaseMetadata`** - creation-time metadata stored alongside each database entry
- **Studio UI: CreateDatabaseDialog** - full-form modal with database type radio group, storage mode toggle, memory slider, WAL/durability controls, backward edges toggle, thread selector, and schema file upload
- **Studio UI: database type badges** - non-LPG databases show a type badge (RDF, OWL, RDFS, JSON) in the sidebar list
- **9 new integration tests** - database creation with options, RDF database creation, persistent rejection without data-dir, system resources endpoint, resource allocation tracking, database info metadata, WAL durability options, invalid durability validation, OpenAPI path verification

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
  - `POST /query` - auto-commit query execution
  - `POST /cypher` - Cypher convenience endpoint
  - `POST /graphql` - GraphQL convenience endpoint
  - `POST /gremlin` - Gremlin convenience endpoint
  - `POST /sparql` - SPARQL convenience endpoint
  - `POST /tx/begin`, `/tx/query`, `/tx/commit`, `/tx/rollback` - explicit transactions via `X-Session-Id` header
  - `GET /health` - server health check with version and uptime
  - `GET /metrics` - Prometheus-compatible metrics endpoint
- **Multi-database support**
  - `GET /db` - list all databases
  - `POST /db` - create a named database
  - `DELETE /db/{name}` - delete a database
  - `GET /db/{name}` - database info
  - `GET /db/{name}/stats` - detailed statistics (memory, disk, counts)
  - `GET /db/{name}/schema` - labels, edge types, property keys
  - Default database always exists and cannot be deleted
  - Persistent mode auto-discovers existing databases on startup
  - Migration from single-file to multi-database directory layout
- **Multi-language query dispatch** - GQL (default), Cypher, GraphQL, Gremlin, SPARQL
- **Session management** - DashMap-based concurrent session registry with configurable TTL and background cleanup
- **Bearer token authentication** - optional `GRAFEO_AUTH_TOKEN` env var, exempt endpoints (`/health`, `/studio/`)
- **Per-query timeouts** - global `GRAFEO_QUERY_TIMEOUT` with per-request override via `timeout_ms`
- **Request ID tracking** - `X-Request-Id` header on all responses
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
  - `ci.yml` - fmt, clippy, docs, test (3 OS), security audit, client lint/build, Docker build
  - `publish.yml` - Docker Hub publish on `v*` tags
- **Pre-commit hooks** (prek) - fmt, clippy, deny, typos
- **Integration test suite** - health, query, Cypher, transactions, multi-database CRUD, error cases, UI redirect, auth

[Unreleased]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/GrafeoDB/grafeo-server/releases/tag/v0.1.0
