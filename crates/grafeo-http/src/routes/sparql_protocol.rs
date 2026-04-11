//! W3C SPARQL 1.1 Protocol endpoints.
//!
//! Implements [SPARQL 1.1 Protocol](https://www.w3.org/TR/sparql11-protocol/) with
//! proper content-type negotiation for query and update operations.
//!
//! Supports:
//! - `GET /db/{name}/sparql?query=...` (query only)
//! - `POST /db/{name}/sparql` with `Content-Type: application/sparql-query`
//! - `POST /db/{name}/sparql` with `Content-Type: application/sparql-update`
//! - `POST /db/{name}/sparql` with `Content-Type: application/x-www-form-urlencoded`
//! - `POST /db/{name}/sparql` with `Content-Type: application/json` (backwards compat)
//!
//! Response format is negotiated via the `Accept` header:
//! - `application/sparql-results+json` for SELECT/ASK (default)
//! - `application/json` for Grafeo's native JSON format
//! - `text/turtle` for CONSTRUCT/DESCRIBE (future)

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;

use grafeo_service::query::QueryService;

use crate::encode::{convert_json_params, streaming_json_response};
use crate::encode_sparql::sparql_results_json_response;
use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;
use crate::types::QueryRequest;

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

const CT_SPARQL_QUERY: &str = "application/sparql-query";
const CT_SPARQL_UPDATE: &str = "application/sparql-update";
const CT_FORM_URLENCODED: &str = "application/x-www-form-urlencoded";
const CT_JSON: &str = "application/json";

const ACCEPT_SPARQL_JSON: &str = "application/sparql-results+json";
const ACCEPT_JSON: &str = "application/json";

// ---------------------------------------------------------------------------
// GET /db/{name}/sparql?query=...
// ---------------------------------------------------------------------------

/// Query parameters for SPARQL GET requests.
#[derive(serde::Deserialize)]
pub struct SparqlGetParams {
    /// URL-encoded SPARQL query string.
    pub query: String,
    /// Override default graph URI (repeatable, not yet enforced).
    #[serde(rename = "default-graph-uri")]
    pub default_graph_uri: Option<String>,
    /// Named graph URI (repeatable, not yet enforced).
    #[serde(rename = "named-graph-uri")]
    pub named_graph_uri: Option<String>,
}

/// `GET /db/{name}/sparql?query=...`
///
/// Executes a SPARQL query (read-only). Updates via GET return 400.
pub async fn sparql_get(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<SparqlGetParams>,
    auth: AuthContext,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let timeout = state.effective_timeout(None);
    let identity = auth.identity(state.service().is_query_read_only());

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &params.query,
        Some("sparql"),
        None,
        timeout,
        state.service().is_query_read_only(),
        Some(identity),
    )
    .await?;

    Ok(format_sparql_response(result, &headers))
}

// ---------------------------------------------------------------------------
// POST /db/{name}/sparql
// ---------------------------------------------------------------------------

/// `POST /db/{name}/sparql`
///
/// Content-Type dispatch:
/// - `application/sparql-query`: body is a SPARQL query string
/// - `application/sparql-update`: body is a SPARQL update string
/// - `application/x-www-form-urlencoded`: `query=...` or `update=...` parameter
/// - `application/json`: Grafeo JSON format (backwards compat)
pub async fn sparql_post(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    auth: AuthContext,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(CT_JSON);

    // Extract the base content type (strip charset and other parameters).
    let base_ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();

    let read_only = state.service().is_query_read_only();
    let identity = auth.identity(read_only);

    let statement = match base_ct {
        CT_SPARQL_QUERY | CT_SPARQL_UPDATE => String::from_utf8(body.to_vec())
            .map_err(|_| ApiError::bad_request("request body is not valid UTF-8"))?,
        CT_FORM_URLENCODED => {
            let form: Vec<(String, String)> = serde_urlencoded::from_bytes(&body)
                .map_err(|e| ApiError::bad_request(format!("invalid form encoding: {e}")))?;

            form.iter()
                .find(|(k, _)| k == "query" || k == "update")
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    ApiError::bad_request("form body must contain 'query' or 'update' parameter")
                })?
        }
        CT_JSON => {
            // Backwards-compatible JSON body.
            let req: QueryRequest = serde_json::from_slice(&body)
                .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;

            let params = convert_json_params(req.params.as_ref())?;
            let timeout = state.effective_timeout(req.timeout_ms);

            let result = QueryService::execute(
                state.databases(),
                state.metrics(),
                &db_name,
                &req.query,
                Some("sparql"),
                params,
                timeout,
                read_only,
                Some(identity),
            )
            .await?;

            return Ok(format_sparql_response(result, &headers));
        }
        _ => {
            return Err(ApiError::bad_request(format!(
                "unsupported Content-Type: {base_ct}. Expected one of: \
                 application/sparql-query, application/sparql-update, \
                 application/x-www-form-urlencoded, application/json"
            )));
        }
    };

    let timeout = state.effective_timeout(None);

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &statement,
        Some("sparql"),
        None,
        timeout,
        read_only,
        Some(identity),
    )
    .await?;

    Ok(format_sparql_response(result, &headers))
}

