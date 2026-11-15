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
//!    is listed, or names the request's own `Host` on the effective scheme
//!    and that `Host` is trusted (the DNS-rebinding case of rule 1).
//!
//! With `--cors-origins '*'` every origin counts as listed only for a request
//! that carries credentials the page set itself (`Authorization: Bearer` or a
//! non-empty `X-API-Key`, without a Basic login), and only on a server that
//! checks credentials: a browser attaches a cached Basic login on its own, and
//! on a server without credentials any page could make a token up. A browser
//! can't set those headers on a WebSocket, so with `'*'` another origin opens
//! it only when listed.
//!
//! A trusted `Host` (rules 1 and 3) is a name in `--trusted-hosts` (with
//! loopback and `--host`) or any IP address: a DNS-rebinding page always
//! carries a DNS name of its own.
//!
//! Reads (GET, HEAD) other than WebSocket upgrades pass: CORS keeps a foreign
//! page from reading their answers. Preflights (OPTIONS) are left to the CORS
//! layer.
//!
//! A server without credentials also answers only on host names it trusts
//! (every request but the probes): a DNS-rebinding page reaches it under the
//! page's own name, where any read would otherwise be the page's to see. An IP
//! address is never such a name, so it is always answered.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, HOST, ORIGIN, UPGRADE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::ErrorBody;

/// Whether the server checks credentials (then a rebinding page has no login
/// for its own name, and the host rule is not needed).
fn has_credentials(state: &AppState) -> bool {
    #[cfg(feature = "auth")]
    {
        state.auth().is_some()
    }
    #[cfg(not(feature = "auth"))]
    {
        let _ = state;
        false
    }
}

/// The host a request is for: its `Host` header, or the authority of its
/// target when it has none (an HTTP/2 request names it in `:authority`).
fn request_host(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.as_str().to_string()))
}

/// Paths answered whatever the `Host`: the probes, which orchestrators send to
/// the pod's address or their own service name.
fn is_probe(path: &str) -> bool {
    path == "/health" || path == "/ready"
}

