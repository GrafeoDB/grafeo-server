//! W3C SPARQL 1.1 Graph Store HTTP Protocol.
//!
//! Implements [Graph Store Protocol](https://www.w3.org/TR/sparql11-http-rdf-update/)
//! for RESTful CRUD on RDF named graphs.
//!
//! All operations are mapped to their SPARQL equivalents and dispatched
//! through the standard query service, so no direct grafeo-core dependency
//! is needed.
//!
//! Endpoints: `GET/PUT/POST/DELETE/HEAD /db/{name}/graph-store`
//!
//! Graph identification via query parameters:
//! - `?default` targets the default graph
//! - `?graph=<iri>` targets a named graph

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use grafeo_service::query::QueryService;

use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

/// Graph identification parameters.
#[derive(serde::Deserialize, Default)]
pub struct GraphStoreParams {
    /// If present (any value or empty), targets the default graph.
    pub default: Option<String>,
    /// IRI of a named graph.
    pub graph: Option<String>,
}

enum GraphTarget {
    Default,
    Named(String),
}

fn resolve_target(params: &GraphStoreParams) -> Result<GraphTarget, ApiError> {
    if params.default.is_some() {
        Ok(GraphTarget::Default)
    } else if let Some(ref iri) = params.graph {
        if iri.is_empty() {
            return Err(ApiError::bad_request("?graph= parameter must not be empty"));
        }
        Ok(GraphTarget::Named(iri.clone()))
    } else {
        Err(ApiError::bad_request(
            "must specify ?default or ?graph=<iri>",
        ))
    }
}

// ---------------------------------------------------------------------------
// Content negotiation
// ---------------------------------------------------------------------------

const CT_TURTLE: &str = "text/turtle";
const CT_NTRIPLES: &str = "application/n-triples";

// ---------------------------------------------------------------------------
// GET /db/{name}/graph-store
// ---------------------------------------------------------------------------

/// Retrieve graph content as N-Triples.
///
/// Uses `CONSTRUCT { ?s ?p ?o } WHERE { ... }` under the hood.
pub async fn graph_store_get(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<GraphStoreParams>,
    auth: AuthContext,
    _headers: HeaderMap,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let target = resolve_target(&params)?;

    let sparql = match target {
        GraphTarget::Default => "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }".to_string(),
        GraphTarget::Named(ref iri) => {
            format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{iri}> {{ ?s ?p ?o }} }}")
        }
    };

    let timeout = state.effective_timeout(None);
    let identity = auth.identity(state.service().is_query_read_only());

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &sparql,
        Some("sparql"),
        None,
        timeout,
        state.service().is_query_read_only(),
        Some(identity),
    )
    .await?;

    // CONSTRUCT results come back as rows with columns [subject, predicate, object].
    // Serialize as N-Triples.
    let mut body = String::new();
    for row in result.rows() {
        if row.len() >= 3 {
            let s = value_to_nt_term(&row[0]);
            let p = value_to_nt_term(&row[1]);
            let o = value_to_nt_term(&row[2]);
            use std::fmt::Write;
            let _ = writeln!(body, "{s} {p} {o} .");
        }
    }

    // Return as N-Triples (most interoperable, no prefix management needed).
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", CT_NTRIPLES)
        .body(axum::body::Body::from(body))
        .expect("valid response"))
}

// ---------------------------------------------------------------------------
// HEAD /db/{name}/graph-store
// ---------------------------------------------------------------------------

