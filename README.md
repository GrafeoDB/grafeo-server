# Grafeo Server

HTTP server for the [Grafeo](https://github.com/GrafeoDB/grafeo) graph database. Turns Grafeo's embeddable engine into a standalone database server accessible via REST API and web UI.

Pure Rust, single binary, ~20MB Docker image.

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

## Development

```bash
cargo build                  # Debug build
cargo test                   # Run tests
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

### Health Check

```bash
curl http://localhost:7474/health
```

### API Documentation

Interactive Swagger UI is served at `http://localhost:7474/api/docs/` and the OpenAPI JSON spec at `http://localhost:7474/api/openapi.json`.

## Configuration

Environment variables (prefix `GRAFEO_`):

| Variable | Default | Description |
|----------|---------|-------------|
| `GRAFEO_HOST` | `0.0.0.0` | Bind address |
| `GRAFEO_PORT` | `7474` | Bind port |
| `GRAFEO_DATA_DIR` | _(none)_ | Persistence directory (omit for in-memory) |
| `GRAFEO_SESSION_TTL` | `300` | Transaction session timeout (seconds) |
| `GRAFEO_CORS_ORIGINS` | _(none)_ | Comma-separated allowed origins |
| `GRAFEO_LOG_LEVEL` | `info` | Tracing log level |

CLI flags override environment variables:

```bash
grafeo-server --host 0.0.0.0 --port 7474 --data-dir ./mydata --log-level info
grafeo-server  # no --data-dir = in-memory mode
```

## Related

- [Grafeo](https://github.com/GrafeoDB/grafeo), the embeddable graph database engine
- [grafeo-web](https://github.com/GrafeoDB/grafeo-web), Grafeo in the browser via WebAssembly
- [anywidget-graph](https://github.com/GrafeoDB/anywidget-graph), interactive graph visualization for notebooks

## License

AGPL-3.0-or-later
