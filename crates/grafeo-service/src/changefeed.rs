//! Real-time push changefeed for offline-first applications.
//!
//! `ChangeHub` manages one `tokio::sync::broadcast` channel per database.
//! A lightweight background task polls the CDC log every 100 ms and broadcasts
//! new `ChangeEventDto` values to all active subscribers.
//!
//! # Usage
//!
//! ```no_run
//! # use grafeo_service::changefeed::ChangeHub;
//! # use grafeo_service::ServiceState;
//! # async fn example(hub: &ChangeHub, state: ServiceState) {
//! let mut receiver = hub.subscribe("default", 0, state);
//!
//! while let Ok(event) = receiver.recv().await {
//!     println!("live event: {:?}", event.kind);
//! }
//! # }
//! ```
//!
//! A subscriber that falls more than the channel capacity behind loses
//! events. [`LiveCursor`] turns that into a [`LaggedNotice`] that says where
//! to resume, so the subscription can end with it instead of going on with a
//! silent gap.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::ServiceState;
use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::sync::{ChangeEventDto, SyncService};

/// Capacity of each per-database broadcast channel.
const CHANNEL_CAPACITY: usize = 1_024;

/// Interval between CDC polls for each active database channel.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Maximum events pulled per poll tick.
const POLL_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// ChangeHub
// ---------------------------------------------------------------------------

struct ChannelState {
    sender: broadcast::Sender<ChangeEventDto>,
    /// The `since` of the next poll: one past the resume cursor
    /// (`server_epoch`) of the last pull that returned events.
    next_since: Arc<AtomicU64>,
    /// Handle to the background poll task. `None` means no task is running.
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ChannelState {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            sender,
            next_since: Arc::new(AtomicU64::new(0)),
            task: Mutex::new(None),
        }
    }
}

/// Manages real-time push channels for all active databases.
///
/// Cloning is cheap: the inner map is `Arc`-wrapped.
#[derive(Clone)]
pub struct ChangeHub {
    channels: Arc<DashMap<String, Arc<ChannelState>>>,
}

impl ChangeHub {
    /// Creates a new empty hub.
    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
        }
    }

    /// Subscribes to live change events for `db_name`.
    ///
    /// `since_epoch` is the first epoch the subscriber has not seen: the
    /// `since` of its next pull (`server_epoch + 1` after a pull, 0 for the
    /// full history). The hub's polls move up to it, never back. Historical
    /// events before it must be fetched separately via `SyncService::pull()`:
    /// the receiver only yields events broadcast after the subscription.
    ///
    /// A background poll task is started (or restarted) automatically if none
    /// is currently running for this database.
    pub fn subscribe(
        &self,
        db_name: &str,
        since_epoch: u64,
        state: ServiceState,
    ) -> broadcast::Receiver<ChangeEventDto> {
        let channel = self
            .channels
            .entry(db_name.to_string())
            .or_insert_with(|| Arc::new(ChannelState::new()))
            .clone();

        // Move the next poll up to `since_epoch` so the poll task starts from here.
        channel.next_since.fetch_max(since_epoch, Ordering::Relaxed);

        self.ensure_task_running(db_name, &channel, state);

        channel.sender.subscribe()
    }

    /// Ensures a background poll task is running for `db_name`.
    fn ensure_task_running(&self, db_name: &str, channel: &Arc<ChannelState>, state: ServiceState) {
        let mut guard = channel.task.lock();

        // Check if the existing task is still alive.
        let needs_restart = match guard.as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        };

        if needs_restart {
            let db = db_name.to_string();
            let sender = channel.sender.clone();
            let next_since = Arc::clone(&channel.next_since);
            let handle = tokio::spawn(poll_task(db, sender, next_since, state));
            *guard = Some(handle);
        }
    }
}

impl Default for ChangeHub {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Live subscribers
// ---------------------------------------------------------------------------

/// Where a live subscriber stands: which broadcast events it still needs,
/// and where it resumes if it falls behind.
#[derive(Debug, Clone)]
pub struct LiveCursor {
    /// Events before this epoch are dropped: the subscriber has them already
    /// (from its history pull, or it asked to start here), and the hub may
    /// be behind it.
    since: u64,
    /// Epoch of the last event handed out, if any.
    last_sent: Option<u64>,
}

impl LiveCursor {
    /// A subscriber that needs the events from epoch `since` on.
    #[must_use]
    pub fn new(since: u64) -> Self {
        Self {
            since,
            last_sent: None,
        }
    }