/// Check if a graph exists (no body).
pub async fn graph_store_head(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<GraphStoreParams>,
    auth: AuthContext,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let target = resolve_target(&params)?;

    let sparql = match target {
        GraphTarget::Default => "ASK { ?s ?p ?o }".to_string(),
        GraphTarget::Named(ref iri) => {
            format!("ASK {{ GRAPH <{iri}> {{ ?s ?p ?o }} }}")
        }
    };

    let timeout = state.effective_timeout(None);
    let identity = auth.identity(state.service().is_query_read_only());

    // If the query succeeds, the graph exists (or is the default graph).
    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &sparql,
        Some("sparql"),
        None,
        timeout,
        state.service().is_query_read_only(),
        Some(identity),
    )
    .await?;

    // For default graph, always 200. For named, check if ASK returned true.
    match target {
        GraphTarget::Default => Ok(StatusCode::OK.into_response()),
        GraphTarget::Named(_) => {
            // ASK returns a single boolean row. If true, graph has data.
            let has_data = result
                .rows()
                .first()
                .and_then(|row| row.first())
                .and_then(|v| {
                    if let grafeo_common::Value::Bool(b) = *v {
                        Some(b)
                    } else {
                        None
                    }
                })
                .unwrap_or(false);

            if has_data {
                Ok(StatusCode::OK.into_response())
            } else {
                Ok(StatusCode::NOT_FOUND.into_response())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PUT /db/{name}/graph-store
// ---------------------------------------------------------------------------

/// Replace graph content.
///
/// Equivalent to: `DROP SILENT GRAPH <iri>; INSERT DATA { ... }`
pub async fn graph_store_put(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<GraphStoreParams>,
    auth: AuthContext,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let target = resolve_target(&params)?;
    let parsed = parse_body_to_ntriples(&headers, &body)?;

    // Drop existing content.
    let drop_sparql = match target {
        GraphTarget::Default => "DROP SILENT DEFAULT".to_string(),
        GraphTarget::Named(ref iri) => format!("DROP SILENT GRAPH <{iri}>"),
    };

    let timeout = state.effective_timeout(None);
    let read_only = state.service().is_query_read_only();
    let identity = auth.identity(read_only);

    QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &drop_sparql,
        Some("sparql"),
        None,
        timeout,
        read_only,
        Some(identity.clone()),
    )
    .await?;

    // Insert new content.
    if !parsed.lines.is_empty() {
        let insert_sparql = build_insert_data(&target, &parsed);
        QueryService::execute(
            state.databases(),
            state.metrics(),
            &db_name,
            &insert_sparql,
            Some("sparql"),
            None,
            timeout,
            read_only,
            Some(identity),
        )
        .await?;
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// POST /db/{name}/graph-store
// ---------------------------------------------------------------------------

/// Merge triples into a graph.
///
/// Without `?default` or `?graph=`, creates a new named graph and returns
/// `201 Created` with a `Location` header.
pub async fn graph_store_post(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<GraphStoreParams>,
    auth: AuthContext,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let parsed = parse_body_to_ntriples(&headers, &body)?;
    let read_only = state.service().is_query_read_only();
    let identity = auth.identity(read_only);

    // No target params: create a new named graph.
    if params.default.is_none() && params.graph.is_none() {
        let graph_iri = format!("urn:grafeo:graph:{}", uuid::Uuid::new_v4());
        let create_sparql = format!("CREATE GRAPH <{graph_iri}>");
        let timeout = state.effective_timeout(None);

        QueryService::execute(
            state.databases(),
            state.metrics(),
            &db_name,
            &create_sparql,
            Some("sparql"),
            None,
            timeout,
            read_only,
            Some(identity.clone()),
        )
        .await?;

        if !parsed.lines.is_empty() {
            let target = GraphTarget::Named(graph_iri.clone());
            let insert_sparql = build_insert_data(&target, &parsed);
            QueryService::execute(
                state.databases(),
                state.metrics(),
                &db_name,
                &insert_sparql,
                Some("sparql"),
                None,
                timeout,
                read_only,
                Some(identity),
            )
            .await?;
        }

        return Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header(
                "location",
                format!("/api/db/{db_name}/graph-store?graph={graph_iri}"),
            )
            .body(axum::body::Body::empty())
            .expect("valid response"));
    }

    let target = resolve_target(&params)?;

    if !parsed.lines.is_empty() {
        let insert_sparql = build_insert_data(&target, &parsed);
        let timeout = state.effective_timeout(None);
        QueryService::execute(
            state.databases(),
            state.metrics(),
            &db_name,
            &insert_sparql,
            Some("sparql"),
            None,
            timeout,
            read_only,
            Some(identity),
        )
        .await?;
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// DELETE /db/{name}/graph-store
// ---------------------------------------------------------------------------

/// Drop or clear a graph.
pub async fn graph_store_delete(
    State(state): State<AppState>,
    Path(db_name): Path<String>,
    Query(params): Query<GraphStoreParams>,
    auth: AuthContext,
) -> Result<Response, ApiError> {
    auth.check_db_access(&db_name)?;
    let target = resolve_target(&params)?;

    let sparql = match target {
        GraphTarget::Default => "DROP DEFAULT".to_string(),
        GraphTarget::Named(ref iri) => format!("DROP GRAPH <{iri}>"),
    };

    let timeout = state.effective_timeout(None);
    let identity = auth.identity(state.service().is_query_read_only());

    let result = QueryService::execute(
        state.databases(),
        state.metrics(),
        &db_name,
        &sparql,
        Some("sparql"),
        None,
        timeout,
        state.service().is_query_read_only(),
        Some(identity),
    )
    .await;

    match result {
        Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => {
            // If the graph didn't exist, return 404.
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("does not exist") {
                Ok(StatusCode::NOT_FOUND.into_response())
            } else {
                Err(ApiError::from(e))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Body parsing
// ---------------------------------------------------------------------------

/// Parsed RDF request body with prologue declarations separated from data.
struct ParsedBody {
    /// SPARQL PREFIX/BASE declarations extracted from Turtle `@prefix`/`@base`.
    prologue: Vec<String>,
    /// Data lines for inside the INSERT DATA block.
    lines: Vec<String>,
}

/// Validates that a Turtle `@prefix`/`@base` directive body contains only
/// the expected content (prefix name + IRI) and nothing that could inject
/// additional SPARQL statements when embedded in the query prologue.
fn validate_turtle_directive(rest: &str, kind: &str) -> Result<(), ApiError> {
    // The directive body must not contain characters that could break out
    // of the prologue: newlines, semicolons, or braces.
    if rest.contains('\n')
        || rest.contains('\r')
        || rest.contains(';')
        || rest.contains('{')
        || rest.contains('}')
    {
        return Err(ApiError::bad_request(format!(
            "invalid @{kind} directive: contains disallowed characters"
        )));
    }
    // Must contain a valid IRI (enclosed in angle brackets).
    if !rest.contains('<') || !rest.contains('>') {
        return Err(ApiError::bad_request(format!(
            "invalid @{kind} directive: missing IRI"
        )));
    }
    Ok(())
}

/// Parses the request body into data lines and optional prologue declarations.
///
/// Accepts `application/n-triples` and `text/turtle`.
/// For Turtle input, `@prefix`/`@base` directives are extracted, converted to
/// SPARQL `PREFIX`/`BASE` declarations, and returned in `ParsedBody::prologue`
/// so callers can place them in the query prologue (outside INSERT DATA blocks).
fn parse_body_to_ntriples(headers: &HeaderMap, body: &Bytes) -> Result<ParsedBody, ApiError> {
    if body.is_empty() {
        return Ok(ParsedBody {
            prologue: Vec::new(),
            lines: Vec::new(),
        });
    }

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(CT_NTRIPLES);

    let base_ct = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();

    let text = std::str::from_utf8(body)
        .map_err(|_| ApiError::bad_request("request body is not valid UTF-8"))?;

    match base_ct {
        CT_NTRIPLES => {
            // N-Triples: each non-empty, non-comment line is a triple.
            let lines = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect();
            Ok(ParsedBody {
                prologue: Vec::new(),
                lines,
            })
        }
        CT_TURTLE => {
            // Extract @prefix/@base directives and convert to SPARQL syntax.
            // Per SPARQL 1.1 Update, PREFIX/BASE must appear in the query
            // prologue, not inside INSERT DATA blocks.
            let mut prologue = Vec::new();
            let mut lines = Vec::new();
            for line in text.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("@prefix") {
                    // @prefix foo: <uri> . → PREFIX foo: <uri>
                    let rest = rest.trim_end().trim_end_matches('.').trim_end();
                    validate_turtle_directive(rest, "prefix")?;
                    prologue.push(format!("PREFIX{rest}"));
                } else if let Some(rest) = trimmed.strip_prefix("@base") {
                    // @base <uri> . → BASE <uri>
                    let rest = rest.trim_end().trim_end_matches('.').trim_end();
                    validate_turtle_directive(rest, "base")?;
                    prologue.push(format!("BASE{rest}"));
                } else {
                    lines.push(line.to_string());
                }
            }
            Ok(ParsedBody { prologue, lines })
        }
        _ => Err(ApiError::bad_request(format!(
            "unsupported Content-Type: {base_ct}. \
             Expected application/n-triples or text/turtle"
        ))),
    }
}

/// Builds an `INSERT DATA { ... }` SPARQL statement from parsed body.
///
/// Any prologue declarations (PREFIX/BASE) are prepended before the INSERT DATA.
fn build_insert_data(target: &GraphTarget, parsed: &ParsedBody) -> String {
    let triples_block = parsed.lines.join("\n");
    let prologue = if parsed.prologue.is_empty() {
        String::new()
    } else {
        format!("{}\n", parsed.prologue.join("\n"))
    };
    match target {
        GraphTarget::Default => format!("{prologue}INSERT DATA {{\n{triples_block}\n}}"),
        GraphTarget::Named(iri) => {
            format!("{prologue}INSERT DATA {{ GRAPH <{iri}> {{\n{triples_block}\n}} }}")
        }
    }
}

/// Converts a Grafeo `Value` to an N-Triples term string.
#[allow(clippy::match_same_arms)]
pub(crate) fn value_to_nt_term(value: &grafeo_common::Value) -> String {
    use grafeo_common::Value;
    match value {
        Value::String(s) => {
            let s_str = s.to_string();
            if s_str.starts_with("http://")
                || s_str.starts_with("https://")
                || s_str.starts_with("urn:")
            {
                format!("<{s_str}>")
            } else if s_str.starts_with("_:") {
                s_str
            } else {
                // Escape the string for N-Triples.
                let escaped = s_str
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t");
                format!("\"{escaped}\"")
            }
        }
        Value::Int64(i) => {
            format!("\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
        }
        Value::Float64(f) => {
            format!("\"{f}\"^^<http://www.w3.org/2001/XMLSchema#double>")
        }
        Value::Bool(b) => {
            format!("\"{b}\"^^<http://www.w3.org/2001/XMLSchema#boolean>")
        }
        _ => {
            let s = format!("{value:?}");
            let escaped = s.replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::AppState;

    fn app() -> axum::Router {
        let state = AppState::new_in_memory(300);
        crate::router(state)
    }

    /// SPARQL may not be enabled in per-crate tests; accept both 200-level and
    /// 400 (feature-disabled) as valid HTTP-layer results.
    fn is_ok_or_sparql_disabled(status: StatusCode) -> bool {
        status.is_success()
            || status == StatusCode::NO_CONTENT
            || status == StatusCode::CREATED
            || status == StatusCode::BAD_REQUEST
    }

    // -----------------------------------------------------------------------
    // resolve_target — parameter validation (pure HTTP layer)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn missing_graph_params_returns_400() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/graph-store")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn empty_graph_iri_returns_400() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/graph-store?graph=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // GET /db/{name}/graph-store
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_default_graph() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/graph-store?default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            is_ok_or_sparql_disabled(resp.status()),
            "unexpected: {}",
            resp.status()
        );
        if resp.status() == StatusCode::OK {
            let ct = resp
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(ct.contains("n-triples"), "ct: {ct}");
        }
    }

    #[tokio::test]
    async fn get_named_graph() {
        let resp = app()
            .oneshot(
                Request::get("/db/default/graph-store?graph=http://example.org/g1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(is_ok_or_sparql_disabled(resp.status()));
    }

    #[tokio::test]
    async fn get_nonexistent_database() {
        let resp = app()
            .oneshot(
                Request::get("/db/nonexistent/graph-store?default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // HEAD /db/{name}/graph-store
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn head_default_graph() {
        let resp = app()
            .oneshot(
                Request::head("/db/default/graph-store?default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(is_ok_or_sparql_disabled(resp.status()));
    }

    #[tokio::test]
    async fn head_missing_params() {
        let resp = app()
            .oneshot(
                Request::head("/db/default/graph-store")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // PUT /db/{name}/graph-store
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_default_graph_empty_body() {
        let resp = app()
            .oneshot(
                Request::put("/db/default/graph-store?default")
                    .header("content-type", "application/n-triples")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(is_ok_or_sparql_disabled(resp.status()));
    }

    #[tokio::test]
    async fn put_missing_params() {
        let resp = app()
            .oneshot(
                Request::put("/db/default/graph-store")
                    .header("content-type", "application/n-triples")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_unsupported_content_type() {
        let resp = app()
            .oneshot(
                Request::put("/db/default/graph-store?default")
                    .header("content-type", "application/rdf+xml")
                    .body(Body::from("<rdf:RDF/>"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // POST /db/{name}/graph-store
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_creates_new_graph_without_params() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/graph-store")
                    .header("content-type", "application/n-triples")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Without ?default or ?graph=, POST creates a new graph (201) or
        // 400 if sparql is disabled.
        assert!(
            resp.status() == StatusCode::CREATED || resp.status() == StatusCode::BAD_REQUEST,
            "unexpected: {}",
            resp.status()
        );
        if resp.status() == StatusCode::CREATED {
            assert!(resp.headers().contains_key("location"));
        }
    }

    #[tokio::test]
    async fn post_merge_into_default_graph() {
        let resp = app()
            .oneshot(
                Request::post("/db/default/graph-store?default")
                    .header("content-type", "application/n-triples")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(is_ok_or_sparql_disabled(resp.status()));
    }

    // -----------------------------------------------------------------------
    // DELETE /db/{name}/graph-store
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_missing_params() {
        let resp = app()
            .oneshot(
                Request::delete("/db/default/graph-store")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_default_graph() {
        let resp = app()
            .oneshot(
                Request::delete("/db/default/graph-store?default")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(is_ok_or_sparql_disabled(resp.status()));
    }

    // -----------------------------------------------------------------------
    // parse_body_to_ntriples (pure function tests)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_empty_body() {
        let headers = HeaderMap::new();
        let body = Bytes::new();
        let result = parse_body_to_ntriples(&headers, &body).unwrap();
        assert!(result.lines.is_empty());
        assert!(result.prologue.is_empty());
    }

    #[test]
    fn parse_ntriples_body() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/n-triples".parse().unwrap());
        let body = Bytes::from(
            "<http://ex.org/s> <http://ex.org/p> \"hello\" .\n\
             # comment line\n\
             <http://ex.org/s> <http://ex.org/p2> \"world\" .\n",
        );
        let result = parse_body_to_ntriples(&headers, &body).unwrap();
        assert_eq!(result.lines.len(), 2);
        assert!(result.lines[0].contains("<http://ex.org/s>"));
        assert!(result.prologue.is_empty());
    }

    #[test]
    fn parse_turtle_extracts_prefixes() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/turtle".parse().unwrap());
        let body = Bytes::from(
            "@prefix ex: <http://example.org/> .\n\
             @base <http://example.org/base/> .\n\
             ex:s ex:p ex:o .\n",
        );
        let parsed = parse_body_to_ntriples(&headers, &body).unwrap();
        assert_eq!(parsed.prologue.len(), 2);
        assert_eq!(parsed.prologue[0], "PREFIX ex: <http://example.org/>");
        assert_eq!(parsed.prologue[1], "BASE <http://example.org/base/>");
        assert_eq!(parsed.lines.len(), 1);
        assert!(parsed.lines[0].contains("ex:s"));
    }

    #[test]
    fn parse_unsupported_content_type() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/rdf+xml".parse().unwrap());
        let body = Bytes::from("<rdf:RDF/>");
        let err = parse_body_to_ntriples(&headers, &body);
        assert!(err.is_err());
    }

    // -----------------------------------------------------------------------
    // build_insert_data (pure function tests)
    // -----------------------------------------------------------------------

    #[test]
    fn build_insert_data_default_graph() {
        let parsed = ParsedBody {
            prologue: vec![],
            lines: vec!["<http://ex.org/s> <http://ex.org/p> \"val\" .".to_string()],
        };
        let sparql = build_insert_data(&GraphTarget::Default, &parsed);
        assert!(sparql.starts_with("INSERT DATA"));
        assert!(sparql.contains("<http://ex.org/s>"));
        assert!(!sparql.contains("GRAPH"));
    }

    #[test]
    fn build_insert_data_named_graph() {
        let parsed = ParsedBody {
            prologue: vec![],
            lines: vec!["<http://ex.org/s> <http://ex.org/p> \"val\" .".to_string()],
        };
        let sparql = build_insert_data(&GraphTarget::Named("http://ex.org/g1".into()), &parsed);
        assert!(sparql.contains("GRAPH <http://ex.org/g1>"));
    }

    #[test]
    fn build_insert_data_with_prologue() {
        let parsed = ParsedBody {
            prologue: vec!["PREFIX ex: <http://example.org/>".to_string()],
            lines: vec!["ex:s ex:p ex:o .".to_string()],
        };
        let sparql = build_insert_data(&GraphTarget::Default, &parsed);
        assert!(sparql.starts_with("PREFIX ex:"));
        assert!(sparql.contains("INSERT DATA"));
        assert!(sparql.contains("ex:s ex:p ex:o"));
    }

    // -----------------------------------------------------------------------
    // value_to_nt_term (pure function tests)
    // -----------------------------------------------------------------------

    #[test]
    fn nt_term_iri() {
        let v = grafeo_common::Value::String("http://example.org/s".into());
        assert_eq!(value_to_nt_term(&v), "<http://example.org/s>");
    }

    #[test]
    fn nt_term_blank_node() {
        let v = grafeo_common::Value::String("_:b0".into());
        assert_eq!(value_to_nt_term(&v), "_:b0");
    }

    #[test]
    fn nt_term_plain_string() {
        let v = grafeo_common::Value::String("hello world".into());
        assert_eq!(value_to_nt_term(&v), "\"hello world\"");
    }

    #[test]
    fn nt_term_string_with_quotes() {
        let v = grafeo_common::Value::String("say \"hi\"".into());
        assert_eq!(value_to_nt_term(&v), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn nt_term_integer() {
        let v = grafeo_common::Value::Int64(42);
        assert_eq!(
            value_to_nt_term(&v),
            "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        );
    }

    #[test]
    fn nt_term_float() {
        let v = grafeo_common::Value::Float64(1.5);
        assert_eq!(
            value_to_nt_term(&v),
            "\"1.5\"^^<http://www.w3.org/2001/XMLSchema#double>"
        );
    }

    #[test]
    fn nt_term_boolean() {
        let v = grafeo_common::Value::Bool(true);
        assert_eq!(
            value_to_nt_term(&v),
            "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"
        );
    }
}
