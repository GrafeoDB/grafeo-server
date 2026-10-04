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
/// again straight away. A response without changes can report a lower
/// `server_epoch` than the cursor already held: keep the larger one.
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

    use grafeo_service::changefeed::{LaggedNotice, LiveCursor, LiveItem};
    use grafeo_service::sync::{ChangeEventDto, ChangesResponse, SyncService};

    use crate::error::ApiError;
    use crate::middleware::auth_context::AuthContext;
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
    /// SSE event, matching the `ChangeEventDto` schema. A named event ends
    /// the stream:
    ///
    /// - `lagged`: the client fell too far behind and lost events. The data
    ///   is `{"skipped": n, "last_epoch": e}`: reconnect with `since = e + 1`.
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
            change_stream(state, name, first, params.since).map(|item| Ok(item.into_event()));
        Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
    }

    /// What the change stream sends: a change, or the message that ends it.
    #[derive(Debug)]
    enum StreamItem {
        Change(Box<ChangeEventDto>),
        Lagged(LaggedNotice),
    }

    impl StreamItem {
        fn into_event(self) -> Event {
            match self {
                Self::Change(event) => Event::default().data(to_json(&event)),
                Self::Lagged(notice) => Event::default().event("lagged").data(to_json(&notice)),
            }
        }
    }

    /// The changes of `name` from epoch `since` on: the history page by page
    /// (`first` is its first page), then live events from the hub.
    ///
    /// It subscribes to the hub before it sends the last history page and
    /// then pulls once more, so nothing the hub broadcasts while the history
    /// streams is lost; live events the history already sent are dropped.
    fn change_stream(
        state: AppState,
        name: String,
        first: ChangesResponse,
        since: u64,
    ) -> impl Stream<Item = StreamItem> {
        async_stream::stream! {
            // Each pull resumes after the previous one's cursor. The cursor
            // never moves back: a pull that finds nothing new can report a
            // lower epoch.
            let mut page = first;
            let mut next_since = since;
            while page.changes.len() >= HISTORY_PAGE {
                next_since = next_since.max(page.server_epoch.saturating_add(1));
                for event in page.changes {
                    yield StreamItem::Change(Box::new(event));
                }
                page = match history_page(&state, &name, next_since).await {
                    Ok(next) => next,
                    Err(e) => {
                        tracing::debug!("SSE history pull for '{name}' failed: {e}");
                        return;
                    }
                };
            }

            // The last page: subscribe before sending it, so the hub cannot
            // move past an event while this stream is still in its history.
            next_since = next_since.max(page.server_epoch.saturating_add(1));
            let mut receiver = state
                .change_hub()
                .subscribe(&name, next_since, state.service().clone());
            for event in page.changes {
                yield StreamItem::Change(Box::new(event));
            }
            // What the hub broadcast before the subscription is in the log
            // by now: pull it.
            loop {
                let page = match history_page(&state, &name, next_since).await {
                    Ok(page) => page,
                    Err(e) => {
                        tracing::debug!("SSE history pull for '{name}' failed: {e}");
                        return;
                    }
                };
                next_since = next_since.max(page.server_epoch.saturating_add(1));
                let full = page.changes.len() >= HISTORY_PAGE;
                for event in page.changes {
                    yield StreamItem::Change(Box::new(event));
                }
                if !full {
                    break;
                }
            }

            let mut cursor = LiveCursor::new(next_since);
            loop {
                match cursor.next(&mut receiver).await {
                    LiveItem::Change(event) => yield StreamItem::Change(event),
                    LiveItem::Lagged(notice) => {
                        tracing::warn!(
                            db = %name,
                            skipped = notice.skipped,
                            last_epoch = notice.last_epoch,
                            "SSE change stream fell behind; ending it"
                        );
                        yield StreamItem::Lagged(notice);
                        break;
                    }
                    LiveItem::Closed => break,
                }
            }
        }
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

    fn to_json(value: &impl serde::Serialize) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use super::*;

        fn cdc_state() -> AppState {
            let state = AppState::new_in_memory(300);
            state
                .databases()
                .get("default")
                .unwrap()
                .db()
                .set_cdc_enabled(true);
            state
        }

        async fn next_item(stream: &mut (impl Stream<Item = StreamItem> + Unpin)) -> StreamItem {
            tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .expect("no item within 10 s")
                .expect("the stream ended")
        }

        fn change_label(item: StreamItem) -> String {
            match item {
                StreamItem::Change(event) => event.labels.unwrap().remove(0),
                other @ StreamItem::Lagged(_) => panic!("expected a change, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_write_during_the_history_arrives_exactly_once() {
            let state = cdc_state();
            let db = state.databases().get("default").unwrap().db();
            for _ in 0..3 {
                db.create_node(&["Old"]).unwrap();
            }
            // Another subscriber keeps the hub running from here on.
            let mut other = state.change_hub().subscribe(
                "default",
                db.current_epoch().0 + 1,
                state.service().clone(),
            );

            let first = history_page(&state, "default", 0).await.unwrap();
            let mut stream = Box::pin(change_stream(
                state.clone(),
                "default".to_string(),
                first,
                0,
            ));
            assert_eq!(change_label(next_item(&mut stream).await), "Old");

            // A write the hub broadcasts (and moves past) while the new
            // stream is still sending its history.
            let during = db.create_node(&["During"]).unwrap().as_u64();
            let seen = tokio::time::timeout(Duration::from_secs(10), other.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(seen.id, during);

            let mut labels = Vec::new();
            for _ in 0..3 {
                labels.push(change_label(next_item(&mut stream).await));
            }
            assert_eq!(labels, ["Old", "Old", "During"]);

            // Live from here: the hub's copy of `During` is not sent again.
            db.create_node(&["After"]).unwrap();
            assert_eq!(change_label(next_item(&mut stream).await), "After");
        }

        #[tokio::test]
        async fn a_lagging_stream_ends_with_a_resumable_notice() {
            let state = cdc_state();
            let db = state.databases().get("default").unwrap().db();
            let first = history_page(&state, "default", 0).await.unwrap();
            let mut stream = Box::pin(change_stream(
                state.clone(),
                "default".to_string(),
                first,
                0,
            ));
            db.create_node(&["Warmup"]).unwrap();
            let warmup_epoch = db.current_epoch().0;
            assert_eq!(change_label(next_item(&mut stream).await), "Warmup");

            // One epoch with more events than the hub's channel holds.
            db.batch_create_nodes_with_labels(
                &["Burst"],
                vec![std::collections::HashMap::new(); 2_000],
            )
            .unwrap();
            match next_item(&mut stream).await {
                StreamItem::Lagged(notice) => {
                    assert!(notice.skipped >= 2_000 - 1_024, "{notice:?}");
                    assert_eq!(notice.last_epoch, warmup_epoch);
                }
                other @ StreamItem::Change(_) => panic!("expected a lag, got {other:?}"),
            }
            assert!(stream.next().await.is_none(), "the lag ends the stream");

            // Resuming at last_epoch + 1 gets the whole burst.
            let resumed =
                SyncService::pull(state.databases(), "default", warmup_epoch + 1, 10_000).unwrap();
            assert_eq!(resumed.changes.len(), 2_000);
        }
    }
}

#[cfg(feature = "push-changefeed")]
pub use sse::db_changes_stream;
