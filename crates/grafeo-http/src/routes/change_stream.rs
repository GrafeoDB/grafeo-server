//! The change stream shared by the SSE route and WebSocket subscriptions:
//! the history of a database from an epoch on, page by page, then its live
//! changes from the change hub, with no gap and no duplicate between them.
//!
//! Requires the `push-changefeed` feature.

use futures_util::Stream;

use grafeo_service::changefeed::{LaggedNotice, LiveCursor, LiveItem};
use grafeo_service::error::ServiceError;
use grafeo_service::sync::{ChangeEventDto, ChangesResponse, SyncService};

use crate::error::ApiError;
use crate::state::AppState;

/// Events per pull of history.
const HISTORY_PAGE: usize = 10_000;

/// The `Error` message of a stream whose live feed stopped: the database was
/// dropped, restored, compacted or replaced, or its CDC turned off.
/// Reconnecting tells why.
pub(crate) const FEED_CLOSED: &str = "change feed closed";

/// What a change stream yields: a change, or the item that ends it.
#[derive(Debug)]
pub(crate) enum StreamItem {
    Change(Box<ChangeEventDto>),
    /// The subscriber fell behind and lost events.
    Lagged(LaggedNotice),
    /// A history pull failed, or the live feed stopped ([`FEED_CLOSED`]).
    Error(String),
}

/// The changes of `name` from epoch `since` on: the history page by page
/// (`first` is its first page, from [`history_page`]), then live events from
/// the hub. It ends after a `Lagged` or `Error` item. `subscriber` names the
/// stream in the log (an SSE stream, or a WebSocket subscription by its
/// `sub_id`), so a lag or an error can be traced to it.
///
/// It subscribes to the hub before it sends the last history page and then
/// pulls once more, so nothing the hub broadcasts while the history streams
/// is lost; live events the history already sent are dropped.
pub(crate) fn change_stream(
    state: AppState,
    name: String,
    first: ChangesResponse,
    since: u64,
    subscriber: String,
) -> impl Stream<Item = StreamItem> {
    async_stream::stream! {
        // Each pull resumes after the cursor of the last one that returned
        // events (see `resume_after`).
        let mut page = first;
        let mut next_since = since;
        while page.changes.len() >= HISTORY_PAGE {
            next_since = resume_after(next_since, &page);
            for event in page.changes {
                yield StreamItem::Change(Box::new(event));
            }
            page = match history_page(&state, &name, next_since).await {
                Ok(next) => next,
                Err(e) => {
                    yield history_failed(&name, &subscriber, &e);
                    return;
                }
            };
        }

        // The last page: subscribe before sending it, so the hub cannot move
        // past an event while this stream is still in its history.
        next_since = resume_after(next_since, &page);
        let mut receiver = state
            .change_hub()
            .subscribe(&name, next_since, state.service().clone());
        for event in page.changes {
            yield StreamItem::Change(Box::new(event));
        }
        // What the hub broadcast before the subscription is in the log by
        // now: pull it.
        loop {
            let page = match history_page(&state, &name, next_since).await {
                Ok(page) => page,
                Err(e) => {
                    yield history_failed(&name, &subscriber, &e);
                    return;
                }
            };
            next_since = resume_after(next_since, &page);
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
                        subscriber = %subscriber,
                        skipped = notice.skipped,
                        since = notice.since,
                        "change stream fell behind; ending it"
                    );
                    yield StreamItem::Lagged(notice);
                    break;
                }
                LiveItem::Closed => {
                    tracing::warn!(
                        db = %name,
                        subscriber = %subscriber,
                        "change feed stopped; ending the change stream"
                    );
                    yield StreamItem::Error(FEED_CLOSED.to_string());
                    break;
                }
            }
        }
    }
}

/// Where the pull after `page` starts: past its cursor when it returned
/// events, still at `next_since` when it was empty. An empty page has nothing
/// to move past, and its cursor can name an epoch that is still open (0
/// before the first write).
fn resume_after(next_since: u64, page: &ChangesResponse) -> u64 {
    if page.changes.is_empty() {
        next_since
    } else {
        next_since.max(page.server_epoch.saturating_add(1))
    }
}

/// The `Error` item for a failed history pull. An internal error's text stays
/// in the log, as in the HTTP error mapping.
fn history_failed(name: &str, subscriber: &str, error: &ApiError) -> StreamItem {
    tracing::warn!(
        db = %name,
        subscriber = %subscriber,
        error = %error,
        "change stream history pull failed; ending it"
    );
    StreamItem::Error(match &error.0 {
        ServiceError::Internal(_) => "internal error".to_string(),
        _ => error.to_string(),
    })
}