    /// The next event `receiver` has for this subscriber, skipping the ones
    /// before its `since`.
    pub async fn next(&mut self, receiver: &mut broadcast::Receiver<ChangeEventDto>) -> LiveItem {
        loop {
            match receiver.recv().await {
                Ok(event) if event.epoch < self.since => {}
                Ok(event) => {
                    self.last_sent = Some(event.epoch);
                    return LiveItem::Change(Box::new(event));
                }
                Err(RecvError::Lagged(skipped)) => return LiveItem::Lagged(self.lagged(skipped)),
                Err(RecvError::Closed) => return LiveItem::Closed,
            }
        }
    }

    /// The notice for this subscriber after `skipped` events were dropped.
    /// The epoch of the last event sent may have lost events of its own, so
    /// it does not count as delivered in full.
    fn lagged(&self, skipped: u64) -> LaggedNotice {
        let first_open = self.last_sent.unwrap_or(self.since);
        LaggedNotice {
            skipped,
            last_epoch: first_open.saturating_sub(1),
        }
    }
}

/// What [`LiveCursor::next`] found.
#[derive(Debug)]
pub enum LiveItem {
    /// An event for the subscriber.
    Change(Box<ChangeEventDto>),
    /// The subscriber fell behind and lost events: end the subscription
    /// with this notice.
    Lagged(LaggedNotice),
    /// The hub's channel closed.
    Closed,
}

/// Ends a live subscription that fell behind the hub.
///
/// Resume with a pull or a new subscription at `since = last_epoch + 1`.
/// Events of that epoch already received may arrive again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaggedNotice {
    /// Events the subscriber lost.
    pub skipped: u64,
    /// The newest epoch whose events were all delivered.
    pub last_epoch: u64,
}

// ---------------------------------------------------------------------------
// Background poll task
// ---------------------------------------------------------------------------

async fn poll_task(
    db_name: String,
    sender: broadcast::Sender<ChangeEventDto>,
    next_since: Arc<AtomicU64>,
    state: ServiceState,
) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        // Stop when there are no subscribers.
        if sender.receiver_count() == 0 {
            debug!("changefeed poll task: no subscribers for '{db_name}', stopping");
            break;
        }

        if let Err(e) = poll_once(state.databases(), &db_name, &sender, &next_since) {
            debug!("changefeed poll error for '{db_name}': {e}");
            break;
        }
    }
}

