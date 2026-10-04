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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::broadcast;
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
    /// (`server_epoch`) of the last pull, 0 before any.
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

/// Pulls the events from `next_since` on, broadcasts them, and moves
/// `next_since` past the pull's resume cursor, so the next poll neither
/// repeats an event nor skips one a cut batch left out.
fn poll_once(
    databases: &DatabaseManager,
    db_name: &str,
    sender: &broadcast::Sender<ChangeEventDto>,
    next_since: &AtomicU64,
) -> Result<(), ServiceError> {
    let since = next_since.load(Ordering::Relaxed);
    let resp = SyncService::pull(databases, db_name, since, POLL_LIMIT)?;
    next_since.fetch_max(resp.server_epoch.saturating_add(1), Ordering::Relaxed);
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
