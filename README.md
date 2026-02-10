[![CI](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml/badge.svg)](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/GrafeoDB/grafeo-server/graph/badge.svg)](https://codecov.io/gh/GrafeoDB/grafeo-server)
[![Crates.io](https://img.shields.io/crates/v/grafeo-server.svg)](https://crates.io/crates/grafeo-server)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

# Grafeo Server

HTTP server for the [Grafeo](https://github.com/GrafeoDB/grafeo) graph database. Turns Grafeo's embeddable engine into a standalone database server accessible via REST API and web UI.

Pure Rust, single binary, ~40MB Docker image.

## Quick Start

### Docker Hub

```bash
docker run -p 7474:7474 grafeo/grafeo-server
```

Or with persistent storage:

```bash
docker run -p 7474:7474 -v grafeo-data:/data grafeo/grafeo-server --data-dir /data
```

See [grafeo/grafeo-server on Docker Hub](https://hub.docker.com/r/grafeo/grafeo-server) for all available tags.

### Docker Compose

```bash
docker compose up -d
```

The server is available at `http://localhost:7474`. Web UI at `http://localhost:7474/studio/`.

### From source

```bash
# Build the web UI (optional, embedded at compile time)
cd client && npm ci && npm run build && cd ..

# Build and run
cargo run -- --data-dir ./data

# Or in-memory mode for quick experimentation
cargo run
```

## API

### Query (auto-commit)

```bash
# GQL (default)
curl -X POST http://localhost:7474/query \
  -H "Content-Type: application/json" \
  -d '{"query": "INSERT (:Person {name: '\''Alice'\'', age: 30})"}'

curl -X POST http://localhost:7474/query \
  -H "Content-Type: application/json" \
  -d '{"query": "MATCH (p:Person) RETURN p.name, p.age"}'

# Cypher
curl -X POST http://localhost:7474/cypher \
  -H "Content-Type: application/json" \
  -d '{"query": "MATCH (n) RETURN count(n)"}'

# GraphQL
curl -X POST http://localhost:7474/graphql \
  -H "Content-Type: application/json" \
  -d '{"query": "{ Person { name age } }"}'

# Gremlin
curl -X POST http://localhost:7474/gremlin \
  -H "Content-Type: application/json" \
  -d '{"query": "g.V().hasLabel('\''Person'\'').values('\''name'\'')"}'

# SPARQL (operates on the RDF triple store, separate from the property graph)
curl -X POST http://localhost:7474/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> INSERT DATA { ex:alice a foaf:Person . ex:alice foaf:name \"Alice\" }"}'

curl -X POST http://localhost:7474/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { ?p a foaf:Person . ?p foaf:name ?name }"}'
```

### Batch Queries

Execute multiple queries atomically in a single request. All queries run within an implicit transaction — if any query fails, the entire batch is rolled back.

```bash
curl -X POST http://localhost:7474/batch \
  -H "Content-Type: application/json" \
  -d '{
    "queries": [
      {"query": "INSERT (:Person {name: '\''Alice'\''})"},
      {"query": "INSERT (:Person {name: '\''Bob'\''})"},
      {"query": "MATCH (p:Person) RETURN p.name"}
    ]
  }'
```

### Transactions

```bash
# Begin transaction
SESSION=$(curl -s -X POST http://localhost:7474/tx/begin | jq -r .session_id)

# Execute within transaction
curl -X POST http://localhost:7474/tx/query \
  -H "Content-Type: application/json" \
  -H "X-Session-Id: $SESSION" \
  -d '{"query": "INSERT (:Person {name: '\''Bob'\''})"}'

# Commit
curl -X POST http://localhost:7474/tx/commit \
  -H "X-Session-Id: $SESSION"

# Or rollback
curl -X POST http://localhost:7474/tx/rollback \
  -H "X-Session-Id: $SESSION"
```

### WebSocket

Connect to `ws://localhost:7474/ws` for interactive query execution over a persistent connection. Messages use a JSON-tagged protocol:

```json
// Client → Server: query
{"type": "query", "id": "q1", "query": "MATCH (n) RETURN n", "language": "cypher", "database": "default"}

// Server → Client: result
{"type": "result", "id": "q1", "columns": [...], "rows": [...], "execution_time_ms": 1.2}

// Client → Server: ping
{"type": "ping"}

// Server → Client: pong
{"type": "pong"}
```

The `id` field is optional and echoed back for request/response correlation.

### Health Check

```bash
curl http://localhost:7474/health
```

### API Documentation

Interactive Swagger UI is served at `http://localhost:7474/api/docs/` and the OpenAPI JSON spec at `http://localhost:7474/api/openapi.json`.

## Configuration

All settings are available as CLI flags and environment variables (prefix `GRAFEO_`). CLI flags override environment variables.

| Variable | CLI Flag | Default | Description |
|----------|----------|---------|-------------|
| `GRAFEO_HOST` | `--host` | `0.0.0.0` | Bind address |
| `GRAFEO_PORT` | `--port` | `7474` | Bind port |
| `GRAFEO_DATA_DIR` | `--data-dir` | _(none)_ | Persistence directory (omit for in-memory) |
| `GRAFEO_SESSION_TTL` | `--session-ttl` | `300` | Transaction session timeout (seconds) |
| `GRAFEO_QUERY_TIMEOUT` | `--query-timeout` | `30` | Query execution timeout in seconds (0 = disabled) |
| `GRAFEO_CORS_ORIGINS` | `--cors-origins` | _(none)_ | Comma-separated allowed origins (`*` for all) |
| `GRAFEO_LOG_LEVEL` | `--log-level` | `info` | Tracing log level |
| `GRAFEO_LOG_FORMAT` | `--log-format` | `pretty` | Log format: `pretty` or `json` |
| `GRAFEO_RATE_LIMIT` | `--rate-limit` | `0` | Max requests per window per IP (0 = disabled) |
| `GRAFEO_RATE_LIMIT_WINDOW` | `--rate-limit-window` | `60` | Rate limit window in seconds |

### Authentication (feature: `auth`)

Requires building with `--features auth` or `--features full`.

| Variable | CLI Flag | Default | Description |
|----------|----------|---------|-------------|
| `GRAFEO_AUTH_TOKEN` | `--auth-token` | _(none)_ | Bearer token / API key |
| `GRAFEO_AUTH_USER` | `--auth-user` | _(none)_ | HTTP Basic username (requires password) |
| `GRAFEO_AUTH_PASSWORD` | `--auth-password` | _(none)_ | HTTP Basic password (requires username) |

When an auth token is set, all API endpoints require `Authorization: Bearer <token>` or `X-API-Key: <token>`. `/health`, `/metrics`, and `/studio/` are exempt.

```bash
# Bearer token
grafeo-server --auth-token my-secret-token

# Basic auth
grafeo-server --auth-user admin --auth-password secret

# Both methods can be configured simultaneously
```

### TLS (feature: `tls`)

Requires building with `--features tls` or `--features full`.

| Variable | CLI Flag | Default | Description |
|----------|----------|---------|-------------|
| `GRAFEO_TLS_CERT` | `--tls-cert` | _(none)_ | Path to TLS certificate (PEM) |
| `GRAFEO_TLS_KEY` | `--tls-key` | _(none)_ | Path to TLS private key (PEM) |

```bash
grafeo-server --tls-cert cert.pem --tls-key key.pem
```

### Examples

```bash
# Minimal (in-memory, no auth)
grafeo-server

# Persistent with auth and rate limiting
grafeo-server --data-dir ./data --auth-token my-token --rate-limit 100

# Production with TLS
grafeo-server --data-dir /data --tls-cert /certs/cert.pem --tls-key /certs/key.pem \
  --auth-token $API_TOKEN --rate-limit 1000 --cors-origins "https://app.example.com" \
  --log-format json
```

## Feature Flags

Grafeo Server uses Cargo feature flags to keep the default build lean. Optional features can be enabled at compile time:

| Feature | Description | Default |
|---------|-------------|---------|
| `owl-schema` | OWL/Turtle schema parsing for database creation | Yes |
| `rdfs-schema` | RDFS schema support (implies `owl-schema`) | Yes |
| `json-schema` | JSON Schema validation for database creation | No |
| `auth` | Bearer token and HTTP Basic authentication | No |
| `tls` | Built-in HTTPS via rustls | No |
| `full` | All features above | No |

```bash
# Default build (owl-schema + rdfs-schema)
cargo build --release

# With authentication
cargo build --release --features auth

# Everything enabled
cargo build --release --features full
```

## Development

```bash
cargo build                  # Debug build
cargo test                   # Run tests (default features)
cargo test --features auth   # Run tests including auth tests
cargo fmt --all -- --check   # Check formatting
cargo clippy --all-targets -- -D warnings  # Lint
cargo deny check             # License/advisory audit
```

### Web UI development

```bash
# Terminal 1: Server
cargo run

# Terminal 2: UI dev server with HMR (proxies API to :7474)
cd client && npm run dev
# Open http://localhost:5173
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, code style, and pull request guidelines.

## Related

- [Grafeo](https://github.com/GrafeoDB/grafeo), the embeddable graph database engine
- [grafeo-web](https://github.com/GrafeoDB/grafeo-web), Grafeo in the browser via WebAssembly
- [anywidget-graph](https://github.com/GrafeoDB/anywidget-graph), interactive graph visualization for notebooks

## License

AGPL-3.0-or-later
