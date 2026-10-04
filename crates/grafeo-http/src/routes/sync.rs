//! Sync endpoints for offline-first applications.
//!
//! # Endpoints
//!
//! - `GET /db/{name}/changes?since=<epoch>&limit=<n>` — pull changefeed
//! - `POST /db/{name}/sync` — push client changes with LWW conflict resolution
//! - `GET /db/{name}/changes/stream` — SSE push stream (requires `push-changefeed`)
//!
//! Requires the `sync` feature (implies `cdc`).

use axum::extract::{Path, Query, State};
use axum::response::Json;
use serde::Deserialize;

use grafeo_service::sync::{ChangesResponse, SyncRequest, SyncResponse, SyncService};

use crate::error::ApiError;
use crate::state::AppState;

const MAX_LIMIT: usize = 10_000;
const DEFAULT_LIMIT: usize = 1_000;

/// Query parameters for the changefeed endpoint.
#[derive(Debug, Deserialize)]
pub struct ChangesQuery {
    /// Return events with epoch >= this value: 0 (the default) for the full
    /// history, else the previous response's `server_epoch + 1`.
    #[serde(default)]
    pub since: u64,
    /// About how many events to return. Defaults to 1 000, max 10 000. An
    /// epoch is never split, so a response can hold more.
    pub limit: Option<usize>,
}

/// Poll for change events since a given epoch.
///
/// Returns the mutations (create, update, delete) for nodes and edges in the
/// named database where the MVCC epoch is >= `since`, in the order they
/// happened.
///
/// `server_epoch` in the response is the resume cursor: every event up to it
/// has been returned. Pass `server_epoch + 1` as `since` on the next request.
/// A response never splits an epoch: past `limit` events it runs to the end
/// of the epoch of the `limit`-th one, and `server_epoch` is the epoch of its
/// last event. If `changes.len() >= limit`, more events may be waiting: poll
/// again straight away.
pub async fn db_changes(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<ChangesQuery>,
) -> Result<Json<ChangesResponse>, ApiError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let since = params.since;

    let result = tokio::task::spawn_blocking(move || {
        SyncService::pull(state.databases(), &name, since, limit)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))??;

    Ok(Json(result))
}

/// Apply a client changeset to the named database.
///
/// Accepts a JSON body with `{ client_id, last_seen_epoch, changes: [...] }`.
/// Changes are applied in order with last-write-wins (LWW) conflict
/// resolution: if the server has a more recent CDC timestamp for the target
/// entity, the client change is skipped and recorded in `conflicts`.
///
/// Returns `{ server_epoch, applied, skipped, conflicts, id_mappings }`.
/// The `id_mappings` array maps each create request (by index) to the
/// server-assigned entity ID.
pub async fn db_apply(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    let result =
        tokio::task::spawn_blocking(move || SyncService::apply(state.databases(), &name, request))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))??;

    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// SSE push stream (requires `push-changefeed` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "push-changefeed")]
mod sse {
    use std::convert::Infallible;

    use axum::extract::{Path, Query, State};
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    use grafeo_service::sync::{ChangeEventDto, ChangesResponse, SyncService};

    use crate::error::ApiError;
    use crate::routes::sync::ChangesQuery;
    use crate::state::AppState;

    /// Events per pull of history.
    const HISTORY_PAGE: usize = 10_000;

    /// Server-Sent Events stream of change events for the named database.
    ///
    /// The client receives all historical events from epoch `since` on
    /// first (0 for the full history, else one past the last epoch it saw),
    /// then live events as they are committed. The stream stays open until
    /// the client disconnects.
    ///
    /// Events are newline-delimited JSON objects in the `data:` field of each
    /// SSE event, matching the `ChangeEventDto` schema.
    ///
    /// The `limit` query parameter is ignored for the streaming endpoint.
    pub async fn db_changes_stream(
        State(state): State<AppState>,
        Path(name): Path<String>,
        Query(params): Query<ChangesQuery>,
    ) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
        // The first page is pulled up front so a missing database, or one
        // without CDC, is an HTTP error rather than an empty stream.
        let first = history_page(&state, &name, params.since).await?;

        let stream = async_stream::stream! {
            // History page by page, each pull resuming after the previous
            // one's cursor, until a page is not full.
            let mut page = first;
            let live_since = loop {
                let next_since = page.server_epoch.saturating_add(1);
                let full = page.changes.len() >= HISTORY_PAGE;
                for event in &page.changes {
                    yield Ok(sse_event(event));
                }
                if !full {
                    break next_since;
                }
                match history_page(&state, &name, next_since).await {
                    Ok(next) => page = next,
                    Err(e) => {
                        tracing::debug!("SSE history pull for '{name}' failed: {e}");
                        return;
                    }
                }
            };

            // Then live events from the hub, starting after the history.
            let mut receiver = state
                .change_hub()
                .subscribe(&name, live_since, state.service().clone());
            loop {
                match receiver.recv().await {
                    Ok(event) => yield Ok(sse_event(&event)),
                    Err(RecvError::Lagged(n)) => {
                        tracing::debug!("SSE receiver lagged by {n} events");
                        // Continue: the client will see the next available event.
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        };

        Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
    }

    /// Pulls the history of `name` from epoch `since` on, one page.
    async fn history_page(
        state: &AppState,
        name: &str,
        since: u64,
    ) -> Result<ChangesResponse, ApiError> {
        let state = state.clone();
        let name = name.to_string();
        let page = tokio::task::spawn_blocking(move || {
            SyncService::pull(state.databases(), &name, since, HISTORY_PAGE)
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
        Ok(page)
    }

    fn sse_event(event: &ChangeEventDto) -> Event {
        let json = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
        Event::default().data(json)
    }
}

#[cfg(feature = "push-changefeed")]
pub use sse::db_changes_stream;
