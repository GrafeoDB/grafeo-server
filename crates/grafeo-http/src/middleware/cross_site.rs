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
//!    passes only when the `Host` is a trusted name for this server
//!    (`--trusted-hosts`, the `--host` value and loopback by default). The
//!    browser sets the header and a page can't, but a DNS-rebinding page
//!    sends `same-origin` with its own name in `Host`, so the header alone
//!    is not enough.
//! 2. Any other `Sec-Fetch-Site` (`same-site`, `cross-site`) passes only for
//!    an `Origin` listed in `--cors-origins` (or `*`).
//! 3. Without `Sec-Fetch-Site` (older browsers), an `Origin` passes when it
//!    is listed or names the request's own `Host` on the effective scheme.
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
    if is_guarded(req.method(), req.headers())
        && !is_allowed(
            req.headers(),
            state.cors_origins(),
            state.trusted_hosts(),
            state.scheme(),
        )
    {
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            origin = ?req.headers().get(ORIGIN),
            "refused a cross-site request"
        );
        state.audit_event(
            grafeo_service::audit::AuditEvent::new("cross_site_refused")
                .transport("http")
                .source_ip(super::rate_limit::extract_ip(&req, state.trusted_proxies()))
                .detail(format!(
                    "{} {} origin {:?}",
                    req.method(),
                    req.uri().path(),
                    req.headers().get(ORIGIN)
                )),
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
fn is_allowed(
    headers: &HeaderMap,
    allowed_origins: &[String],
    trusted_hosts: &[String],
    scheme: &str,
) -> bool {
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok());
    let listed = origin.is_some_and(|o| allowed_origins.iter().any(|a| a == "*" || a == o));
    let host = headers.get(HOST).and_then(|v| v.to_str().ok());
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        // The browser says the request is same-origin (or from the address
        // bar). A DNS-rebinding page sends `same-origin` too, with its own
        // name in `Host`, so the Host must be one this server answers on.
        Some(site)
            if site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none") =>
        {
            host.is_some_and(|h| host_is_trusted(h, trusted_hosts))
        }
        Some(_) => listed,
        None => match origin {
            None => true,
            Some(o) => listed || names_host(o, host, scheme),
        },
    }
}

/// The host part of a `Host` header value or trusted-host entry, without
/// port or IPv6 brackets (`LOCALHOST:7474`, `[::1]:7474` → `localhost`, `::1`).
fn host_part(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        // [::1]:7474
        return rest.split(']').next().unwrap_or(rest);
    }
    // A bare IPv6 literal has several colons; only host:port has one.
    if host.matches(':').count() > 1 {
        return host;
    }
    host.split(':').next().unwrap_or(host)
}

/// Whether `host` (a `Host` header value) is a trusted name for this server.
fn host_is_trusted(host: &str, trusted_hosts: &[String]) -> bool {
    let host = host_part(host).to_ascii_lowercase();
    trusted_hosts
        .iter()
        .any(|t| host_part(t).to_ascii_lowercase() == host)
}

/// Whether `origin` (`scheme://host[:port]`) names the authority in `Host`
/// on the effective request `scheme`.
fn names_host(origin: &str, host: Option<&str>, scheme: &str) -> bool {
    let Some(host) = host else { return false };
    let Some((origin_scheme, authority)) = origin.split_once("://") else {
        return false; // "null" and other opaque origins
    };
    !authority.is_empty()
        && origin_scheme.eq_ignore_ascii_case(scheme)
        && authority.eq_ignore_ascii_case(host)
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
        let trusted = crate::state::default_trusted_hosts();
        let http = "http";

        // No browser headers: a non-browser client.
        assert!(is_allowed(&headers(&[host]), none, &trusted, http));
        // Fetch metadata decides when present, but `same-origin` needs a
        // trusted Host: a DNS-rebinding page sends it with its own name.
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "same-origin"),
                ("origin", "https://proxy.example")
            ]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[
                ("host", "evil.example:7474"),
                ("sec-fetch-site", "same-origin"),
                ("origin", "http://evil.example:7474")
            ]),
            none,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[host, ("sec-fetch-site", "none")]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[("sec-fetch-site", "same-origin")]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:5173")
            ]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            &any,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://app.example")
            ]),
            &listed,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            &any,
            &trusted,
            http
        ));
        // Origin alone: listed, or the server's own host on the same scheme.
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://localhost:7474")]),
            none,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://LOCALHOST:7474")]),
            none,
            &trusted,
            http
        ));
        // The scheme is compared too: an https Origin is not the http server.
        assert!(!is_allowed(
            &headers(&[host, ("origin", "https://localhost:7474")]),
            none,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "https://localhost:7474")]),
            none,
            &trusted,
            "https"
        ));
        assert!(!is_allowed(
            &headers(&[host, ("origin", "http://localhost:7475")]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[host, ("origin", "null")]),
            none,
            &trusted,
            http
        ));
        assert!(!is_allowed(
            &headers(&[("origin", "http://localhost:7474")]),
            none,
            &trusted,
            http
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "https://app.example")]),
            &listed,
            &trusted,
            http
        ));
    }

    #[test]
    fn host_names_are_trusted_ignoring_case_port_and_brackets() {
        let trusted = crate::state::default_trusted_hosts();
        assert!(host_is_trusted("localhost:7474", &trusted));
        assert!(host_is_trusted("LOCALHOST", &trusted));
        assert!(host_is_trusted("127.0.0.1:7474", &trusted));
        assert!(host_is_trusted("[::1]:7474", &trusted));
        assert!(!host_is_trusted("evil.example", &trusted));
        let custom = vec!["db.internal".to_string()];
        assert!(host_is_trusted("db.internal:7474", &custom));
        assert!(!host_is_trusted("db.internal.evil.example", &custom));
    }
}