/// Pulls the history of `name` from epoch `since` on, one page. Callers pull
/// the first page before they start a stream, so a missing database, or one
/// without CDC, is an error up front rather than a stream that ends at once.
pub(crate) async fn history_page(
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;

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
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn a_history_error_hides_internal_detail_only() {
        let internal = history_failed(
            "default",
            "test",
            &ApiError::internal("disk at /var/x full"),
        );
        match internal {
            StreamItem::Error(message) => assert_eq!(message, "internal error"),
            other => panic!("expected an error, got {other:?}"),
        }
        let bad = history_failed(
            "default",
            "test",
            &ApiError::bad_request("CDC is not enabled"),
        );
        match bad {
            StreamItem::Error(message) => assert!(message.contains("CDC is not enabled")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn only_a_page_with_events_moves_the_history_cursor() {
        let empty = ChangesResponse {
            server_epoch: 0,
            changes: Vec::new(),
        };
        assert_eq!(resume_after(0, &empty), 0, "epoch 0 may still fill");
        assert_eq!(resume_after(5, &empty), 5);

        let page: ChangesResponse = serde_json::from_value(serde_json::json!({
            "server_epoch": 4,
            "changes": [{
                "id": 1, "entity_type": "node", "kind": "create",
                "epoch": 4, "timestamp": 1,
            }],
        }))
        .unwrap();
        assert_eq!(resume_after(0, &page), 5);
        assert_eq!(resume_after(9, &page), 9, "never back");
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
            "test".to_string(),
        ));
        assert_eq!(change_label(next_item(&mut stream).await), "Old");

        // A write the hub broadcasts (and moves past) while the new stream is
        // still sending its history.
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
            "test".to_string(),
        ));
        db.create_node(&["Warmup"]).unwrap();
        let warmup_epoch = db.current_epoch().0;
        assert_eq!(change_label(next_item(&mut stream).await), "Warmup");

        // One epoch of 5 000 events, several times what the hub's channel
        // holds. The hub sends a whole epoch in one poll without yielding,
        // and this runtime has one thread, so the stream reads nothing until
        // the burst is in the channel and lags for certain. Only a loss is
        // asserted, not how many.
        db.batch_create_nodes_with_labels(
            &["Burst"],
            vec![std::collections::HashMap::new(); 5_000],
        )
        .unwrap();
        let since = match next_item(&mut stream).await {
            StreamItem::Lagged(notice) => {
                assert!(notice.skipped > 0, "{notice:?}");
                // The warm-up came with the history: the live part starts
                // after it.
                assert_eq!(notice.since, warmup_epoch + 1);
                notice.since
            }
            other => panic!("expected a lag, got {other:?}"),
        };
        assert!(stream.next().await.is_none(), "the lag ends the stream");

        // Resuming at `since` gets the whole burst.
        let resumed = SyncService::pull(state.databases(), "default", since, 10_000).unwrap();
        assert_eq!(resumed.changes.len(), 5_000);
    }

    #[tokio::test]
    async fn a_failed_history_pull_ends_the_stream_with_an_error() {
        let state = cdc_state();
        let db = state.databases().get("default").unwrap().db();
        db.create_node(&["Old"]).unwrap();
        let first = history_page(&state, "default", 0).await.unwrap();
        let mut stream = Box::pin(change_stream(
            state.clone(),
            "default".to_string(),
            first,
            0,
            "test".to_string(),
        ));
        assert_eq!(change_label(next_item(&mut stream).await), "Old");

        // The pull after the last page fails: CDC is off now.
        db.set_cdc_enabled(false);
        match next_item(&mut stream).await {
            StreamItem::Error(message) => {
                assert!(message.contains("CDC is not enabled"), "{message}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(stream.next().await.is_none(), "the failure ends the stream");
    }

    #[tokio::test]
    async fn a_dropped_database_ends_the_live_stream_with_an_error() {
        let state = cdc_state();
        state
            .databases()
            .create(&grafeo_service::types::CreateDatabaseRequest {
                name: "gone".to_string(),
                database_type: grafeo_service::types::DatabaseType::Lpg,
                storage_mode: grafeo_service::types::StorageMode::InMemory,
                options: grafeo_service::types::DatabaseOptions::default(),
                schema_file: None,
                schema_filename: None,
            })
            .unwrap();
        let db = state.databases().get("gone").unwrap().db();
        db.set_cdc_enabled(true);
        db.create_node(&["Old"]).unwrap();

        let first = history_page(&state, "gone", 0).await.unwrap();
        let mut stream = Box::pin(change_stream(
            state.clone(),
            "gone".to_string(),
            first,
            0,
            "test".to_string(),
        ));
        assert_eq!(change_label(next_item(&mut stream).await), "Old");
        // Once this has arrived the stream pulls no more history.
        db.create_node(&["Live"]).unwrap();
        assert_eq!(change_label(next_item(&mut stream).await), "Live");
        drop(db);

        // The hub's next poll fails and closes the feed.
        state.databases().delete("gone").unwrap();
        match next_item(&mut stream).await {
            StreamItem::Error(message) => assert_eq!(message, FEED_CLOSED),
            other => panic!("expected the end of the feed, got {other:?}"),
        }
        assert!(
            stream.next().await.is_none(),
            "the closed feed ends the stream"
        );
    }
}