/// Pulls the events from `next_since` on and broadcasts them. When there
/// were any, it moves `next_since` past the pull's resume cursor, so the next
/// poll neither repeats an event nor skips one a cut batch left out. An empty
/// pull leaves it where it is: staying put skips nothing, while an empty
/// pull's cursor can name an epoch that is still open (0 before the first
/// write).
fn poll_once(
    databases: &DatabaseManager,
    db_name: &str,
    sender: &broadcast::Sender<ChangeEventDto>,
    next_since: &AtomicU64,
) -> Result<(), ServiceError> {
    let since = next_since.load(Ordering::Relaxed);
    let resp = SyncService::pull(databases, db_name, since, POLL_LIMIT)?;
    if !resp.changes.is_empty() {
        next_since.fetch_max(resp.server_epoch.saturating_add(1), Ordering::Relaxed);
    }
    for event in resp.changes {
        // Ignore send errors: lagged receivers will get a `RecvError::Lagged`.
        let _ = sender.send(event);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cdc_manager() -> DatabaseManager {
        let mut mgr = DatabaseManager::new(None, false);
        mgr.set_cdc_enabled(true);
        mgr
    }

    /// Creates `n` nodes labelled `label` in one batch: one transaction,
    /// one epoch.
    fn insert_nodes(mgr: &DatabaseManager, label: &str, n: usize) {
        let rows = (0..n)
            .map(|i| {
                std::collections::HashMap::from([(
                    grafeo_common::types::PropertyKey::new("i"),
                    grafeo_common::Value::Int64(i as i64),
                )])
            })
            .collect();
        mgr.get("default")
            .unwrap()
            .db()
            .batch_create_nodes_with_props(label, rows)
            .unwrap();
    }

    fn received(rx: &mut broadcast::Receiver<ChangeEventDto>) -> Vec<ChangeEventDto> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn a_second_poll_does_not_redeliver_the_cursor_epoch() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let next_since = AtomicU64::new(0);
        insert_nodes(&mgr, "A", 2);

        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        assert_eq!(received(&mut rx).len(), 2);

        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        assert!(received(&mut rx).is_empty(), "nothing new, nothing sent");

        insert_nodes(&mgr, "B", 1);
        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        let events = received(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].labels.as_deref(), Some(&["B".to_string()][..]));
    }

    #[test]
    fn an_empty_poll_leaves_the_cursor_and_the_next_write_arrives() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let next_since = AtomicU64::new(0);

        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        assert!(received(&mut rx).is_empty());
        assert_eq!(
            next_since.load(Ordering::Relaxed),
            0,
            "nothing seen, nothing passed"
        );

        insert_nodes(&mgr, "A", 1);
        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        assert_eq!(received(&mut rx).len(), 1);
    }

    /// RDF triple events are recorded at the store's current epoch, which is
    /// 0 before the first write: an empty first poll must not pass it.
    #[cfg(feature = "sparql")]
    #[test]
    fn an_epoch_zero_triple_event_after_an_empty_poll_arrives() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let next_since = AtomicU64::new(0);

        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        assert!(received(&mut rx).is_empty());

        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute_sparql(
                "INSERT DATA { <http://example.org/a> <http://example.org/p> <http://example.org/b> }",
            )
            .unwrap();
        poll_once(&mgr, "default", &sender, &next_since).unwrap();
        let events = received(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].entity_type, "triple");
    }

    fn event_at(epoch: u64) -> ChangeEventDto {
        ChangeEventDto {
            id: epoch,
            entity_type: "node".to_string(),
            kind: "create".to_string(),
            epoch,
            timestamp: epoch,
            before: None,
            after: None,
            labels: None,
            before_labels: None,
            graph: None,
            edge_type: None,
            src_id: None,
            dst_id: None,
            triple_subject: None,
            triple_predicate: None,
            triple_object: None,
            triple_graph: None,
        }
    }

    #[tokio::test]
    async fn live_cursor_skips_what_the_subscriber_has() {
        let (sender, mut rx) = broadcast::channel(8);
        let mut cursor = LiveCursor::new(3);
        for epoch in 1..=4 {
            sender.send(event_at(epoch)).unwrap();
        }
        for expected in [3, 4] {
            match cursor.next(&mut rx).await {
                LiveItem::Change(event) => assert_eq!(event.epoch, expected),
                other => panic!("expected epoch {expected}, got {other:?}"),
            }
        }
        drop(sender);
        assert!(matches!(cursor.next(&mut rx).await, LiveItem::Closed));
    }

    #[tokio::test]
    async fn live_cursor_turns_a_lag_into_a_resumable_notice() {
        let (sender, mut rx) = broadcast::channel(4);
        let mut cursor = LiveCursor::new(5);
        sender.send(event_at(5)).unwrap();
        assert!(matches!(cursor.next(&mut rx).await, LiveItem::Change(_)));

        // Ten more than a channel of 4 holds: the oldest 6 are lost.
        for epoch in 6..16 {
            sender.send(event_at(epoch)).unwrap();
        }
        match cursor.next(&mut rx).await {
            LiveItem::Lagged(notice) => assert_eq!(
                notice,
                LaggedNotice {
                    skipped: 6,
                    // Epoch 5 may have had more events: it is not complete.
                    last_epoch: 4,
                }
            ),
            other => panic!("expected a lag, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn live_cursor_lagged_before_any_event_resumes_at_its_start() {
        let (sender, mut rx) = broadcast::channel(2);
        let mut cursor = LiveCursor::new(7);
        for epoch in 7..12 {
            sender.send(event_at(epoch)).unwrap();
        }
        match cursor.next(&mut rx).await {
            LiveItem::Lagged(notice) => {
                assert_eq!(notice.skipped, 3);
                assert_eq!(notice.last_epoch, 6);
                assert_eq!(
                    serde_json::to_value(&notice).unwrap(),
                    serde_json::json!({"skipped": 3, "last_epoch": 6})
                );
            }
            other => panic!("expected a lag, got {other:?}"),
        }
    }

    #[test]
    fn polls_deliver_a_burst_larger_than_the_poll_limit_once() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let next_since = AtomicU64::new(0);
        // Three epochs of 300 events: more than POLL_LIMIT in all.
        for label in ["A", "B", "C"] {
            insert_nodes(&mgr, label, 300);
        }

        let mut ids = Vec::new();
        for _ in 0..3 {
            poll_once(&mgr, "default", &sender, &next_since).unwrap();
            ids.extend(received(&mut rx).into_iter().map(|e| e.id));
        }
        assert_eq!(ids.len(), 900, "every event once");
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 900, "no event twice");
    }
}
