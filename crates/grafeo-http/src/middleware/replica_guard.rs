//! Middleware that rejects write operations when the server is in replica mode.
//!
//! On a replica, PUT, PATCH and DELETE requests, and POST requests to routes
//! that write data or storage, return `503 Service Unavailable` with
//! `{"error": "replica_mode", "message": "..."}`: a local write would make
//! the replica diverge from its primary and shift the IDs that replicated
//! changes refer to. GET and HEAD requests are always allowed, and so are the
//! POST routes [`post_allowed_on_replica`] lists.

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
        Method::PUT | Method::PATCH | Method::DELETE => true,
        Method::POST => !post_allowed_on_replica(path),
        _ => false,
    }
}

/// The POST routes a replica serves. Any other POST is treated as a write,
/// including a route added later until it is listed here.
///
/// - queries, batches and explicit transactions: the engine's read-only
///   session flag rejects the writes among them (`/db/{name}/sparql` is the
///   SPARQL Protocol form of `/sparql`);
/// - search;
/// - `/db/{name}/sync`: replication itself;
/// - admin operations that keep or inspect the current state without
///   changing it: WAL checkpoint, snapshot, backups, reloading spilled
///   sections, clearing the plan cache, SHACL validation;
/// - token management: server-local credentials, not replicated data.
fn post_allowed_on_replica(path: &str) -> bool {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    matches!(
        segments.as_slice(),
        ["query" | "cypher" | "graphql" | "gremlin" | "sparql" | "sql" | "batch"]
            | ["tx", "begin" | "query" | "commit" | "rollback"]
            | ["search", "vector" | "text" | "hybrid"]
            | ["db", _, "sparql" | "sync"]
            | ["admin", _, "wal", "checkpoint"]
            | ["admin", _, "snapshot" | "backup" | "reload-eligible"]
            | ["admin", _, "backup", "incremental"]
            | ["admin", _, "cache", "clear"]
            | ["admin", _, "validate", "shacl"]
            | ["admin", "tokens"]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every POST route of the router, and whether a replica serves it.
    const POST_ROUTES: &[(&str, bool)] = &[
        ("/query", true),
        ("/cypher", true),
        ("/graphql", true),
        ("/gremlin", true),
        ("/sparql", true),
        ("/sql", true),
        ("/batch", true),
        ("/tx/begin", true),
        ("/tx/query", true),
        ("/tx/commit", true),
        ("/tx/rollback", true),
        ("/db", false),
        ("/db/default/graphs", false),
        ("/db/default/schemas", false),
        ("/db/default/import/tsv", false),
        ("/db/default/upsert/nodes", false),
        ("/db/default/upsert/edges", false),
        ("/db/default/sparql", true),
        ("/db/default/graph-store", false),
        ("/db/default/sync", true),
        ("/admin/default/wal/checkpoint", true),
        ("/admin/default/index", false),
        ("/admin/default/cache/clear", true),
        ("/admin/default/reload-eligible", true),
        ("/admin/default/snapshot", true),
        ("/admin/default/compact", false),
        ("/admin/default/projections", false),
        ("/admin/default/validate/shacl", true),
        ("/admin/default/backup", true),
        ("/admin/default/backup/incremental", true),
        ("/admin/default/restore", false),
        ("/admin/default/restore/epoch", false),
        ("/search/vector", true),
        ("/search/text", true),
        ("/search/hybrid", true),
        ("/admin/tokens", true),
    ];

    #[test]
    fn post_routes_are_classified() {
        for &(path, allowed) in POST_ROUTES {
            assert_eq!(
                rejected_on_replica(&Method::POST, path),
                !allowed,
                "POST {path}"
            );
        }
    }

    #[test]
    fn reads_pass_and_other_writes_are_rejected() {
        for path in ["/db/default", "/db/default/changes", "/health"] {
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
    }
}
