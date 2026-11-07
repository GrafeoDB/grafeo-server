//! Refuses writes and WebSocket upgrades that a browser sends on behalf of a
//! page from another site.
//!
//! Browsers apply CORS to reading a response, not to sending a request: a
//! page on any site can POST a form (such as a SPARQL `update=`) to a local
//! server, and can open a WebSocket to it, sending cached Basic credentials
//! along. So every request that may change state (POST, PUT, PATCH, DELETE)
//! and every WebSocket upgrade must come from the server's own origin or from
//! an origin listed in `--cors-origins`.
//!
//! Only browsers send `Origin` and `Sec-Fetch-Site`; a request with neither
//! (curl, drivers, other servers) passes. The decision:
//!
//! 1. `Sec-Fetch-Site: same-origin` or `none` (typed in the address bar)
//!    passes. The browser sets it and a page can't, and it still holds behind
//!    a proxy that rewrites `Host`.
//! 2. Any other `Sec-Fetch-Site` (`same-site`, `cross-site`) passes only for
//!    an `Origin` listed in `--cors-origins` (or `*`).
//! 3. Without `Sec-Fetch-Site` (older browsers), an `Origin` passes when it
//!    is listed or names the request's own `Host`.
//!
//! Reads (GET, HEAD) other than WebSocket upgrades pass: CORS keeps a foreign
//! page from reading their answers. Preflights (OPTIONS) are left to the CORS
//! layer.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{HOST, ORIGIN, UPGRADE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::ErrorBody;

/// Axum middleware applying the cross-site rule to every request.
pub async fn cross_site_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if is_guarded(req.method(), req.headers()) && !is_allowed(req.headers(), state.cors_origins()) {
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            origin = ?req.headers().get(ORIGIN),
            "refused a cross-site request"
        );
        let body = ErrorBody {
            error: "cross_site_request".to_string(),
            detail: Some(
                "cross-site requests may not write or open a WebSocket; \
                 list the page's origin in --cors-origins to allow it"
                    .to_string(),
            ),
        };
        return (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
    }
    next.run(req).await
}

/// Whether the rule applies: a method that may change state, or a
/// WebSocket upgrade.
fn is_guarded(method: &Method, headers: &HeaderMap) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) || headers
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// Whether a guarded request may go on (see the module docs for the rule).
fn is_allowed(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok());
    let listed = origin.is_some_and(|o| allowed_origins.iter().any(|a| a == "*" || a == o));
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some(site)
            if site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none") =>
        {
            true
        }
        Some(_) => listed,
        None => match origin {
            None => true,
            Some(o) => listed || names_host(o, headers.get(HOST).and_then(|v| v.to_str().ok())),
        },
    }
}

/// Whether `origin` (`scheme://host[:port]`) names the authority in `Host`.
fn names_host(origin: &str, host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let Some((_, authority)) = origin.split_once("://") else {
        return false; // "null" and other opaque origins
    };
    !authority.is_empty() && authority.eq_ignore_ascii_case(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn only_state_changing_methods_and_upgrades_are_guarded() {
        let none = HeaderMap::new();
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(is_guarded(&m, &none), "{m}");
        }
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(!is_guarded(&m, &none), "{m}");
        }
        assert!(is_guarded(
            &Method::GET,
            &headers(&[("upgrade", "WebSocket")])
        ));
    }

    #[test]
    fn the_rule() {
        let none: &[String] = &[];
        let listed = ["https://app.example".to_string()];
        let any = ["*".to_string()];
        let host = ("host", "localhost:7474");

        // No browser headers: a non-browser client.
        assert!(is_allowed(&headers(&[host]), none));
        // Fetch metadata decides when present.
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "same-origin"),
                ("origin", "https://proxy.example")
            ]),
            none
        ));
        assert!(is_allowed(
            &headers(&[host, ("sec-fetch-site", "none")]),
            none
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            none
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:5173")
            ]),
            none
        ));
        assert!(!is_allowed(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            &any
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://app.example")
            ]),
            &listed
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            &any
        ));
        // Origin alone: listed or the server's own host.
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://localhost:7474")]),
            none
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://LOCALHOST:7474")]),
            none
        ));
        assert!(!is_allowed(
            &headers(&[host, ("origin", "http://localhost:7475")]),
            none
        ));
        assert!(!is_allowed(&headers(&[host, ("origin", "null")]), none));
        assert!(!is_allowed(
            &headers(&[("origin", "http://localhost:7474")]),
            none
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "https://app.example")]),
            &listed
        ));
    }
}
