# Contributing to Grafeo Server

Thank you for your interest in contributing to Grafeo Server. This guide covers the development workflow, code standards, and pull request process.

## Getting Started

### Prerequisites

- **Rust** 1.91.1+ (edition 2024)
- **Node.js** 18+ and npm (for web UI development)
- **Docker** (optional, for container builds)

### Clone and Build

```bash
git clone https://github.com/GrafeoDB/grafeo-server.git
cd grafeo-server

# Build with default features
cargo build

# Build with all features
cargo build --features full

# Run in-memory mode
cargo run
```

### Running Tests

```bash
# Default features (51 integration tests)
cargo test

# With authentication tests (60 integration tests)
cargo test --features auth

# All features
cargo test --features full
```

### Web UI

```bash
cd client
npm ci
npm run build    # Production build (embedded into server binary)
npm run dev      # Dev server with HMR on http://localhost:5173
npm run lint     # ESLint
npx tsc --noEmit # Type check
```

## Development Workflow

1. Create a branch from `main`
2. Make your changes
3. Run the checks below
4. Open a pull request against `main`

### Pre-submit Checks

```bash
# Formatting
cargo fmt --all -- --check

# Linting (both default and full feature sets)
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features full -- -D warnings

# Tests
cargo test
cargo test --features auth

# License/advisory audit
cargo deny check
```

If you have [prek](https://github.com/nicholasgasior/prek) installed, `prek run --all-files` runs formatting, clippy, deny, and typo checks in one command.

## Code Style

The project follows the [Grafeo Rust Code Style Guide](.claude/CODE_STYLE.md). Key points:

- **Formatting** - Use `cargo fmt` defaults. No custom rustfmt config.
- **Linting** - `clippy::all` and `clippy::pedantic` as warnings. See `Cargo.toml` `[lints.clippy]` for allowed exceptions.
- **Error handling** - Use `thiserror` enums. Never panic in library code. Use `?` propagation.
- **Naming** - No abbreviations in public APIs (exception: domain-standard like `Id`, `WAL`, `RDF`). `PascalCase` for types, `snake_case` for functions.
- **Imports** - Group by: std, external crates, workspace crates, current crate. Alphabetize within groups.
- **Documentation** - All public items should have doc comments. First line is verb-first (e.g., "Creates a new...", "Returns the...").
- **Unsafe** - `unsafe_code = "warn"` at crate level. Requires `// SAFETY:` justification.

### Architecture Patterns

- **Async boundaries** - Database operations run via `tokio::task::spawn_blocking` to avoid blocking the async runtime
- **Concurrency** - `parking_lot::Mutex` (not std), `DashMap` for session/rate-limit maps
- **State** - `AppState` wraps `Arc<Inner>` and is cloned into all handlers
- **Feature gating** - Use `#[cfg(feature = "...")]` on modules, struct fields, enum variants, and tests as appropriate

## Project Structure

```
src/
  main.rs            Entry point, CLI config, graceful shutdown
  lib.rs             Public module exports (enables integration tests)
  config.rs          CLI/env configuration (clap derive)
  state.rs           AppState with Arc<Inner>
  error.rs           ApiError enum -> JSON responses
  database_manager.rs  Multi-database management
  sessions.rs        DashMap session registry with TTL
  auth.rs            Authentication middleware (feature: auth)
  rate_limit.rs      Per-IP rate limiting
  request_id.rs      X-Request-Id tracking
  metrics.rs         Prometheus-compatible metrics
  schema.rs          Schema parsing (feature-gated)
  tls.rs             HTTPS via rustls (feature: tls)
  ui.rs              rust-embed static file serving
  routes/
    mod.rs           Router assembly, CORS, OpenAPI
    query.rs         POST /query, /cypher, /graphql, /gremlin, /sparql
    batch.rs         POST /batch
    transaction.rs   POST /tx/begin, /tx/query, /tx/commit, /tx/rollback
    database.rs      GET/POST /db, GET/DELETE /db/{name}, stats, schema
    system.rs        GET /health, /metrics, /system/resources
    websocket.rs     GET /ws
    types.rs         Request/response structs
    helpers.rs       Shared handler utilities
tests/
  integration.rs     Integration test suite (runs against in-memory server)
client/              React + TypeScript web UI
```

## Feature Flags

When adding new optional functionality, prefer feature-gating it:

| Feature | What it gates |
|---------|---------------|
| `owl-schema` | OWL/Turtle schema parsing |
| `rdfs-schema` | RDFS schema support |
| `json-schema` | JSON Schema validation |
| `auth` | Authentication middleware + `subtle` crate |
| `tls` | HTTPS support + `rustls` crate family |
| `full` | All of the above |

Remember to:
- Add the feature to `full` in `Cargo.toml`
- Gate modules with `#[cfg(feature = "...")]` in `lib.rs`
- Gate related tests in `tests/integration.rs`
- Update CI to test both with and without the feature

## Pull Requests

- Keep PRs focused on a single concern
- Include tests for new functionality
- Update `CHANGELOG.md` under `[Unreleased]` with your changes
- Ensure all CI checks pass (formatting, clippy, tests, audit)
- Link related issues in the PR description

## Reporting Issues

File issues at [github.com/GrafeoDB/grafeo-server/issues](https://github.com/GrafeoDB/grafeo-server/issues). Include:

- Grafeo Server version (`grafeo-server --version`)
- OS and architecture
- Steps to reproduce
- Expected vs actual behavior

## License

By contributing, you agree that your contributions will be licensed under AGPL-3.0-or-later.
