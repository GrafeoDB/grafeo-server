//! Sync endpoints for offline-first applications.
//!
//! # Endpoints
//!
//! - `GET /db/{name}/changes?since=<epoch>&limit=<n>`: pull changefeed
//! - `POST /db/{name}/sync`: push client changes with LWW conflict resolution
//! - `GET /db/{name}/changes/stream`: SSE push stream (requires `push-changefeed`)
//!
//! Requires the `sync` feature (implies `cdc`).

use axum::extract::{Path, Query, State};
use axum::response::Json;
use serde::Deserialize;

use grafeo_service::sync::{ChangesResponse, SyncRequest, SyncResponse, SyncService};

use crate::error::ApiError;
use crate::middleware::auth_context::AuthContext;
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
/// again straight away. `server_epoch` is never below `since - 1`. Advance a
/// stored cursor only after a response that has changes: on a database
/// without writes yet an empty response reports epoch 0, which is still open.
///
/// With auth on, the token must be allowed on the database.
pub async fn db_changes(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(name): Path<String>,
    Query(params): Query<ChangesQuery>,
) -> Result<Json<ChangesResponse>, ApiError> {
    auth.check_db_access(&name)?;
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
///
/// With auth on, the token must be allowed on the database and may write.
pub async fn db_apply(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(name): Path<String>,
    Json(request): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    auth.check_db_access(&name)?;
    auth.check_write()?;
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
    use futures_util::{Stream, StreamExt};

    use crate::error::ApiError;
    use crate::middleware::auth_context::AuthContext;
    use crate::routes::change_stream::{StreamItem, change_stream, history_page};
    use crate::routes::sync::ChangesQuery;
    use crate::state::AppState;

    /// Server-Sent Events stream of change events for the named database.
    ///
    /// The client receives all historical events from epoch `since` on
    /// first (0 for the full history, else one past the last epoch it saw),
    /// then live events as they are committed. The stream stays open until
    /// the client disconnects or one of the named events below ends it.
    ///
    /// Events are newline-delimited JSON objects in the `data:` field of each
    /// SSE event, matching the `ChangeEventDto` schema. Two named events end
    /// the stream:
    ///
    /// - `lagged`: the client fell too far behind and lost events. The data
    ///   is `{"skipped": n, "since": e}`: reconnect with `?since=<e>` (the
    ///   first epoch not delivered in full; inclusive, so epoch 0 is resumed
    ///   too).
    /// - `error`: a history pull failed, or the live feed stopped (the
    ///   database was dropped or restored, or its CDC turned off). The data
    ///   is `{"message": "..."}`; an internal failure reads "internal error",
    ///   with the detail in the server log.
    ///
    /// Both are terminal: the server ends the stream after either. A browser
    /// `EventSource` then reconnects by itself with the original `?since=`,
    /// replaying what it already has, unless the client calls `close()` on
    /// these events and opens a new stream from the cursor it received.
    ///
    /// The `limit` query parameter is ignored for the streaming endpoint.
    ///
    /// With auth on, the token must be allowed on the database.
    pub async fn db_changes_stream(
        State(state): State<AppState>,
        auth: AuthContext,
        Path(name): Path<String>,
        Query(params): Query<ChangesQuery>,
    ) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
        auth.check_db_access(&name)?;
        // The first page is pulled up front so a missing database, or one
        // without CDC, is an HTTP error rather than an empty stream.
        let first = history_page(&state, &name, params.since).await?;
        let stream =
            change_stream(state, name, first, params.since).map(|item| Ok(sse_event(item)));
        Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
    }

    /// The SSE event a change stream item becomes: a change in `data:`, or
    /// a named `lagged` or `error` event that ends the stream.
    fn sse_event(item: StreamItem) -> Event {
        match item {
            StreamItem::Change(event) => Event::default().data(to_json(&event)),
            StreamItem::Lagged(notice) => Event::default().event("lagged").data(to_json(&notice)),
            StreamItem::Error(message) => Event::default()
                .event("error")
                .data(to_json(&serde_json::json!({ "message": message }))),
        }
    }

    fn to_json(value: &impl serde::Serialize) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(feature = "push-changefeed")]
pub use sse::db_changes_stream;
