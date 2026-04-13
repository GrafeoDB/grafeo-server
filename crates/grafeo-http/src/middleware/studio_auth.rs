//! Studio auth response augmentation.
//!
//! Adds `WWW-Authenticate: Basic realm="Grafeo Studio"` to 401 responses
//! on Studio routes so browsers show a native credential prompt instead
//! of the JSON error body that `auth_middleware` returns for API routes.
//!
//! Layered on the Studio router in `main.rs` outside `auth_middleware` so
//! it sees the 401 response and can mutate headers before the client receives
//! it.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

pub async fn studio_www_authenticate(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    if response.status() == StatusCode::UNAUTHORIZED
        && !response.headers().contains_key(header::WWW_AUTHENTICATE)
    {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Basic realm="Grafeo Studio", charset="UTF-8""#),
        );
    }
    response
}

/// Wraps a router with the auth middleware and the Studio-specific
/// WWW-Authenticate response wrapper. Used by `main.rs` to gate Studio
/// behind the same auth layer as the API routes, while keeping browser
/// credential prompts working on 401s.
///
/// When auth is not configured at runtime, `auth_middleware` is a
/// pass-through, so this function is safe to call unconditionally
/// whenever the `auth` feature is compiled in.
pub fn wrap_with_auth(router: axum::Router, state: crate::state::AppState) -> axum::Router {
    router
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::middleware::auth::auth_middleware,
        ))
        .layer(axum::middleware::from_fn(studio_www_authenticate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware::from_fn;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn unauthorized() -> Response {
        StatusCode::UNAUTHORIZED.into_response()
    }

    async fn ok() -> Response {
        StatusCode::OK.into_response()
    }

    async fn unauthorized_with_header() -> Response {
        let mut resp = StatusCode::UNAUTHORIZED.into_response();
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        resp
    }

    #[tokio::test]
    async fn adds_header_on_401() {
        let app = Router::new()
            .route("/", get(unauthorized))
            .layer(from_fn(studio_www_authenticate));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            r#"Basic realm="Grafeo Studio", charset="UTF-8""#
        );
    }

    #[tokio::test]
    async fn leaves_200_untouched() {
        let app = Router::new()
            .route("/", get(ok))
            .layer(from_fn(studio_www_authenticate));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(header::WWW_AUTHENTICATE).is_none());
    }

    #[tokio::test]
    async fn preserves_existing_header() {
        let app = Router::new()
            .route("/", get(unauthorized_with_header))
            .layer(from_fn(studio_www_authenticate));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
    }
}
