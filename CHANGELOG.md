# Changelog

All notable changes to grafeo-server are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.36] - Unreleased

Role-based access control, per-database token scoping, and engine 0.5.36 alignment (Identity, Role, session-level permission enforcement).

### Added

- **Per-database token scoping**: runtime token CRUD with role and database restrictions ([#52](https://github.com/GrafeoDB/grafeo-server/issues/52), implementation by [@Michaelzag](https://github.com/Michaelzag))
  - `POST /admin/tokens`: create a scoped API token (plaintext returned once)
  - `GET /admin/tokens`: list all tokens (metadata only)
  - `GET /admin/tokens/{id}`: get a single token
  - `DELETE /admin/tokens/{id}`: revoke a token (immediate effect)
- **Token roles**: `admin` (full access), `read-write` (data mutations), `read-only` (queries only). Mapped to engine `Role::Admin`, `Role::ReadWrite`, `Role::ReadOnly`
- **Token database scoping**: tokens can be restricted to specific databases. Empty list = all databases. Enforced at the HTTP middleware layer (403 for scope violations)
- **`TokenStore`**: SHA-256 hashed token storage in `{data_dir}/tokens.json` with atomic writes via temp file + rename
- **`AuthContext::identity()`**: builds engine `Identity` from token metadata, caps to `ReadOnly` when server is in read-only mode
- **CLI flags**: `--auth-user` / `--auth-password` for HTTP Basic auth, `--token-store-path` for explicit token store location
- **HTTP Basic auth**: `Authorization: Basic <base64>` with constant-time comparison (always admin scope)
- **GWP identity-scoped sessions**: GWP (gRPC) connections now pass authenticated identity through to session creation via `SessionConfig.auth_info`. Sessions are scoped to the token's role, with engine-level permission enforcement
- **BoltR identity-scoped sessions**: Bolt v5.x connections now pass authenticated identity via `set_session_auth()` after LOGON. Sessions are recreated with the token's role, matching HTTP transport behavior

### Changed

- **grafeo-engine 0.5.36**: role-based access control, `Identity`/`Role`/`StatementKind` types, `session_with_identity()`, `session_with_role()`, `PermissionDenied` error with context
- **Identity-scoped sessions**: all HTTP route handlers (query, batch, transaction, SPARQL protocol, Graph Store protocol, WebSocket) now create sessions via `session_with_identity()` instead of `session()`. The engine enforces statement-level permissions after parsing, before execution
- **`check_bearer` returns `Option<TokenInfo>`** instead of `bool`, carrying role and database scope through the middleware chain
- **`TokenScope.role` uses engine `Role` enum** instead of string, with serde bridge for JSON wire format (`"admin"`, `"read-write"`, `"read-only"`)
- **`session_read_only()` replaced**: all GWP and BoltR backends use `session_with_role(Role::ReadOnly)` instead of the deprecated `session_read_only()`
- **Permission denied errors**: engine `PermissionDenied` (via `QueryError::Semantic`) now maps to `ServiceError::Forbidden` (HTTP 403) instead of `BadRequest` (400)
- **`--auth-token` preserved**: legacy single token stays as root/bootstrap credential with implicit `Admin` role and no database restrictions
- **Token store auto-activation**: only derives `{data_dir}/tokens.json` when at least one credential is configured, preventing spurious auth activation from `--data-dir` alone
- **gwp dependency updated to 0.2.1**: `AuthValidator` returns `AuthInfo`, `SessionConfig` carries `auth_info`
- **boltr dependency updated to 0.2.0**: `AuthValidator` returns `AuthInfo`, `BoltBackend.set_session_auth()`
- **GWP/BoltR validators use PendingAuth mechanism** (DashMap) to pass full `TokenInfo` from validator to backend
- **GWP/BoltR sessions preserve identity** across database switches and session resets

## [0.5.35] - 2026-04-11

Backup/restore, CDC error handling, and engine 0.5.35 alignment (non-exhaustive enums, private QueryResult rows, new feature flags).

### Added

- **Backup/restore endpoints**: hot snapshots via MVCC, no write downtime ([#53](https://github.com/GrafeoDB/grafeo-server/pull/53) by [@Michaelzag](https://github.com/Michaelzag))
  - `POST /admin/{db}/backup`: create a `.grafeo` snapshot
  - `GET /admin/{db}/backups`: list backups for a database
  - `GET /backups`: list all backups across databases
  - `POST /admin/{db}/restore`: restore from a backup file (automatic safety backup before restore)
  - `DELETE /backups/{filename}`: delete a backup file
  - `GET /backups/download/{filename}`: download a backup file
- **CLI flags**: `--backup-dir` (backup storage directory), `--backup-retention` (keep N most recent per database)
- **ArcSwap database handle**: `DatabaseEntry.db` changed from `Arc<GrafeoDB>` to `ArcSwap<GrafeoDB>` for lock-free hot-swapping during restore. Callers get 503 during restore instead of 404 ([#53](https://github.com/GrafeoDB/grafeo-server/pull/53) by [@Michaelzag](https://github.com/Michaelzag))
- **Sync pull CDC validation**: `POST /sync/{db}/pull` now returns 400 with an explanatory message when CDC is not enabled on the database, instead of silently returning empty results ([#55](https://github.com/GrafeoDB/grafeo-server/pull/55) by [@Michaelzag](https://github.com/Michaelzag))
- **`lpg` feature flag**: new explicit LPG data model feature, forwarded through `grafeo-service` to `grafeo-engine/lpg`. Included in `default` and `full` tiers
- **`triple-store` feature flag**: replaces `rdf` as the canonical name for RDF/triple store support. `rdf` kept as deprecated alias

### Changed

- **grafeo-engine 0.5.35**: section-based container format, `grafeo-storage` crate extraction, incremental backup API (`backup_full()`/`backup_incremental()`/`restore_to_epoch()`), CDC retention and eviction, WAL replay fix, block-based LPG/RDF section formats, persona-based feature profiles
- **`QueryResult.rows` now private**: all access migrated to `.rows()`, `.into_rows()`, and `QueryResult::from_rows()` constructor
- **`#[non_exhaustive]` on all engine enums**: all `match` on `Value`, `EntityId`, `ChangeKind`, `SchemaInfo` now include wildcard arms
- **Feature rename**: `rdf` -> `triple-store` in `grafeo-service` and binary crate features. `sparql` feature now depends on `triple-store` directly
- **`full` tier updated**: includes `lpg` and uses `triple-store` instead of deprecated `rdf` alias

### Fixed (via engine 0.5.35)

- WAL not replayed on reopen: data no longer lost across restarts without explicit checkpoint ([grafeo#252](https://github.com/GrafeoDB/grafeo/issues/252))
- CDC event log unbounded memory: epoch-based and count-based retention with BufferManager eviction ([grafeo#250](https://github.com/GrafeoDB/grafeo/issues/250))
- Graph/schema context validation: reject nonexistent targets, `drop_graph()` auto-clears active context ([grafeo#245](https://github.com/GrafeoDB/grafeo/issues/245))

## [0.5.34] - 2026-04-07

Schema hierarchy, compact endpoint, bulk import, and two engine releases worth of query correctness fixes.

### Added

- **GQL schema namespace endpoints**: `GET /db/{name}/schemas`, `POST /db/{name}/schemas`, `DELETE /db/{name}/schemas/{schema}` for listing, creating, and dropping ISO/IEC 39075 schema namespaces with full data isolation
- **Compact endpoint**: `POST /admin/{db}/compact` converts a live database to a memory-efficient columnar read-only store (`compact-store` feature). One-way operation, requires exclusive access
- **Bulk TSV import**: `POST /db/{name}/import/tsv` for fast edge list ingestion bypassing per-edge transaction overhead (10-100x throughput)
- **`compact-store` feature flag**: forwarded through `grafeo-service` to `grafeo-engine`, included in `full` tier
- **GWP session schema context**: `SessionProperty::Schema` now calls `session.set_schema()`, `ResetTarget::Schema` calls `session.reset_schema()`. GWP catalog operations (`list_schemas`, `create_schema`, `drop_schema`) now delegate to the engine instead of returning a hardcoded single schema

### Changed

- **grafeo-engine 0.5.33**: GraphChallenge algorithms (k-truss, triangle counting, subgraph isomorphism, stochastic block partition), TSV/MMIO bulk import, `RdfGraphStoreAdapter` bridging RDF to LPG algorithms, Kahan compensated summation, WAL `sync_all()` outside lock
- **grafeo-engine 0.5.34**: GQL schema hierarchy (`CREATE SCHEMA`/`DROP SCHEMA`, `SESSION SET SCHEMA`), streaming RDF triple sink (`TripleSink`, `BatchInsertSink`, `CountSink`), deterministic snapshot export, `#[non_exhaustive]` on 13 public enums, `LabelRegistry` combined lock
- **Non-exhaustive enum handling**: all `match` on engine enums (`SchemaInfo`, `ChangeKind`) now include wildcard arms for forward compatibility

### Fixed (via engine 0.5.33, 0.5.34)

- WAL deadlock on property mutations: store mutation applied before WAL logging
- Multi-aggregate extraction: `sum(a) + count(b)` correctly extracts all aggregates
- Mixed `WITH ... WHERE`/HAVING: non-aggregate conjuncts stay as WHERE
- Null property pattern matching: `MATCH (n {key: null})` matches absent and explicitly null
- MERGE null key matching: `MERGE (n:T {a: null, b: 'x'})` correctly finds existing nodes
- `SET += {key: null}` removes property instead of keeping null
- Negative LIMIT clamped to 0 instead of syntax error
- `i64::MIN` literal parsing: `-9223372036854775808` folds at parse time
- NaN/Inf float literals recognized in GQL and Cypher
- `nodes(path)` returns property maps instead of raw IDs
- Cyclic VLP pattern matching: `MATCH p=(s)-[:R*]->(s)` filters via `id()` equality
- VLP default depth raised: unbounded `[*]` expands up to `min_hops + 100`
- WAL sync counter race: `fetch_sub` preserves concurrent increments
- `delete_node_edges` self-loop deduplication via HashSet
- CompactStore multi-table edge types across label pairs
- `round()`/`floor()`/`ceil()` return Float64 instead of truncating to Int64
- `CALL ... YIELD` with aggregation now works over procedure results
- Cypher keyword-as-label: `Order`, `By`, `Skip`, `Limit` usable as node labels
- `CAST(bool AS INT)`: `true` casts to `1`
- List `+` concatenation: `[1, 2] + [3, 4]` returns `[1, 2, 3, 4]`
- Parameter substitution in multi-statement queries
- ORDER BY + LIMIT/SKIP: SKIP and LIMIT apply after ORDER BY
- SET self-reference: `SET n.value = n.value + 1` pre-computes before write
- `size(collect())` nested aggregate no longer panics
- `SUM()` on empty result set returns null per ISO GQL
- `cypher` feature now correctly depends on `gql`

## [0.5.32] - 2026-04-04

Replication correctness: atomic batch replay, read-only replica enforcement, HLC timestamps, Maelstrom and Jepsen testing.

### Changed

- **Atomic sync apply**: `SyncService::apply()` now wraps all mutations in a session transaction for all-or-nothing visibility. Readers on replicas never see partial batch state. Uses new session CRUD methods (`set_node_property`, `delete_node`, etc.) for transactional property updates and deletions
- **Persistent replica epoch**: `ReplicationState::with_persistence(data_dir)` saves per-database epoch to `{data_dir}/.replica-epochs` on each advance. Replicas resume from their last position after restart instead of re-fetching from epoch 0
- **HLC timestamp comparison**: `server_is_newer()` now explicitly converts `u64` client timestamps to `HlcTimestamp` for type-safe comparison, compatible with grafeo-engine 0.5.32's HLC-based CDC timestamps
- **grafeo-engine 0.5.32**: CDC opt-in per session (`set_cdc_enabled`, `session_with_cdc`), Hybrid Logical Clock for causal consistency, `GrafeoDB::compact()` columnar read-only store, session CRUD methods, Gremlin `valueMap()`/`elementMap()`, sibling CALL block scope fix, push-based aggregator hash collision fix, Cypher ORDER BY zero fix
- **CDC auto-activation**: server automatically enables CDC on all databases when running as a replication primary, ensuring all mutations (both direct API and session-driven) produce change events

### Added

- **Replication integration tests**: end-to-end primary-to-replica data flow tests via `SyncService::pull()` + `apply()`, including session-driven mutations (INSERT/SET/DELETE via GQL), epoch persistence roundtrip
- **`ReplicationMode::is_primary()` accessor**: complements `is_replica()` for symmetric replication mode checks
- **Engine-level read-only on replicas**: `QueryService` creates read-only sessions on replicas via `session_read_only()`, preventing mutations through `POST /query` across all transports (HTTP, GWP, BoltR). Previously only PUT/PATCH/DELETE were blocked by middleware
- **`ServiceState::is_query_read_only()`**: combines global read-only flag with replica detection for all transports
- **Maelstrom consistency test node** (`crates/grafeo-maelstrom`): standalone Rust binary implementing Maelstrom's `lin-kv` protocol with primary-replica replication model, write/CAS forwarding, LWW conflict resolution, and HLC timestamps. Run with `maelstrom test -w lin-kv --consistency-models read-uncommitted`
- **Jepsen test harness** (`jepsen/`): Clojure project for Docker-based chaos testing with network partition (docker pause) and crash (docker kill) nemeses, eventual consistency checker

## [0.5.30] - 2026-03-30

### Added

- **W3C SPARQL 1.1 Protocol**: `GET/POST /db/{name}/sparql` with `application/sparql-query`, `application/sparql-update`, form-encoded, and JSON content types, plus Accept-based response negotiation (SPARQL Results JSON)
- **W3C Graph Store HTTP Protocol**: `GET/PUT/POST/DELETE/HEAD /db/{name}/graph-store` with `?default` and `?graph=<iri>` for RESTful named graph CRUD
- **SPARQL Results JSON**: W3C-compliant response format with typed term bindings for SELECT and boolean for ASK
- **Snapshot endpoint**: `POST /admin/{db}/snapshot` writes a point-in-time `.grafeo` snapshot (requires `async-storage` + `grafeo-file` features)
- **`async-storage` feature flag**: forwards engine's async WAL and storage operations, included in `full` tier

### Changed

- **grafeo-engine 0.5.30**: async storage foundation (`async_wal_checkpoint()`, `async_write_snapshot()`), Turtle/N-Quads parser and serializer, declarative `.gtest` spec test framework (2500+ tests), EXISTS in RETURN, GQL aggregate detection in WITH, adjacency list memory optimization (SmallVec to Vec), Gremlin rewrites (`coalesce`, `optional`, `group().by()`, `addE`, `or()`), SQL/PGQ HAVING inline aggregates and zero-length paths, SPARQL dateTime/LANGMATCHES functions, 86 stale spec test skips removed
- **`DatabaseEntry.db` is now `Arc<GrafeoDB>`**: internal refactor enabling direct use of engine async methods; all existing access auto-derefs transparently
- **WAL checkpoint uses native async**: `wal_checkpoint` service method calls `async_wal_checkpoint()` when `async-storage` is enabled, removing manual `spawn_blocking` wrapper

### Fixed (via engine 0.5.28, 0.5.29, 0.5.30)

- Single-file `.grafeo` storage broken in bindings: `grafeo-file` feature was missing from engine defaults (0.5.28)
- Integer arithmetic overflow panic: checked arithmetic now returns NULL on overflow (0.5.29)
- Label intersection across MATCH clauses: `MATCH (n:A) MATCH (n:B)` now filters to both labels (0.5.29)
- CASE WHEN with NULL aggregate returned NULL when WHEN branch was true (0.5.29)
- EXISTS with property filters silently dropped WHERE clause (0.5.29)
- Keywords as property names (`order`, etc.) rejected in property contexts (0.5.29)
- Gremlin `hasLabel` on edges, `coalesce()` semantics, `group().by()` two-pass, `optional()` per-row semantics, `values()` null filtering, `addE` with `as()` labels, `or()` three-valued logic (0.5.29)
- SPARQL functions in SELECT projections, IN/NOT IN operators, BOUND() with OPTIONAL (0.5.29)
- SQL/PGQ unbounded variable-length paths, COUNT(column) NULL skipping, CASE expressions, outer SELECT projection, ORDER BY on aggregate aliases (0.5.29)
- SPARQL dateTime extraction functions (YEAR, MONTH, DAY, etc.) with timezone offsets (0.5.30)
- SPARQL LANGMATCHES() RFC 4647 basic filtering with wildcard support (0.5.30)
- SPARQL LANG() companion columns tracked through triple scans (0.5.30)
- SQL/PGQ parameter references (`$name`, `$min_age`) in WHERE clauses (0.5.30)
- SQL/PGQ HAVING inline aggregates (`HAVING COUNT(*) > 0`) now extracted correctly (0.5.30)
- SQL/PGQ zero-length paths (`*0..N`) now emit source node as 0-hop match (0.5.30)
- Cypher `collect(DISTINCT ...)` through non-aggregate function calls (0.5.30)

## [0.5.27] - 2026-03-27

Starting with this release, grafeo-server uses **lockstep versioning** with the
grafeo engine: server and engine share the same version number. This replaces
the independent 0.4.x series and makes compatibility immediately obvious.

### Changed

- **grafeo-engine 0.5.27**: GQL conformance (ISO/IEC 39075:2024, 234 queries cross-validated), SQL/PGQ GROUP BY/HAVING, post-edge quantifiers, path alternation, vector search filter optimization, adjacency inline capacity raised (SmallVec 4 to 8)

### Fixed (via engine upgrade)

- WAL directory-format data loss on close (#174)
- UNWIND variable silently dropped in SET clause (#172, present since 0.5.14)
- SET n:Label drops variable binding (#178, #182)
- EXISTS/COUNT subquery bugs (target-side correlation, end-node label verification)
- CREATE after MATCH creates phantom nodes (#181)
- `labels(n)`/`type(r)` in aggregation (#187)
- GROUP BY on list-valued keys, ORDER BY complex expressions leaked synthetic columns
- SPARQL GROUP BY/ORDER BY with expressions
- Single-file storage silent failure when WAL disabled (#185)
- Windows read-only file ops failure

## [0.4.9] - 2026-03-25

### Added

- **Offline-first changefeed** (`sync` feature): `GET /db/{name}/changes?since={epoch}&limit={n}` returns a paginated JSON array of CDC events (creates, updates, deletes) for nodes, edges, and RDF triples. Store `server_epoch` from the response and pass it as `since` on the next poll to receive only new changes
- **Offline-first sync apply** (`sync` feature): `POST /db/{name}/sync` accepts a `{ client_id, last_seen_epoch, changes: [...] }` body and applies the changeset with last-write-wins (LWW) conflict resolution. Returns `{ server_epoch, applied, skipped, conflicts, id_mappings }`. The `id_mappings` array maps each create request (by index) to the server-assigned entity ID. Idempotent: replaying the same request twice produces the same state
- **RDF triple events in changefeed**: SPARQL INSERT DATA / DELETE DATA mutations now appear in the changefeed with `entity_type: "triple"` and N-Triples-encoded `triple_subject`, `triple_predicate`, `triple_object`, and `triple_graph` fields (requires `cdc` + `rdf` features in grafeo-engine)
- **SSE push changefeed** (`push-changefeed` feature): `GET /db/{name}/changes/stream` streams CDC events as Server-Sent Events in real time. A `ChangeHub` background task polls each database at 100ms intervals and broadcasts to all connected subscribers. Clients receive a JSON-encoded `ChangeEventDto` in each `data:` field. Compatible with browsers, Dart/Flutter, and any HTTP/1.1 SSE client
- **WebSocket CRDT subscriptions** (`push-changefeed` feature): the existing `/ws` WebSocket endpoint now accepts `Subscribe` and `Unsubscribe` messages. Each subscription is identified by a client-supplied `sub_id` and receives `Change` events as they arrive; multiple databases can be subscribed simultaneously on a single connection
- **Schema version awareness** (`sync` feature): `SyncRequest` now accepts an optional `schema_version` field (FNV-1a hex hash over sorted label names and property keys). When present and mismatched, `SyncResponse` sets `schema_mismatch: true` and always returns `server_schema_version` so clients can detect drift even without sending their own version
- **CRDT counter properties** (`sync` feature): `SyncChangeRequest` accepts a `crdt_op` field (`GrowAdd` for G-Counter, `Increment` for PN-Counter) alongside `crdt_property`. When set, the server merges the counter value instead of applying last-write-wins. CRDT operations are commutative and idempotent across replicas
- **`grafeo-sync` helper crate**: thin async HTTP client wrapping the sync wire protocol. `SyncClient::pull()`, `push()`, and `sync()` handle epoch tracking automatically. Suitable for use in Dart/Flutter, WASM, and Rust-native clients
- **Offline-first guide**: `docs/guides/offline-first.md` covers the full poll-push-sync cycle, wire protocol reference, Dart/Flutter example (local `grafeo_dart` FFI + `SharedPreferences` epoch tracking), WASM/browser example (`localStorage` + Service Worker buffering), conflict handling, and ID mapping patterns
- **Primary-replica replication** (`replication` feature): CDC-based streaming replication for read-scalability
  - `ReplicationMode` enum: `Standalone` (default), `Primary` (read-write, advertises itself), `Replica` (read-only, polls primary)
  - Replica instances reject all write operations (POST/PUT/PATCH/DELETE) with `503 Service Unavailable` and `{"error": "replica_mode"}`
  - Background poll task fetches `GET /db/{name}/changes?since={epoch}&limit=500` on the primary every 500ms and applies events via `SyncService::apply()`; per-database epoch progress tracked in `ReplicationState`
  - `GET /admin/replication`: returns current mode, primary URL (replica mode), and per-database `{ last_applied_epoch, last_error }`
  - Configured via `--replication-mode` (`standalone` | `primary` | `replica`) and `--primary-url` CLI args, or `GRAFEO_REPLICATION_MODE` / `GRAFEO_PRIMARY_URL` env vars

### Fixed

- **Bolt encoding for CRDT counters**: `grafeo-boltr` now encodes `Value::GCounter` and `Value::PnCounter` to `BoltValue::Integer` (resolved totals) so Bolt clients receive a usable scalar rather than a missing match arm compile error

### Changed

- Bumped grafeo-engine and grafeo-common to **0.5.25** (RDF CDC bridge, CDC structural metadata on create events, `Value::GCounter` and `Value::PnCounter` variants)
- `ChangeEventDto` extended with `triple_subject`, `triple_predicate`, `triple_object`, `triple_graph` fields; `entity_type` now also accepts `"triple"`; `#[derive(Clone)]` added for broadcast channel support
- `full` tier now includes `sync`, `push-changefeed`, and `replication` features
- 123 total integration tests (10 new sync/CRDT/SSE/schema tests); 57 grafeo-service unit tests (6 new replication tests); new tests cover edge creates with id_mappings remapping, updates/deletes applied count, LWW conflict detection, limit/pagination, validation error reasons, and replication mode/epoch tracking

## [0.4.8] - 2026-03-24

### Added

- **Engine Prometheus metrics integration**: `/metrics` endpoint now includes per-database engine metrics (when the `metrics` feature is enabled), appended to existing server-level Prometheus output
- **Read-only mode**: `--read-only` / `GRAFEO_READ_ONLY` opens all databases in read-only mode; mutating operations return HTTP 403 with `"read_only"` error code; `ServiceError::ReadOnly` variant added to the service error enum
- **`temporal` feature flag**: forwards the engine's append-only versioned properties feature through the feature chain
- **`jsonl-import` and `parquet-import` feature flags**: forward engine LOAD DATA format support for JSONL and Parquet imports
- **`metrics` feature flag**: forwards engine-level Prometheus metrics collection
- **`import` composite feature**: enables both `jsonl-import` and `parquet-import` in one flag

### Changed

- Bumped grafeo-engine and grafeo-common to **0.5.24**
- `full` tier now includes `temporal`, `import`, and `metrics` features
- `detect_features()` reports `temporal`, `metrics`, `jsonl-import`, and `parquet-import` in the health endpoint when enabled
- 184 total tests (112 integration + 72 workspace unit tests)

## [0.4.7] - 2026-03-14

### Added

- **Memory usage endpoint**: `GET /admin/{db}/memory` returns a hierarchical breakdown of memory usage across store (nodes, edges, properties), indexes (adjacency, label, property, vector, text), MVCC versioning, caches (parsed/optimized plan cache), string pools (label/type registries), and buffer manager regions
- **Named graph endpoints**: manage sub-graphs within a database, distinct from databases themselves
  - `GET /db/{name}/graphs`: list all named graphs in a database
  - `POST /db/{name}/graphs`: create a named graph (`{"name": "..."}`)
  - `DELETE /db/{name}/graphs/{graph}`: drop a named graph
- **`grafeo-file` feature**: forwards the engine's single-file `.grafeo` database format through the feature chain (included in `storage` profile); enables `GrafeoDB::save("mydb.grafeo")` for DuckDB-style single-file databases with sidecar WAL
- **5 new unit tests**: memory usage (default DB, not found), named graph CRUD (list empty, create/drop lifecycle, not found)
- **4 new integration tests**: memory usage endpoint (success, 404), named graph CRUD (full lifecycle, 404)
- **OpenAPI**: memory usage, named graph list/create/drop endpoints documented; `CreateGraphRequest` and `GraphListResponse` schemas added

### Changed

- Bumped grafeo-engine and grafeo-common to **0.5.21** (single-file `.grafeo` format, DDL schema persistence, memory introspection API, named graph WAL/snapshot persistence, LOAD DATA multi-format import, graph type enforcement, query introspection functions, snapshot v3 format, C#/Dart bindings, 1300+ spec tests)
- Engine query compliance fixes (transparent to server): `SUM()` on empty set returns NULL, `CASE WHEN` with NULL correctly falls through, `SESSION SET SCHEMA`/`GRAPH` are independent per ISO/IEC 39075, `COUNT(*)` parsing, `CALL subquery` variable scope, correlated `EXISTS` via semi-join
- `storage` feature group now includes `grafeo-file` alongside `parallel`, `wal`, `spill`, `mmap`
- `detect_features()` reports `grafeo-file` in health endpoint when enabled
- 164 total tests (92 integration + 72 workspace unit tests)

## [0.4.6] - 2026-03-08

### Added

- **Cache stats endpoint**: `GET /admin/{db}/cache` returns query plan cache statistics (parsed/optimized sizes, hit/miss counts, hit rates, invalidation count)
- **Cache clear endpoint**: `POST /admin/{db}/cache/clear` manually clears the query plan cache (forces re-parsing and re-optimization)
- **GQLSTATUS in HTTP responses**: non-success GQLSTATUS codes (per ISO/IEC 39075) included in `QueryResponse` JSON as `gql_status` field; omitted for standard success (`"00000"`)

### Changed

- Bumped grafeo-engine and grafeo-common to **0.5.16** (EXPLAIN/PROFILE statements, LOAD CSV, Cypher DDL, savepoints, correlated EXISTS subqueries, subpath variable binding, temporal map constructors, relationship WHERE clause, plan cache invalidation, SPARQL params fix)
- **ZonedDatetime support**: new `Value::ZonedDatetime` variant encoded across all transports:
  - HTTP: `{"$datetime": "2024-06-15T10:30:00+05:30"}`
  - GWP: native `ZonedDateTime` with date, time, offset components
  - Bolt: native `DateTime` with seconds, nanoseconds, tz_offset_seconds
- **SPARQL regex support**: `sparql` feature now enables `grafeo-engine/regex` for SPARQL FILTER regex functions
- Fixed duplicate `gwp`/`bolt` feature keys in workspace Cargo.toml (merged transport and tier definitions)

### Internal

- `Session::begin_tx()` renamed to `begin_transaction()` (engine API change)
- `QueryResult` gains `gql_status` field (GQLSTATUS code per ISO/IEC 39075)

## [0.4.5] - 2026-02-28

### Added

- **Bolt ROUTE message**: Neo4j driver cluster discovery via ROUTE (0x66) with single-server routing table (WRITE + READ + ROUTE roles). `GrafeoBackend::with_advertise_addr()` configures the address returned in routing responses.
- **Bolt TLS support**: `boltr` crate now supports TLS via `tokio-rustls` (feature: `tls`). `TlsConfig::from_pem()` for certificate loading, `BoltServer::tls()` builder method. `grafeo-boltr` `serve()` reads `tls_cert`/`tls_key` from `BoltrOptions` and wraps connections transparently.
- **Bolt TELEMETRY message**: Bolt 5.4+ driver telemetry (0x54) acknowledged with SUCCESS (no-op).
- **Bolt bookmark utilities**: `extract_bookmarks()` helper in `boltr` to parse bookmarks from Bolt extra dicts. Bookmarks are already passthrough-functional via `begin_transaction`/`execute` extra dicts.
- **2 new Bolt integration tests**: database switching via `db` extra field, multi-language dispatch via `language` extra field (Cypher + SPARQL).

### Changed

- **GWP 0.1.6 migration (breaking)**: migrated from flat Database model to GQL Catalog hierarchy (Schema → Graph → Graph Type). Single `"default"` schema maps Grafeo's database namespace. New catalog methods: `list_schemas`, `list_graphs`, `create_graph`, `drop_graph`, `get_graph_info`, plus graph type stubs. `ResultHeader` now includes `ordered: false`. `TypeDescriptor` gains precision, scale, min/max length, max cardinality, is_group, is_open, duration_qualifier, component_types fields.
- **Multi-language dispatch in GWP**: `GrafeoSession` tracks `language` field; `execute` routes through `QueryService::dispatch()` for language-specific execution; `reset_session` clears language. Same extension mechanism as Bolt (`{"language": "sparql"}`).
- Bumped grafeo-engine and grafeo-common to **0.5.10** (bidirectional BFS, compact properties, skip index, union graph improvements)
- Bumped boltr to **0.1.1** (ROUTE, TELEMETRY, TLS, bookmark utilities, state machine update)
- Bumped gwp to **0.1.6** (GQL Catalog model: schemas, graphs, graph types)
- 96 integration tests (85 default + 11 Bolt), 131 total with workspace unit tests

## [0.4.4] - 2026-02-22

### Added

- **BoltR protocol** (`grafeo-boltr`): Bolt v5.x wire-compatible binary protocol for Neo4j driver connectivity
  - Pure-Rust `boltr` crate: PackStream encoding/decoding, chunked framing, 15 Bolt message types, connection state machine, handshake (v5.1–v5.4), server framework with `BoltBackend` trait, client module for testing
  - `grafeo-boltr` adapter: `GrafeoBackend` implementing `BoltBackend` for grafeo-engine, value encoding (grafeo_common::Value ↔ BoltValue), auth bridging (AuthProvider → AuthValidator)
  - Grafeo extension: multi-language queries via RUN extra dict (`{"language": "sparql"}`)
  - Server info in HELLO response: `{"server": "GrafeoDB/0.4.4"}`
  - `--bolt-port` CLI flag / `GRAFEO_BOLT_PORT` env var (default: 7687)
  - `--bolt-max-sessions` CLI flag / `GRAFEO_BOLT_MAX_SESSIONS` env var (default: 0 = unlimited)
  - Idle session reaping via `--session-ttl` (shared with HTTP/GWP)
  - Auth support: bearer and basic auth via Bolt HELLO/LOGON (feature: `auth`)
  - Shutdown signal integration (graceful Ctrl-C handling)
- **`bolt` feature flag**: enables Bolt v5 transport (`dep:grafeo-boltr`)
- **`boltr` feature tier**: HTTP + Bolt + Studio + all-languages + storage + algos (swaps GWP for Bolt)
- **`QueryService::dispatch()`**: public method for transport crates to use centralized language dispatch on their own engine sessions
- **9 new Bolt integration tests**: connect/authenticate, execute query, transaction commit/rollback, reset from failed, query with parameters, health feature detection, multiple queries per session, server info inspection
- **boltr client module** (feature: `client`): `BoltSession` and `BoltConnection` for integration testing

### Changed

- Bumped grafeo-engine and grafeo-common to 0.5.8
- GWP default port changed from 7687 to **7688** (`GRAFEO_GWP_PORT=7687` restores previous behavior)
- BoltR takes standard Bolt port **7687** (Neo4j driver default)
- GWP added to `default` feature tier (was HTTP + Studio only, now HTTP + GWP + Studio)
- `full` tier now includes all 3 protocols: HTTP + GWP + Bolt
- Renamed `crates/grafeo-bolt/` → `crates/grafeo-boltr/` (package: `grafeo-boltr`)
- `auth` and `tls` features now forward to `grafeo-boltr?/auth` and `grafeo-boltr?/tls`
- Standalone mode supports any combination of GWP/Bolt without HTTP
- 94 integration tests (85 existing + 9 Bolt)

## [0.4.3] - 2026-02-19

### Added

- **Admin endpoints** (HTTP + GWP): database introspection, maintenance, and index management via both REST and gRPC
  - `GET /admin/{db}/stats`: detailed database statistics (node/edge/label/property counts, memory, disk usage)
  - `GET /admin/{db}/wal`: WAL status (enabled, path, size, record count, epoch)
  - `POST /admin/{db}/wal/checkpoint`: force WAL checkpoint (flush pending records to storage)
  - `GET /admin/{db}/validate`: database integrity validation (dangling edges, index consistency)
  - `POST /admin/{db}/index`: create property, vector, or text index
  - `DELETE /admin/{db}/index`: drop property, vector, or text index
- **Search endpoints** (HTTP + GWP): vector, text, and hybrid search via both REST and gRPC
  - `POST /search/vector`: KNN vector similarity search via HNSW index (feature: `vector-index`)
  - `POST /search/text`: full-text BM25 search (feature: `text-index`)
  - `POST /search/hybrid`: combined vector + text search with rank fusion (feature: `hybrid-search`)
- **grafeo-service admin module** (`admin.rs`): transport-agnostic `AdminService` with 6 operations (database_stats, wal_status, wal_checkpoint, validate, create_index, drop_index) and 7 unit tests
- **grafeo-service search module** (`search.rs`): transport-agnostic `SearchService` with 3 operations (vector_search, text_search, hybrid_search); feature-gated with stub fallbacks when features are disabled
- **gwp AdminService** (gRPC): 6 RPCs — `GetDatabaseStats`, `WalStatus`, `WalCheckpoint`, `Validate`, `CreateIndex`, `DropIndex`; registered in `GqlServer::builder()`
- **gwp SearchService** (gRPC): 3 RPCs — `VectorSearch`, `TextSearch`, `HybridSearch` with filter support; registered in `GqlServer::builder()`
- **grafeo-gwp backend**: implemented all 10 admin/search trait methods in `GrafeoBackend`, converting between service types and gwp proto types
- **14 new integration tests**: 10 HTTP (admin stats, not-found, WAL status, WAL checkpoint, validate, create/drop index, stats after insertion, OpenAPI paths, search vector/text error handling) + 4 GWP (admin stats, WAL status, validate, create/drop index via raw tonic proto clients)
- **OpenAPI**: admin and search endpoints fully documented with schemas (`DatabaseStats`, `WalStatusInfo`, `ValidationInfo`, `IndexDef`, `VectorSearchReq`, `TextSearchReq`, `HybridSearchReq`, `SearchHit`, `SearchResponse`) and "Admin" / "Search" tags

### Changed

- Bumped grafeo-engine and grafeo-common to 0.5.7
- Bumped gwp to 0.1.5 (adds AdminService and SearchService gRPC definitions)
- Added `algos` feature flag forwarding for graph algorithm procedures (CALL procedures now require the `algos` feature, enabled by default and in `full` tier)
- 132 total tests (94 integration + 20 grafeo-service unit + 9 grafeo-gwp + 9 grafeo-http)

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

[Unreleased]: https://github.com/GrafeoDB/grafeo-server/compare/v0.5.30...HEAD
[0.5.30]: https://github.com/GrafeoDB/grafeo-server/compare/v0.5.27...v0.5.30
[0.5.27]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.9...v0.5.27
[0.4.9]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.8...v0.4.9
[0.4.8]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.7...v0.4.8
[0.4.7]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.6...v0.4.7
[0.4.6]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/GrafeoDB/grafeo-server/compare/v0.4.2...v0.4.3
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
