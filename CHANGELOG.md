# Changelog

All notable changes to grafeo-server are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Bumped grafeo-engine to 0.4.1, grafeo-common to 0.4.1

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

[Unreleased]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/GrafeoDB/grafeo-server/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/GrafeoDB/grafeo-server/releases/tag/v0.1.0
