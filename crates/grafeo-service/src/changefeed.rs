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
    last_epoch: Arc<AtomicU64>,
    /// Handle to the background poll task. `None` means no task is running.
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ChannelState {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            sender,
            last_epoch: Arc::new(AtomicU64::new(0)),
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
    /// Historical events since `since_epoch` must be fetched separately via
    /// `SyncService::pull()` — the receiver only yields events that arrive
    /// after the subscription point.
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

        // Advance epoch to at least `since_epoch` so the poll task starts from here.
        channel.last_epoch.fetch_max(since_epoch, Ordering::Relaxed);

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
            let last_epoch = Arc::clone(&channel.last_epoch);
            let handle = tokio::spawn(poll_task(db, sender, last_epoch, state));
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
    last_epoch: Arc<AtomicU64>,
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

        let since = last_epoch.load(Ordering::Relaxed);

        let resp = match SyncService::pull(state.databases(), &db_name, since, POLL_LIMIT) {
            Ok(r) => r,
            Err(e) => {
                debug!("changefeed poll error for '{db_name}': {e}");
                break;
            }
        };

        if !resp.changes.is_empty() {
            last_epoch.store(resp.server_epoch, Ordering::Relaxed);
            for event in resp.changes {
                // Ignore send errors — lagged receivers will get a `RecvError::Lagged`.
                let _ = sender.send(event);
            }
        }
    }
}