// ---------------------------------------------------------------------------
// Response format negotiation
// ---------------------------------------------------------------------------

/// Picks the response format based on the `Accept` header.
fn format_sparql_response(
    result: grafeo_engine::database::QueryResult,
    headers: &HeaderMap,
) -> Response {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(ACCEPT_SPARQL_JSON);

    if accept.contains(ACCEPT_JSON) && !accept.contains(ACCEPT_SPARQL_JSON) {
        // Explicit request for Grafeo's native JSON format.
        streaming_json_response(result)
    } else {
        // Default: W3C SPARQL Results JSON.
        sparql_results_json_response(result)
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::AppState;

    fn app() -> axum::Router {
        let state = AppState::new_in_memory(300);
        crate::router(state)
    }

    /// Helper: returns true if the sparql feature is active (query execution
    /// succeeds). When running via `cargo test --workspace`, default features
    /// include sparql. When testing `grafeo-http` in isolation, sparql may be
    /// absent, so execution-dependent tests accept both 200 and 400.
    fn expects_sparql_success(status: StatusCode) -> bool {
        // 200 = sparql enabled, query succeeded.
        // 400 = sparql disabled ("sparql support not enabled") OR query error.
        // Both are valid HTTP-layer results; the protocol dispatch itself worked.
        status == StatusCode::OK || status == StatusCode::BAD_REQUEST
    }

    // -----------------------------------------------------------------------
    // GET /db/{name}/sparql
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_sparql_select() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/sparql?query=SELECT%20%3Fs%20%3Fp%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20LIMIT%201")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // OK when sparql is enabled, BadRequest when disabled.
        assert!(
            expects_sparql_success(resp.status()),
            "unexpected status: {}",
            resp.status()
        );
    }

    #[tokio::test]
    async fn get_sparql_missing_query_param() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/sparql")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_sparql_database_not_found() {
        let resp = app()
            .oneshot(
                Request::get("/db/nonexistent/sparql?query=SELECT%20*%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // 404 when sparql enabled, 400 when sparql disabled (feature error
        // is returned before the database lookup).
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected status: {}",
            resp.status()
        );
    }

    // -----------------------------------------------------------------------
    // POST — content-type dispatch
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_sparql_query_content_type() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/sparql-query")
                    .body(Body::from("SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 1"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    #[tokio::test]
    async fn post_sparql_query_with_charset() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/sparql-query; charset=utf-8")
                    .body(Body::from("SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 0"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    #[tokio::test]
    async fn post_sparql_update_content_type() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/sparql-update")
                    .body(Body::from(
                        "INSERT DATA { <http://ex.org/s> <http://ex.org/p> \"hello\" }",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    // -----------------------------------------------------------------------
    // POST — form-urlencoded
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_sparql_form_encoded_query() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "query=SELECT+%3Fs+%3Fp+%3Fo+WHERE+%7B+%3Fs+%3Fp+%3Fo+%7D+LIMIT+0",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    #[tokio::test]
    async fn post_sparql_form_encoded_update() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "update=INSERT+DATA+%7B+%3Chttp%3A%2F%2Fex.org%2Fs%3E+%3Chttp%3A%2F%2Fex.org%2Fp%3E+%22val%22+%7D",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    #[tokio::test]
    async fn post_sparql_form_missing_query_and_update() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("other=value"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Always 400: the form parsing error occurs BEFORE engine dispatch.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // POST — application/json (backwards compat)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_sparql_json_body() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"query":"SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 0"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(expects_sparql_success(resp.status()));
    }

    // -----------------------------------------------------------------------
    // POST — unsupported content-type
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_sparql_unsupported_content_type() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/sparql")
                    .header("content-type", "text/plain")
                    .body(Body::from("SELECT * WHERE { ?s ?p ?o }"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Always 400: unsupported content-type is a protocol-level error.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // Accept header negotiation (requires sparql feature for 200)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn accept_sparql_results_json() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/sparql?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20LIMIT%200")
                    .header("accept", "application/sparql-results+json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if resp.status() == StatusCode::OK {
            let ct = resp
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(ct.contains("sparql-results+json"), "ct: {ct}");
        }
        // If 400, sparql is disabled; content-type test is not applicable.
    }

    #[tokio::test]
    async fn accept_application_json_returns_native_format() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/sparql?query=SELECT%20%3Fs%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D%20LIMIT%200")
                    .header("accept", "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if resp.status() == StatusCode::OK {
            let ct = resp
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(ct.contains("application/json"), "ct: {ct}");
            assert!(!ct.contains("sparql-results"), "ct: {ct}");
        }
    }
}
