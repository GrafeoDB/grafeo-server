//! Grafeo Studio — embedded web UI served from compiled-in static files.
//!
//! The `client/dist/` directory is embedded at compile time using `rust-embed`.
//! If the directory doesn't exist at build time (no UI built), endpoints
//! return 404 gracefully.

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../client/dist/"]
#[prefix = ""]
struct UiAssets;

/// Creates a router that serves the embedded web UI.
pub fn router() -> Router {
    Router::new()
        .route("/", get(root_redirect))
        .route("/studio", get(studio_index))
        .route("/studio/", get(studio_index))
        .route("/studio/{*path}", get(studio_static))
}

async fn root_redirect() -> Redirect {
    Redirect::permanent("/studio/")
}

async fn studio_index() -> Response {
    serve_file("index.html")
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

async fn studio_static(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches("/studio/");
    if let Some(resp) = serve_file(path) {
        return resp;
    }
    // SPA fallback: serve index.html for client-side routes
    serve_file("index.html")
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

fn serve_file(path: &str) -> Option<Response> {
    let asset = UiAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            asset.data.to_vec(),
        )
            .into_response(),
    )
}
