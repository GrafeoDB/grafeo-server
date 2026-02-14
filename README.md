[![CI](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml/badge.svg)](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/GrafeoDB/grafeo-server/graph/badge.svg)](https://codecov.io/gh/GrafeoDB/grafeo-server)
[![Docker standard](https://img.shields.io/docker/v/grafeo/grafeo-server/latest?label=standard&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker lite](https://img.shields.io/docker/v/grafeo/grafeo-server/lite?label=lite&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker full](https://img.shields.io/docker/v/grafeo/grafeo-server/full?label=full&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

# Grafeo Server

HTTP server for the [Grafeo](https://github.com/GrafeoDB/grafeo) graph database. Turns Grafeo's embeddable engine into a standalone database server accessible via REST API and web UI.

Pure Rust, single binary. Available in three Docker image variants to match your deployment needs.

## Quick Start

### Docker Hub

```bash
# Standard - all query languages, AI/search features, web UI
docker run -p 7474:7474 grafeo/grafeo-server

# With persistent storage
docker run -p 7474:7474 -v grafeo-data:/data grafeo/grafeo-server --data-dir /data
```

Three image variants are available:

| Variant | Tag | Languages | AI/Search | GWP | Web UI | Use Case |
|---------|-----|-----------|-----------|-----|--------|----------|
| **lite** | `grafeo-server:lite` | GQL only | No | No | No | Sidecar, CI, embedded |
| **gwp** | `grafeo-server:gwp` | GQL only | No | Yes (:7687) | No | Wire protocol, microservice |
| **standard** | `grafeo-server:latest` | All 6 | Yes | Yes (:7687) | Yes | General purpose |
| **full** | `grafeo-server:full` | All 6 | Yes + ONNX embed | Yes (:7687) | Yes | Production, AI/RAG |

```bash
# Lite - GQL only, no web UI, no GWP, smallest image
docker run -p 7474:7474 grafeo/grafeo-server:lite

# GWP - GQL only + wire protocol, no web UI
docker run -p 7474:7474 -p 7687:7687 grafeo/grafeo-server:gwp

# Full - everything including auth, TLS, ONNX embeddings
docker run -p 7474:7474 -p 7687:7687 grafeo/grafeo-server:full
```

Versioned tags: `grafeo-server:0.3.0`, `grafeo-server:0.3.0-lite`, `grafeo-server:0.3.0-gwp`, `grafeo-server:0.3.0-full`.

See [grafeo/grafeo-server on Docker Hub](https://hub.docker.com/r/grafeo/grafeo-server) for all available tags.

### Docker Compose

```bash
docker compose up -d
```

The server is available at `http://localhost:7474`. Web UI at `http://localhost:7474/studio/`. GWP (gRPC) on `localhost:7687`.

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

# SQL/PGQ
curl -X POST http://localhost:7474/sql \
  -H "Content-Type: application/json" \
  -d '{"query": "CALL grafeo.procedures() YIELD name, description"}'

# SPARQL (operates on the RDF triple store, separate from the property graph)
curl -X POST http://localhost:7474/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> INSERT DATA { ex:alice a foaf:Person . ex:alice foaf:name \"Alice\" }"}'

curl -X POST http://localhost:7474/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?name WHERE { ?p a foaf:Person . ?p foaf:name ?name }"}'
```

### Graph Algorithms (CALL Procedures)

All query endpoints support `CALL` procedures for 22+ built-in graph algorithms:

```bash
# List all available algorithms
curl -X POST http://localhost:7474/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CALL grafeo.procedures() YIELD name, description"}'

# PageRank
curl -X POST http://localhost:7474/query \
  -H "Content-Type: application/json" \
  -d '{"query": "CALL grafeo.pagerank({damping: 0.85}) YIELD node_id, score"}'

# Connected components via Cypher
curl -X POST http://localhost:7474/cypher \
  -H "Content-Type: application/json" \
  -d '{"query": "CALL grafeo.connected_components() YIELD node_id, component_id"}'
```

Available algorithms include: PageRank, BFS, DFS, Dijkstra, Bellman-Ford, Connected Components, Strongly Connected Components, Louvain, Label Propagation, Betweenness/Closeness/Degree Centrality, Clustering Coefficient, Topological Sort, Kruskal, Prim, Max Flow, Min-Cost Flow, Articulation Points, Bridges, K-Core, and more.

### Batch Queries

Execute multiple queries atomically in a single request. All queries run within an implicit transaction - if any query fails, the entire batch is rolled back.

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

### GQL Wire Protocol (GWP)

The standard and full builds include a gRPC-based binary wire protocol on port 7687, fully aligned with the GQL type system (ISO/IEC 39075). Use the [`gwp`](https://crates.io/crates/gwp) Rust client or any gRPC client.

```rust
use gwp::client::GqlConnection;
use std::collections::HashMap;

let conn = GqlConnection::connect("http://localhost:7687").await?;
let mut session = conn.create_session().await?;

let mut cursor = session.execute(
    "MATCH (n:Person) RETURN n.name",
    HashMap::new(),
).await?;

let rows = cursor.collect_rows().await?;
session.close().await?;
```

Configure the port with `--gwp-port` or `GRAFEO_GWP_PORT` (default: 7687).

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
| `GRAFEO_GWP_PORT` | `--gwp-port` | `7687` | GQL Wire Protocol (gRPC) port |
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

Grafeo Server uses Cargo feature flags to control both server capabilities and which engine features are compiled in. The default build includes all query languages, AI/search, and schema parsing - matching the **standard** Docker image.

### Server Features

| Feature | Description | Default |
|---------|-------------|---------|
| `owl-schema` | OWL/Turtle schema parsing for database creation | Yes |
| `rdfs-schema` | RDFS schema support (implies `owl-schema`) | Yes |
| `json-schema` | JSON Schema validation for database creation | No |
| `gwp` | GQL Wire Protocol (gRPC) on port 7687 | Yes |
| `auth` | Bearer token and HTTP Basic authentication | No |
| `tls` | Built-in HTTPS via rustls | No |

### Engine: Query Languages

| Feature | Description | Default |
|---------|-------------|---------|
| `gql` | GQL (ISO/IEC 39075) | Yes |
| `cypher` | Cypher (openCypher 9.0) | Yes |
| `sparql` | SPARQL (W3C 1.1) - implies `rdf` | Yes |
| `gremlin` | Gremlin (Apache TinkerPop) | Yes |
| `graphql` | GraphQL | Yes |
| `sql-pgq` | SQL/PGQ (SQL:2023 GRAPH_TABLE) | Yes |
| `all-languages` | All of the above | Yes |

### Engine: Storage & AI

| Feature | Description | Default |
|---------|-------------|---------|
| `storage` | parallel + wal + spill + mmap | Yes |
| `ai` | vector-index + text-index + hybrid-search + cdc | Yes |
| `rdf` | RDF graph model support | Yes |
| `embed` | ONNX embedding generation (~17MB) | No |

### Presets

| Preset | Contents |
|--------|----------|
| `default` | all-languages + ai + rdf + storage + owl-schema + rdfs-schema + gwp |
| `full` | Everything (default + embed + json-schema + auth + tls) |

```bash
# Default build (standard)
cargo build --release

# Lite - GQL + core storage only
cargo build --release --no-default-features --features "gql,storage"

# With authentication
cargo build --release --features auth

# Everything
cargo build --release --features full
```

### Docker Build Targets

The Dockerfile supports three build targets matching these presets:

```bash
docker build --target lite     -t grafeo-server:lite .
docker build --target gwp      -t grafeo-server:gwp .
docker build --target standard -t grafeo-server:standard .   # default
docker build --target full     -t grafeo-server:full .
```

### Feature Discovery

The `/health` endpoint reports which features are compiled into the running server:

```json
{
  "status": "ok",
  "features": {
    "languages": ["gql", "cypher", "sparql", "gremlin", "graphql", "sql-pgq"],
    "engine": ["parallel", "wal", "spill", "mmap", "rdf", "vector-index", "text-index", "hybrid-search", "cdc"],
    "server": ["owl-schema", "rdfs-schema", "gwp"]
  }
}
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

Apache-2.0