/// Axum middleware applying the cross-site rule to every request.
pub async fn cross_site_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let credentials = has_credentials(&state);
    if !credentials
        && !is_probe(req.uri().path())
        && request_host(&req).is_some_and(|h| !host_is_answered(&h, state.trusted_hosts()))
    {
        tracing::warn!(
            path = %req.uri().path(),
            host = ?req.headers().get(HOST),
            "refused a request for an untrusted host name"
        );
        let body = ErrorBody {
            error: "untrusted_host".to_string(),
            detail: Some(
                "this server has no credentials and answers only on its trusted host names; \
                 add this name to --trusted-hosts (GRAFEO_TRUSTED_HOSTS) to use it"
                    .to_string(),
            ),
        };
        return (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
    }
    if is_guarded(req.method(), req.headers())
        && !is_allowed(
            req.headers(),
            state.cors_origins(),
            state.trusted_hosts(),
            state.scheme(),
            credentials,
        )
    {
        tracing::warn!(
            method = %req.method(),
            path = %req.uri().path(),
            origin = ?req.headers().get(ORIGIN),
            "refused a cross-site request"
        );
        // The detail is truncated: a page can drive these refusals at high
        // volume without credentials, and the bounded audit channel must not
        // let them crowd out auth and token events.
        state.audit_event(
            grafeo_service::audit::AuditEvent::new("cross_site_refused")
                .transport("http")
                .source_ip(super::rate_limit::extract_ip(&req, state.trusted_proxies()))
                .detail(grafeo_service::audit::truncate(
                    &format!(
                        "{} {} origin {:?}",
                        req.method(),
                        req.uri().path(),
                        req.headers().get(ORIGIN)
                    ),
                    500,
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
    credentials_checked: bool,
) -> bool {
    let origin = headers.get(ORIGIN).and_then(|v| v.to_str().ok());
    // '*' lets another origin write only with credentials it set itself, and
    // only where the server checks them: without credentials any page could
    // send a made-up token.
    let listed = origin.is_some_and(|o| {
        allowed_origins.iter().any(|a| a == o)
            || (allowed_origins.iter().any(|a| a == "*")
                && credentials_checked
                && explicit_credentials(headers))
    });
    let host = headers.get(HOST).and_then(|v| v.to_str().ok());
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        // The browser says the request is same-origin (or from the address
        // bar). A DNS-rebinding page sends `same-origin` too, with its own
        // name in `Host`, so the Host must be one this server answers on.
        Some(site)
            if site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none") =>
        {
            host.is_some_and(|h| host_is_answered(h, trusted_hosts))
        }
        Some(_) => listed,
        // Without fetch metadata (browsers before it, or a non-browser
        // client with an Origin) the Origin must name this request's Host,
        // and that Host must be one this server answers on: a DNS-rebinding
        // page names its own host in both.
        None => match origin {
            None => true,
            Some(o) => {
                listed
                    || (names_host(o, host, scheme)
                        && host.is_some_and(|h| host_is_answered(h, trusted_hosts)))
            }
        },
    }
}

/// Whether the request carries credentials a page must set itself: a bearer
/// token or an API key. A browser sends neither on its own; it does send a
/// Basic login it has cached, so `Authorization: Basic` is not one.
fn explicit_credentials(headers: &HeaderMap) -> bool {
    let authorization = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    let scheme_is = |name: &str| {
        authorization
            .and_then(|v| v.get(..name.len()))
            .is_some_and(|s| s.eq_ignore_ascii_case(name))
    };
    // A Basic login rides along with any other header: the auth middleware
    // falls back to it, so its presence makes the request ambient.
    if scheme_is("basic ") {
        return false;
    }
    scheme_is("bearer ")
        || headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|key| !key.trim().is_empty())
}

/// Whether `host` is one this server answers on: a trusted name, or an IP
/// address (a DNS-rebinding page always carries a DNS name of its own, so an
/// IP is never one).
fn host_is_answered(host: &str, trusted_hosts: &[String]) -> bool {
    host_part(host).parse::<std::net::IpAddr>().is_ok() || host_is_trusted(host, trusted_hosts)
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
        assert!(is_allowed(&headers(&[host]), none, &trusted, http, true));
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
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[
                ("host", "evil.example:7474"),
                ("sec-fetch-site", "same-origin"),
                ("origin", "http://evil.example:7474")
            ]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[host, ("sec-fetch-site", "none")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[("sec-fetch-site", "same-origin")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "same-site"),
                ("origin", "http://localhost:5173")
            ]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[host, ("sec-fetch-site", "cross-site")]),
            &any,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://app.example")
            ]),
            &listed,
            &trusted,
            http,
            true
        ));
        // '*' lets another origin write only with credentials it set itself.
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example")
            ]),
            &any,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example"),
                ("authorization", "Basic YTpi")
            ]),
            &any,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example"),
                ("authorization", "Bearer t")
            ]),
            &any,
            &trusted,
            http,
            true
        ));
        // On a server without credentials a token proves nothing: '*' lets no
        // other origin write.
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example"),
                ("authorization", "Bearer t")
            ]),
            &any,
            &trusted,
            http,
            false
        ));
        // A Basic login next to a key, or an empty key, is not explicit.
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example"),
                ("x-api-key", "k"),
                ("authorization", "Basic YTpi")
            ]),
            &any,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[
                host,
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://evil.example"),
                ("x-api-key", " ")
            ]),
            &any,
            &trusted,
            http,
            true
        ));
        // Origin alone: listed, or the server's own host on the same scheme.
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://localhost:7474")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "http://LOCALHOST:7474")]),
            none,
            &trusted,
            http,
            true
        ));
        // The scheme is compared too: an https Origin is not the http server.
        assert!(!is_allowed(
            &headers(&[host, ("origin", "https://localhost:7474")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "https://localhost:7474")]),
            none,
            &trusted,
            "https",
            true
        ));
        assert!(!is_allowed(
            &headers(&[host, ("origin", "http://localhost:7475")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[host, ("origin", "null")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(!is_allowed(
            &headers(&[("origin", "http://localhost:7474")]),
            none,
            &trusted,
            http,
            true
        ));
        assert!(is_allowed(
            &headers(&[host, ("origin", "https://app.example")]),
            &listed,
            &trusted,
            http,
            true
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
