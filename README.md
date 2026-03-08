[![CI](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml/badge.svg)](https://github.com/GrafeoDB/grafeo-server/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/GrafeoDB/grafeo-server/graph/badge.svg)](https://codecov.io/gh/GrafeoDB/grafeo-server)
[![Docker standard](https://img.shields.io/docker/v/grafeo/grafeo-server/latest?label=standard&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker gwp](https://img.shields.io/docker/v/grafeo/grafeo-server/gwp?label=gwp&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker bolt](https://img.shields.io/docker/v/grafeo/grafeo-server/bolt?label=bolt&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![Docker full](https://img.shields.io/docker/v/grafeo/grafeo-server/full?label=full&logo=docker)](https://hub.docker.com/r/grafeo/grafeo-server)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

# Grafeo Server

Graph database server for the [Grafeo](https://github.com/GrafeoDB/grafeo) engine. Provides REST API, embedded web UI, GQL Wire Protocol (gRPC), and Bolt v5.x wire protocol access to Grafeo's multi-language query engine.

Pure Rust, single binary. Available in three tiers to match different deployment needs.

## Quick Start

### Docker Hub

```bash
# Standard - HTTP + Studio UI, all query languages
docker run -p 7474:7474 grafeo/grafeo-server

# With persistent storage
docker run -p 7474:7474 -v grafeo-data:/data grafeo/grafeo-server --data-dir /data
```

Four image tiers are available:

| Tier         | Tag                    | Transport         | Languages | AI/Search   | Web UI | Binary  |
| ------------ | ---------------------- | ----------------- | --------- | ----------- | ------ | ------- |
| **gwp**      | `grafeo-server:gwp`    | GWP (gRPC :7688)  | GQL       | No          | No     | ~7 MB   |
| **bolt**     | `grafeo-server:bolt`   | Bolt v5 (:7687)   | Cypher    | No          | No     | ~8 MB   |
| **standard** | `grafeo-server:latest` | HTTP (:7474)      | All 6     | No          | Studio | ~21 MB  |
| **full**     | `grafeo-server:full`   | HTTP + GWP + Bolt | All 6     | Yes + embed | Studio | ~25 MB  |

```bash
# GWP - gRPC wire protocol, GQL only
docker run -p 7688:7688 grafeo/grafeo-server:gwp --data-dir /data

# Bolt - Neo4j-compatible wire protocol, Cypher
docker run -p 7687:7687 grafeo/grafeo-server:bolt --data-dir /data

# Full - everything including AI, auth, TLS, schemas
docker run -p 7474:7474 -p 7687:7687 -p 7688:7688 grafeo/grafeo-server:full
```

Versioned tags: `grafeo-server:0.4.6`, `grafeo-server:0.4.6-gwp`, `grafeo-server:0.4.6-bolt`, `grafeo-server:0.4.6-full`.

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

# Query plan cache statistics
curl http://localhost:7474/admin/default/cache

# Clear the query plan cache
curl -X POST http://localhost:7474/admin/default/cache/clear
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

The gwp and full builds include a gRPC-based binary wire protocol on port 7688, fully aligned with the GQL type system (ISO/IEC 39075). Use the [`gwp`](https://crates.io/crates/gwp) (0.1.6) Rust client or any gRPC client.

```rust
use gwp::client::GqlConnection;
use std::collections::HashMap;

let conn = GqlConnection::connect("http://localhost:7688").await?;
let mut session = conn.create_session().await?;

let mut cursor = session.execute(
    "MATCH (n:Person) RETURN n.name",
    HashMap::new(),
).await?;

let rows = cursor.collect_rows().await?;
session.close().await?;
```

Configure the port with `--gwp-port` or `GRAFEO_GWP_PORT` (default: 7688).

### Bolt v5.x (BoltR)

The bolt and full builds include a Bolt v5.x wire protocol on port 7687, compatible with Neo4j drivers. Use the [`boltr`](https://crates.io/crates/boltr) (0.1.1) Rust client or any Bolt v5 driver (Python `neo4j`, JavaScript `neo4j-driver`, etc.).

Configure the port with `--bolt-port` or `GRAFEO_BOLT_PORT` (default: 7687).

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
| `GRAFEO_GWP_PORT` | `--gwp-port` | `7688` | GQL Wire Protocol (gRPC) port |
| `GRAFEO_GWP_MAX_SESSIONS` | `--gwp-max-sessions` | `0` | Max concurrent GWP sessions (0 = unlimited) |
| `GRAFEO_BOLT_PORT` | `--bolt-port` | `7687` | Bolt v5.x wire protocol port |
| `GRAFEO_BOLT_MAX_SESSIONS` | `--bolt-max-sessions` | `0` | Max concurrent Bolt sessions (0 = unlimited) |
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
| **GWP** | `--no-default-features --features gwp` | GWP (gRPC) | GQL + storage, ~7 MB |
| **Bolt** | `--no-default-features --features bolt` | Bolt v5 | Cypher + storage, ~8 MB |
| **Standard** | _(default)_ | HTTP | All languages, Studio UI, algos, ~21 MB |
| **Full** | `--features full` | HTTP + GWP + Bolt | Everything including AI, auth, TLS, ~25 MB |

```bash
# Standard (default)
cargo build --release

# GWP - gRPC wire protocol, GQL only
cargo build --release --no-default-features --features gwp

# Bolt - Neo4j-compatible, Cypher only
cargo build --release --no-default-features --features bolt

# Full - everything
cargo build --release --features full
```

### Build Your Own

Start with `--no-default-features` and pick what you need. The matrix below shows what works with what.

**Transport** (pick one or more):

| Feature | Description | Port |
|---------|-------------|------|
| `http` | REST API via axum (Swagger, OpenAPI, WebSocket) | 7474 |
| `gwp` | GQL Wire Protocol (gRPC) | 7688 |
| `bolt` | Bolt v5 wire protocol (Neo4j compatible) | 7687 |

**Query languages** (pick individually or use `all-languages`):

| Feature | Description | Notes |
|---------|-------------|-------|
| `gql` | GQL (ISO/IEC 39075) | Works with all transports |
| `cypher` | Cypher (openCypher 9.0) | Works with all transports |
| `sparql` | SPARQL (W3C 1.1) | Implies `rdf` |
| `gremlin` | Gremlin (Apache TinkerPop) | Works with all transports |
| `graphql` | GraphQL | Works with all transports |
| `sql-pgq` | SQL/PGQ (SQL:2023 GRAPH_TABLE) | Works with all transports |

**Engine capabilities**:

| Feature | Description | Requires |
|---------|-------------|----------|
| `storage` | parallel + wal + spill + mmap | Nothing (recommended for all builds) |
| `algos` | 22+ graph algorithms via CALL procedures | Nothing |
| `ai` | vector-index + text-index + hybrid-search + cdc | Nothing |
| `rdf` | RDF triple store | Enabled automatically by `sparql` |
| `embed` | In-process ONNX embedding generation (~17 MB) | Nothing |

**Server extras** (require `http`):

| Feature | Description | Requires |
|---------|-------------|----------|
| `studio` | Embedded web UI | `http` |
| `auth` | Bearer token + HTTP Basic auth | Any transport |
| `tls` | Built-in HTTPS/gRPCS via rustls | Any transport |
| `owl-schema` | OWL/Turtle schema loading | Nothing |
| `rdfs-schema` | RDFS schema support | `owl-schema` (implied) |
| `json-schema` | JSON Schema validation | Nothing |

**Compatibility notes**:
- `studio` requires `http` (no standalone Studio)
- `sparql` implies `rdf` (RDF store is always created)
- `gwp` uses port 7688 and `bolt` uses port 7687 by default, configurable via `--gwp-port` / `--bolt-port`
- `auth` and `tls` apply to whichever transports are enabled
- All features compose freely otherwise

**Examples**:

```bash
# HTTP API without Studio UI
cargo build --release --no-default-features --features "http,all-languages,storage"

# Standard + auth
cargo build --release --features auth

# GWP + Bolt (both wire protocols, no HTTP)
cargo build --release --no-default-features --features "gwp,bolt,gql,cypher,storage"

# Minimal AI server (HTTP + GQL + vector/text search)
cargo build --release --no-default-features --features "http,gql,storage,ai"

# Edge deployment (GWP + GQL, no algorithms)
cargo build --release --no-default-features --features "gwp,gql,storage"
```

### Docker Build Targets

The Dockerfile supports four build targets matching the tiers:

```bash
docker build --target gwp      -t grafeo-server:gwp .       # GWP-only, port 7688
docker build --target bolt     -t grafeo-server:bolt .       # Bolt-only, port 7687
docker build --target standard -t grafeo-server:standard .   # HTTP + UI, port 7474 (default)
docker build --target full     -t grafeo-server:full .       # All ports
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
