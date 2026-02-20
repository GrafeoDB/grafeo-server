[![CI](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml/badge.svg)](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/GrafeoDB/grafeo-server/graph/badge.svg)](https://codecov.io/gh/GrafeoDB/grafeo-server)
[![Docker standard](https://img.shields.io/docker/v/grafeo/grafeo-server/latest?label=standard&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker lite](https://img.shields.io/docker/v/grafeo/grafeo-server/lite?label=lite&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker full](https://img.shields.io/docker/v/grafeo/grafeo-server/full?label=full&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

# Grafeo Server

Graph database server for the [Grafeo](https://github.com/GrafeoDB/grafeo) engine. Provides REST API, embedded web UI and GQL Wire Protocol (gRPC) access to Grafeo's multi-language query engine.

Pure Rust, single binary. Available in three tiers to match different deployment needs.

## Quick Start

### Docker Hub

```bash
# Standard - HTTP + Studio UI, all query languages
docker run -p 7474:7474 grafeo/grafeo-server

# With persistent storage
docker run -p 7474:7474 -v grafeo-data:/data grafeo/grafeo-server --data-dir /data
```

Three image tiers are available:

| Tier         | Tag                    | Transport        | Languages | AI/Search   | Web UI | Binary  |
| ------------ | ---------------------- | ---------------- | --------- | ----------- | ------ | ------- |
| **lite**     | `grafeo-server:lite`   | GWP (gRPC :7687) | GQL       | No          | No     | ~7 MB   |
| **standard** | `grafeo-server:latest` | HTTP (:7474)     | All 6     | No          | Studio | ~21 MB  |
| **full**     | `grafeo-server:full`   | HTTP + GWP       | All 6     | Yes + embed | Studio | ~25 MB  |

```bash
# Lite - GWP-only, GQL + storage, no HTTP/UI
docker run -p 7687:7687 grafeo/grafeo-server:lite --data-dir /data

# Full - everything including GWP, AI, auth, TLS, schemas
docker run -p 7474:7474 -p 7687:7687 grafeo/grafeo-server:full
```

Versioned tags: `grafeo-server:0.4.3`, `grafeo-server:0.4.3-lite`, `grafeo-server:0.4.3-full`.

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

# Build and run (default: HTTP + Studio + all languages)
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

Available algorithms include: PageRank, BFS, DFS, Dijkstra, Bellman-Ford, Connected Components, Strongly Connected Components, Louvain, Label Propagation, Betweenness/Closeness/Degree Centrality, Clustering Coefficient, Topological Sort, Kruskal, Prim, Max Flow, Min-Cost Flow, Articulation Points, Bridges, K-Core and more.

### Admin

Database introspection, maintenance, and index management. Available via both HTTP and GWP (gRPC).

```bash
# Database statistics (node/edge/label/property counts, memory, disk)
curl http://localhost:7474/admin/default/stats

# WAL status
curl http://localhost:7474/admin/default/wal

# Force WAL checkpoint
curl -X POST http://localhost:7474/admin/default/wal/checkpoint

# Database integrity validation
curl http://localhost:7474/admin/default/validate

# Create a property index
curl -X POST http://localhost:7474/admin/default/index \
  -H "Content-Type: application/json" \
  -d '{"index_type": "property", "label": "Person", "property": "name"}'

# Drop an index
curl -X DELETE http://localhost:7474/admin/default/index \
  -H "Content-Type: application/json" \
  -d '{"index_type": "property", "label": "Person", "property": "name"}'
```

### Search

Vector, text, and hybrid search endpoints. Require the corresponding engine features (`vector-index`, `text-index`, `hybrid-search`), available in the full tier.

```bash
# Vector similarity search (KNN via HNSW index)
curl -X POST http://localhost:7474/search/vector \
  -H "Content-Type: application/json" \
  -d '{"database": "default", "vector": [0.1, 0.2, 0.3], "top_k": 10}'

# Full-text BM25 search
curl -X POST http://localhost:7474/search/text \
  -H "Content-Type: application/json" \
  -d '{"database": "default", "query": "graph database", "top_k": 10}'

# Hybrid search (vector + text with rank fusion)
curl -X POST http://localhost:7474/search/hybrid \
  -H "Content-Type: application/json" \
  -d '{"database": "default", "query": "graph database", "vector": [0.1, 0.2, 0.3], "top_k": 10}'
```

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

The lite and full builds include a gRPC-based binary wire protocol on port 7687, fully aligned with the GQL type system (ISO/IEC 39075). Use the [`gwp`](https://crates.io/crates/gwp) Rust client or any gRPC client.

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
| `GRAFEO_PORT` | `--port` | `7474` | HTTP bind port |
| `GRAFEO_DATA_DIR` | `--data-dir` | _(none)_ | Persistence directory (omit for in-memory) |
| `GRAFEO_SESSION_TTL` | `--session-ttl` | `300` | Transaction session timeout (seconds) |
| `GRAFEO_QUERY_TIMEOUT` | `--query-timeout` | `30` | Query execution timeout in seconds (0 = disabled) |
| `GRAFEO_GWP_PORT` | `--gwp-port` | `7687` | GQL Wire Protocol (gRPC) port |
| `GRAFEO_GWP_MAX_SESSIONS` | `--gwp-max-sessions` | `0` | Max concurrent GWP sessions (0 = unlimited) |
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

When an auth token is set, all API endpoints require `Authorization: Bearer <token>` or `X-API-Key: <token>`. `/health`, `/metrics` and `/studio/` are exempt.

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

Grafeo Server uses Cargo feature flags to control which capabilities are compiled in. The architecture separates transport layers (`http`, `gwp`) from the core database functionality, allowing minimal builds for different deployment scenarios.

### Tiers

| Tier | Cargo Command | Transport | Contents |
|------|--------------|-----------|--------------|
| **Lite** | `--no-default-features --features lite` | GWP only | GQL + storage, ~7 MB binary |
| **Standard** | _(default)_ | HTTP | All languages + Studio UI, ~21 MB binary |
| **Full** | `--features full` | HTTP + GWP | Everything including AI, auth, TLS, ~25 MB binary |

```bash
# Standard (default)
cargo build --release

# Lite - GWP-only, GQL + storage
cargo build --release --no-default-features --features lite

# Full - everything
cargo build --release --features full

# Custom - HTTP API without Studio UI
cargo build --release --no-default-features --features "http,all-languages,storage"

# Custom - add auth to standard
cargo build --release --features auth
```

### Transport Features

| Feature | Description | Default |
|---------|-------------|---------|
| `http` | REST API via axum (Swagger UI, OpenAPI) | Yes |
| `studio` | Embedded web UI via rust-embed (requires `http`) | Yes |
| `gwp` | GQL Wire Protocol (gRPC) on port 7687 | No |

### Server Features

| Feature | Description | Default |
|---------|-------------|---------|
| `owl-schema` | OWL/Turtle schema parsing for database creation | No |
| `rdfs-schema` | RDFS schema support (implies `owl-schema`) | No |
| `json-schema` | JSON Schema validation for database creation | No |
| `auth` | Bearer token and HTTP Basic authentication | No |
| `tls` | Built-in HTTPS via rustls | No |

### Engine: Algorithms

| Feature | Description | Default |
|---------|-------------|---------|
| `algos` | 22+ graph algorithms via CALL procedures | Yes |

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
| `ai` | vector-index + text-index + hybrid-search + cdc | No |
| `rdf` | RDF graph model support | No |
| `embed` | ONNX embedding generation | No |

### Docker Build Targets

The Dockerfile supports three build targets matching these tiers:

```bash
docker build --target lite     -t grafeo-server:lite .      # GWP-only, port 7687
docker build --target standard -t grafeo-server:standard .  # HTTP + UI, port 7474 (default)
docker build --target full     -t grafeo-server:full .      # Both ports
```

### Feature Discovery

The `/health` endpoint reports which features are compiled into the running server:

```json
{
  "status": "ok",
  "features": {
    "languages": ["gql", "cypher", "sparql", "gremlin", "graphql", "sql-pgq"],
    "engine": ["parallel", "wal", "spill", "mmap"],
    "server": ["gwp"]
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

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, code style and pull request guidelines.

## Related

- [Grafeo](https://github.com/GrafeoDB/grafeo), the embeddable graph database engine
- [grafeo-web](https://github.com/GrafeoDB/grafeo-web), Grafeo in the browser via WebAssembly
- [anywidget-graph](https://github.com/GrafeoDB/anywidget-graph), interactive graph visualization for notebooks

## License

Apache-2.0
