//! Middleware that rejects write operations when the server is in replica mode.
//!
//! On a replica, PUT, PATCH, DELETE and POST requests that write data or
//! storage return `503 Service Unavailable` with
//! `{"error": "replica_mode", "message": "..."}`: a local write would make
//! the replica diverge from its primary and shift the IDs that replicated
//! changes refer to. GET and HEAD requests are always allowed (the
//! changefeed `GET /db/{name}/changes` and its stream among them), and so are
//! the requests `write_allowed_on_replica` lists.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::json;

use crate::AppState;

/// Axum middleware that rejects write requests on replicas.
///
/// Applied only when the `replication` feature is enabled. On non-replica
/// instances (Standalone, Primary) this is a zero-cost pass-through.
pub async fn replica_guard_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if state.service().is_replica() && rejected_on_replica(req.method(), req.uri().path()) {
        let body = json!({
            "error": "replica_mode",
            "message": "This instance is a read-only replica. Write operations are not permitted."
        });
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("response builder with valid header is infallible");
    }

    next.run(req).await
}

/// Whether a replica rejects a `method` request to `path`.
fn rejected_on_replica(method: &Method, path: &str) -> bool {
    match *method {
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => {
            !write_allowed_on_replica(method, path)
        }
        _ => false,
    }
}

/// The POST, PUT, PATCH and DELETE requests a replica serves. Any other is
/// treated as a write, including a route added later until it is listed
/// here.
///
/// - queries, batches and explicit transactions (POST): the engine's
///   read-only session flag rejects the writes among them
///   (`/db/{name}/sparql` is the SPARQL Protocol form of `/sparql`);
/// - search (POST);
/// - admin operations that keep or inspect the current state without
///   changing it (POST): WAL checkpoint, snapshot, backups, reloading
///   spilled sections, clearing the plan cache, SHACL validation;
/// - token management (`POST /admin/tokens`, `DELETE /admin/tokens/{id}`),
///   on builds with `auth` only: tokens live in the instance's own token
///   store, not in replicated data. Without `auth` these routes do not
///   exist, and `DELETE /admin/tokens/index` is the index drop of a
///   database named `tokens`.
///
/// `POST /db/{name}/sync` is not listed: on a replica it would be a client
/// write. The replica applies its primary's changes through
/// `SyncService::apply` directly, not over HTTP.
fn write_allowed_on_replica(method: &Method, path: &str) -> bool {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let tokens = cfg!(feature = "auth");
    match *method {
        Method::POST => match segments.as_slice() {
            ["admin", "tokens"] => tokens,
            other => matches!(
                other,
                ["query" | "cypher" | "graphql" | "gremlin" | "sparql" | "sql" | "batch"]
                    | ["tx", "begin" | "query" | "commit" | "rollback"]
                    | ["search", "vector" | "text" | "hybrid"]
                    | ["db", _, "sparql"]
                    | ["admin", _, "wal", "checkpoint"]
                    | ["admin", _, "snapshot" | "backup" | "reload-eligible"]
                    | ["admin", _, "backup", "incremental"]
                    | ["admin", _, "cache", "clear"]
                    | ["admin", _, "validate", "shacl"]
            ),
        },
        Method::DELETE => tokens && matches!(segments.as_slice(), ["admin", "tokens", _]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every write route of the router (method, path), and whether a
    /// replica serves it.
    const WRITE_ROUTES: &[(&str, &str, bool)] = &[
        ("POST", "/query", true),
        ("POST", "/cypher", true),
        ("POST", "/graphql", true),
        ("POST", "/gremlin", true),
        ("POST", "/sparql", true),
        ("POST", "/sql", true),
        ("POST", "/batch", true),
        ("POST", "/tx/begin", true),
        ("POST", "/tx/query", true),
        ("POST", "/tx/commit", true),
        ("POST", "/tx/rollback", true),
        ("POST", "/db", false),
        ("DELETE", "/db/default", false),
        ("POST", "/db/default/graphs", false),
        ("DELETE", "/db/default/graphs/g2", false),
        ("POST", "/db/default/schemas", false),
        ("DELETE", "/db/default/schemas/s1", false),
        ("POST", "/db/default/import/tsv", false),
        ("POST", "/db/default/upsert/nodes", false),
        ("POST", "/db/default/upsert/edges", false),
        ("POST", "/db/default/sparql", true),
        ("PUT", "/db/default/graph-store", false),
        ("POST", "/db/default/graph-store", false),
        ("DELETE", "/db/default/graph-store", false),
        ("POST", "/db/default/sync", false),
        ("POST", "/admin/default/wal/checkpoint", true),
        ("POST", "/admin/default/index", false),
        ("DELETE", "/admin/default/index", false),
        ("POST", "/admin/default/cache/clear", true),
        ("POST", "/admin/default/reload-eligible", true),
        ("POST", "/admin/default/snapshot", true),
        ("POST", "/admin/default/compact", false),
        ("POST", "/admin/default/projections", false),
        ("DELETE", "/admin/default/projections/p1", false),
        ("POST", "/admin/default/validate/shacl", true),
        ("POST", "/admin/default/backup", true),
        ("POST", "/admin/default/backup/incremental", true),
        ("POST", "/admin/default/restore", false),
        ("POST", "/admin/default/restore/epoch", false),
        ("DELETE", "/admin/default/backups/b1.grafeo", false),
        ("POST", "/search/vector", true),
        ("POST", "/search/text", true),
        ("POST", "/search/hybrid", true),
        // Token routes exist only with `auth`. Without it, the DELETE is the
        // index drop of a database named `tokens`.
        ("POST", "/admin/tokens", cfg!(feature = "auth")),
        ("DELETE", "/admin/tokens/tok-1", cfg!(feature = "auth")),
        ("DELETE", "/admin/tokens/index", cfg!(feature = "auth")),
    ];

    #[test]
    fn write_routes_are_classified() {
        for &(method, path, allowed) in WRITE_ROUTES {
            let method = Method::from_bytes(method.as_bytes()).unwrap();
            assert_eq!(
                rejected_on_replica(&method, path),
                !allowed,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn reads_pass_and_other_writes_are_rejected() {
        for path in [
            "/db/default",
            "/db/default/changes",
            "/db/default/changes/stream",
            "/health",
        ] {
            assert!(!rejected_on_replica(&Method::GET, path), "GET {path}");
            assert!(!rejected_on_replica(&Method::HEAD, path), "HEAD {path}");
        }
        // Whatever the path ends with: a database or graph may be named so.
        assert!(rejected_on_replica(&Method::DELETE, "/db/sync"));
        assert!(rejected_on_replica(
            &Method::DELETE,
            "/db/default/graphs/changes"
        ));
        assert!(rejected_on_replica(&Method::PUT, "/db/default/graph-store"));
        assert!(rejected_on_replica(&Method::PATCH, "/db/default"));
    }

    #[test]
    fn names_that_match_a_route_do_not_open_a_write() {
        assert!(rejected_on_replica(&Method::POST, "/db/sync/graphs"));
        assert!(rejected_on_replica(&Method::POST, "/db/query/upsert/nodes"));
        assert!(rejected_on_replica(&Method::POST, "/admin/tokens/restore"));
        assert!(rejected_on_replica(&Method::POST, "/not/a/route"));
        assert!(rejected_on_replica(&Method::DELETE, "/admin/tokens"));
        assert!(rejected_on_replica(&Method::PUT, "/admin/tokens/tok-1"));
        assert!(rejected_on_replica(
            &Method::DELETE,
            "/admin/tokens/projections/p1"
        ));
    }
}
