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

use std::sync::{Arc, Weak};
use std::time::Duration;

use dashmap::DashMap;
use grafeo_engine::GrafeoDB;
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
    /// The background poll task. Locked by a subscribe and by the task's
    /// decision to stop, so no subscriber is left on a channel nobody polls.
    poller: Mutex<Poller>,
}

impl ChannelState {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            sender,
            poller: Mutex::new(Poller::Starting),
        }
    }
}

/// Where a channel's poll task stands.
enum Poller {
    /// The channel is new: its first subscriber starts the task.
    Starting,
    Running(tokio::task::JoinHandle<()>),
    /// The task stopped and the channel left the hub: a subscriber that
    /// still finds it takes a fresh one. Once the last handle on it goes,
    /// its sender drops and its receivers see `Closed`.
    Retired,
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
    /// full history). Filter what the receiver yields with a [`LiveCursor`]
    /// at the same epoch. The receiver only yields events broadcast after
    /// the subscription: fetch the ones before it with `SyncService::pull()`.
    ///
    /// The first subscriber of a database starts its poll task at
    /// `since_epoch`, but never past the first epoch that can still gain
    /// events (the current epoch while none of its events are recorded, else
    /// the next one), so a client that asks for a far epoch cannot hold the
    /// feed back for everyone. Later subscribers leave the
    /// polls where they are: moving them ahead would skip events the
    /// subscribers already listening still wait for.
    ///
    /// When the feed stops (the database was dropped, restored or replaced
    /// by a new one of the same name, or its CDC turned off), the receiver
    /// yields `Closed`.
    pub fn subscribe(
        &self,
        db_name: &str,
        since_epoch: u64,
        state: ServiceState,
    ) -> broadcast::Receiver<ChangeEventDto> {
        loop {
            let channel = Arc::clone(
                &self
                    .channels
                    .entry(db_name.to_string())
                    .or_insert_with(|| Arc::new(ChannelState::new())),
            );
            let mut poller = channel.poller.lock();
            match &*poller {
                Poller::Retired => continue,
                Poller::Running(task) if task.is_finished() => {
                    // The task ended without retiring the channel (it
                    // panicked): retire it, so its subscribers see the end
                    // instead of waiting on a feed nobody polls.
                    self.retire(db_name, &channel, &mut poller);
                    continue;
                }
                Poller::Starting | Poller::Running(_) => {}
            }
            // Subscribe before the task can look for subscribers.
            let receiver = channel.sender.subscribe();
            if matches!(*poller, Poller::Starting) {
                *poller = Poller::Running(tokio::spawn(poll_task(
                    self.clone(),
                    db_name.to_string(),
                    Arc::clone(&channel),
                    first_cursor(state.databases(), db_name, since_epoch),
                    state,
                )));
            }
            return receiver;
        }
    }

    /// Takes `channel` out of the hub. The caller holds its poller lock.
    fn retire(&self, db_name: &str, channel: &Arc<ChannelState>, poller: &mut Poller) {
        *poller = Poller::Retired;
        self.channels
            .remove_if(db_name, |_, current| Arc::ptr_eq(current, channel));
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
    /// it does not count as delivered in full: the resume point is that
    /// epoch, or the subscriber's own `since` before any event was sent.
    fn lagged(&self, skipped: u64) -> LaggedNotice {
        LaggedNotice {
            skipped,
            since: self.last_sent.unwrap_or(self.since),
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
/// Resume with a pull or a new subscription at `since` (`?since=<since>`):
/// it is inclusive, so epoch 0 is never skipped. Events of that epoch
/// already received may arrive again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaggedNotice {
    /// Events the subscriber lost.
    pub skipped: u64,
    /// The first epoch not delivered in full: every event before it was.
    pub since: u64,
}

// ---------------------------------------------------------------------------
// Background poll task
// ---------------------------------------------------------------------------

/// Where a hub's polls stand.
struct HubCursor {
    /// The `since` of the next poll.
    next_since: u64,
    /// The database instance the polls read, once known. A restore swaps
    /// in a new instance and a drop and recreate makes one; either is
    /// another history, which the cursor does not apply to.
    origin: Option<Weak<GrafeoDB>>,
}

impl HubCursor {
    fn new(next_since: u64, origin: Option<Weak<GrafeoDB>>) -> Self {
        Self { next_since, origin }
    }
}

/// Where a new feed of `db_name` starts for its first subscriber, which asked
/// for `since_epoch` on: there, but never past the first epoch that can still
/// gain events. That is the current epoch while none of its events are in the
/// log (the engine publishes an epoch before it records its events), else
/// the next one: where a pull from the current epoch resumes. Starting lower
/// would broadcast events every subscriber has again (a whole epoch, which
/// can be more than the channel holds); starting higher would pass events
/// recorded after the subscription.
fn first_cursor(databases: &DatabaseManager, db_name: &str, since_epoch: u64) -> HubCursor {
    let Some(entry) = databases.get(db_name) else {
        // The first poll fails and closes the feed.
        return HubCursor::new(0, None);
    };
    let db = entry.db();
    let current = db.current_epoch().0;
    // As in the polls, only a pull with events moves past an epoch: an empty
    // one can name the current epoch as done (0 before the first write).
    let open = match SyncService::pull_from(&db, current, 1) {
        Ok(resp) if !resp.changes.is_empty() => resp.server_epoch.saturating_add(1),
        _ => current,
    };
    HubCursor::new(since_epoch.min(open), Some(Arc::downgrade(&db)))
}

/// Polls the CDC log of `db_name` from `cursor` on and broadcasts what it
/// finds on `channel`, until the channel has no subscribers or a poll fails.
/// Either way it retires the channel; after a failure that closes the
/// subscribers still on it.
async fn poll_task(
    hub: ChangeHub,
    db_name: String,
    channel: Arc<ChannelState>,
    mut cursor: HubCursor,
    state: ServiceState,
) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        // Stop when there are no subscribers. Decided under the poller lock:
        // a subscriber either is counted here or finds the channel retired
        // and starts a fresh one.
        if channel.sender.receiver_count() == 0 {
            let mut poller = channel.poller.lock();
            if channel.sender.receiver_count() == 0 {
                debug!("changefeed poll task: no subscribers for '{db_name}', stopping");
                hub.retire(&db_name, &channel, &mut poller);
                return;
            }
        }

        if let Err(e) = poll_once(state.databases(), &db_name, &channel.sender, &mut cursor) {
            debug!("changefeed poll error for '{db_name}': {e}; closing its subscribers");
            hub.retire(&db_name, &channel, &mut channel.poller.lock());
            return;
        }
    }
}

/// Pulls the events from `cursor.next_since` on and broadcasts them. When
/// there were any, it moves the cursor past the pull's resume cursor, so the
/// next poll neither repeats an event nor skips one a cut batch left out. An
/// empty pull leaves it where it is: staying put skips nothing, while an
/// empty pull's cursor can name an epoch that is still open (0 before the
/// first write).
///
/// A database that is no longer the instance the cursor belongs to (restored,
/// or dropped and created again between two polls) fails the poll, and so
/// does one whose epoch went back below the cursor: the cursor means nothing
/// in that history, and staying on it would stall the subscribers silently.
fn poll_once(
    databases: &DatabaseManager,
    db_name: &str,
    sender: &broadcast::Sender<ChangeEventDto>,
    cursor: &mut HubCursor,
) -> Result<(), ServiceError> {
    let db = databases.get_available(db_name)?.db();
    let replaced = || {
        ServiceError::Unavailable(format!(
            "database '{db_name}' was restored or replaced; its change feed ends"
        ))
    };
    match &cursor.origin {
        Some(origin) if !std::ptr::eq(origin.as_ptr(), Arc::as_ptr(&db)) => {
            return Err(replaced());
        }
        Some(_) => {}
        None => cursor.origin = Some(Arc::downgrade(&db)),
    }
    if cursor.next_since > db.current_epoch().0.saturating_add(1) {
        return Err(replaced());
    }
    let resp = SyncService::pull_from(&db, cursor.next_since, POLL_LIMIT)?;
    if !resp.changes.is_empty() {
        cursor.next_since = cursor.next_since.max(resp.server_epoch.saturating_add(1));
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
        let mut cursor = HubCursor::new(0, None);
        insert_nodes(&mgr, "A", 2);

        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        assert_eq!(received(&mut rx).len(), 2);

        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        assert!(received(&mut rx).is_empty(), "nothing new, nothing sent");

        insert_nodes(&mgr, "B", 1);
        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        let events = received(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].labels.as_deref(), Some(&["B".to_string()][..]));
    }

    #[test]
    fn an_empty_poll_leaves_the_cursor_and_the_next_write_arrives() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let mut cursor = HubCursor::new(0, None);

        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        assert!(received(&mut rx).is_empty());
        assert_eq!(cursor.next_since, 0, "nothing seen, nothing passed");

        insert_nodes(&mgr, "A", 1);
        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        assert_eq!(received(&mut rx).len(), 1);
    }

    /// RDF triple events are recorded at the store's current epoch, which is
    /// 0 before the first write: an empty first poll must not pass it.
    #[cfg(feature = "sparql")]
    #[test]
    fn an_epoch_zero_triple_event_after_an_empty_poll_arrives() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let mut cursor = HubCursor::new(0, None);

        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        assert!(received(&mut rx).is_empty());

        mgr.get("default")
            .unwrap()
            .db()
            .session()
            .execute_sparql(
                "INSERT DATA { <http://example.org/a> <http://example.org/p> <http://example.org/b> }",
            )
            .unwrap();
        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
        let events = received(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].entity_type, "triple");
    }

    /// An in-memory service whose `default` database records CDC.
    fn cdc_state() -> ServiceState {
        let state = ServiceState::new_in_memory(300);
        state
            .databases()
            .get("default")
            .unwrap()
            .db()
            .set_cdc_enabled(true);
        state
    }

    /// Creates the in-memory database `name` with CDC on.
    fn create_cdc_database(state: &ServiceState, name: &str) {
        state
            .databases()
            .create(&crate::types::CreateDatabaseRequest {
                name: name.to_string(),
                database_type: crate::types::DatabaseType::Lpg,
                storage_mode: crate::types::StorageMode::InMemory,
                options: crate::types::DatabaseOptions::default(),
                schema_file: None,
                schema_filename: None,
            })
            .unwrap();
        state
            .databases()
            .get(name)
            .unwrap()
            .db()
            .set_cdc_enabled(true);
    }

    async fn recv(
        rx: &mut broadcast::Receiver<ChangeEventDto>,
    ) -> Result<ChangeEventDto, RecvError> {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("nothing within 10 s")
    }

    #[tokio::test]
    async fn a_far_ahead_subscriber_does_not_hold_back_the_others() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let db = state.databases().get("default").unwrap().db();

        let _far = hub.subscribe("default", u64::MAX, state.clone());
        let mut rx = hub.subscribe("default", 0, state.clone());
        let id = db.create_node(&["Live"]).unwrap().as_u64();
        assert_eq!(recv(&mut rx).await.unwrap().id, id);
    }

    #[tokio::test]
    async fn a_later_subscriber_ahead_of_the_hub_does_not_skip_events_for_earlier_ones() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let db = state.databases().get("default").unwrap().db();
        let first = db.create_node(&["First"]).unwrap().as_u64();

        // The poll task has not run yet (single-threaded runtime): the hub
        // is still at epoch 0 when the second subscriber asks for the next
        // epoch on.
        let mut early = hub.subscribe("default", 0, state.clone());
        let mut late = hub.subscribe("default", db.current_epoch().0 + 1, state.clone());
        assert_eq!(recv(&mut early).await.unwrap().id, first);

        let second = db.create_node(&["Second"]).unwrap().as_u64();
        assert_eq!(recv(&mut early).await.unwrap().id, second);
        // `late` filters what it already has with its LiveCursor.
        let mut cursor = LiveCursor::new(db.current_epoch().0);
        match cursor.next(&mut late).await {
            LiveItem::Change(event) => assert_eq!(event.id, second),
            other => panic!("expected the second node, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropping_the_database_closes_its_subscribers() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        create_cdc_database(&state, "gone");

        let mut rx = hub.subscribe("gone", 0, state.clone());
        state.databases().delete("gone").unwrap();
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
        assert!(!hub.channels.contains_key("gone"), "the channel is retired");

        // A subscription to the missing database closes too.
        let mut again = hub.subscribe("gone", 0, state.clone());
        assert!(matches!(recv(&mut again).await, Err(RecvError::Closed)));
    }

    /// Subscribes to `name`, then waits until the hub has moved past one
    /// write, so its cursor is ahead of a fresh database's epoch.
    async fn subscribed_past_a_write(
        hub: &ChangeHub,
        state: &ServiceState,
        name: &str,
    ) -> broadcast::Receiver<ChangeEventDto> {
        let mut rx = hub.subscribe(name, 0, state.clone());
        let db = state.databases().get(name).unwrap().db();
        db.create_node(&["Old"]).unwrap();
        db.create_node(&["Old"]).unwrap();
        recv(&mut rx).await.unwrap();
        recv(&mut rx).await.unwrap();
        rx
    }

    /// A fresh in-memory database with CDC on: epoch 0, another history.
    fn fresh_cdc_db() -> Arc<GrafeoDB> {
        let db = GrafeoDB::new_in_memory();
        db.set_cdc_enabled(true);
        Arc::new(db)
    }

    #[tokio::test]
    async fn a_restore_between_two_polls_closes_the_subscribers() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        create_cdc_database(&state, "restored");
        let mut rx = subscribed_past_a_write(&hub, &state, "restored").await;

        // What a restore does, between two polls (the test runtime runs the
        // poll task only when the test awaits).
        state
            .databases()
            .get("restored")
            .unwrap()
            .swap_db(fresh_cdc_db());
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
    }

    #[tokio::test]
    async fn a_replacement_at_or_past_the_cursor_epoch_still_closes_the_subscribers() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        create_cdc_database(&state, "swapped");
        let mut rx = subscribed_past_a_write(&hub, &state, "swapped").await;

        // The replacement has as many epochs as the original and one more:
        // the cursor is not ahead of it, so the epoch rule lets it through,
        // and without the instance check the hub would send its third node.
        let replacement = fresh_cdc_db();
        for _ in 0..3 {
            replacement.create_node(&["New"]).unwrap();
        }
        state
            .databases()
            .get("swapped")
            .unwrap()
            .swap_db(replacement);
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
    }

    #[tokio::test]
    async fn a_database_recreated_between_two_polls_closes_the_subscribers() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        create_cdc_database(&state, "again");
        let mut rx = subscribed_past_a_write(&hub, &state, "again").await;

        state.databases().delete("again").unwrap();
        create_cdc_database(&state, "again");
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
    }

    #[cfg(feature = "compact-store")]
    #[tokio::test]
    async fn compaction_succeeds_with_a_live_subscriber_and_ends_its_feed() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        create_cdc_database(&state, "columnar");
        let mut rx = subscribed_past_a_write(&hub, &state, "columnar").await;

        crate::admin::AdminService::compact(state.databases(), "columnar")
            .await
            .expect("a live feed does not block compaction");
        assert_eq!(
            state
                .databases()
                .get("columnar")
                .unwrap()
                .metadata
                .storage_mode,
            "compact"
        );
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
    }

    #[test]
    fn a_cursor_past_the_next_epoch_ends_the_feed() {
        let mgr = cdc_manager();
        let (sender, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        // The epoch went back below the cursor (a database at epoch 0).
        let mut cursor = HubCursor::new(2, None);
        assert!(matches!(
            poll_once(&mgr, "default", &sender, &mut cursor),
            Err(ServiceError::Unavailable(_))
        ));
        // One past the current epoch is where a caught-up cursor stands.
        let mut cursor = HubCursor::new(1, None);
        poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
    }

    #[test]
    fn a_new_feed_starts_at_the_first_epoch_that_can_still_gain_events() {
        let mgr = cdc_manager();
        let db = mgr.get("default").unwrap().db();
        let start = |since| first_cursor(&mgr, "default", since).next_since;
        assert_eq!(start(u64::MAX), 0, "epoch 0 is open on a new database");

        db.create_node(&["A"]).unwrap();
        let written = db.current_epoch().0;
        assert_eq!(
            start(u64::MAX),
            written + 1,
            "the write's epoch is complete"
        );
        assert_eq!(start(1), 1, "a lower since is kept");

        // A failed direct call uses up an epoch and records nothing: that
        // epoch looks like one whose write has not recorded its events.
        assert!(
            db.set_node_property(grafeo_common::types::NodeId::new(424_242), "x", 1i64.into())
                .is_err()
        );
        let open = db.current_epoch().0;
        assert_eq!(open, written + 1);
        assert_eq!(
            start(u64::MAX),
            open,
            "an epoch still filling is not passed"
        );

        assert_eq!(first_cursor(&mgr, "missing", 9).next_since, 0);
    }

    /// RDF triple writes record their events at the current epoch, which is
    /// what an epoch still filling looks like to the feed.
    #[cfg(feature = "sparql")]
    #[tokio::test]
    async fn a_far_first_subscriber_does_not_pass_an_epoch_still_filling() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let db = state.databases().get("default").unwrap().db();
        db.create_node(&["A"]).unwrap();
        assert!(
            db.set_node_property(grafeo_common::types::NodeId::new(424_242), "x", 1i64.into())
                .is_err()
        );

        let _far = hub.subscribe("default", u64::MAX, state.clone());
        let mut rx = hub.subscribe("default", 0, state.clone());
        db.session()
            .execute_sparql(
                "INSERT DATA { <http://example.org/a> <http://example.org/p> <http://example.org/b> }",
            )
            .unwrap();
        let event = recv(&mut rx).await.unwrap();
        assert_eq!(event.entity_type, "triple");
    }

    #[tokio::test]
    async fn a_new_feed_does_not_broadcast_a_complete_epoch_again() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let db = state.databases().get("default").unwrap().db();
        db.create_node(&["Before"]).unwrap();

        let mut rx = hub.subscribe("default", u64::MAX, state.clone());
        let id = db.create_node(&["After"]).unwrap().as_u64();
        assert_eq!(
            recv(&mut rx).await.unwrap().id,
            id,
            "first comes the new write"
        );
    }

    #[tokio::test]
    async fn turning_cdc_off_closes_the_subscribers() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let mut rx = hub.subscribe("default", 0, state.clone());
        state
            .databases()
            .get("default")
            .unwrap()
            .db()
            .set_cdc_enabled(false);
        assert!(matches!(recv(&mut rx).await, Err(RecvError::Closed)));
    }

    #[tokio::test]
    async fn a_subscriber_after_the_hub_stopped_gets_a_fresh_feed() {
        let state = cdc_state();
        let hub = ChangeHub::new();
        let db = state.databases().get("default").unwrap().db();

        drop(hub.subscribe("default", 0, state.clone()));
        // With no subscriber left the task retires the channel.
        tokio::time::timeout(Duration::from_secs(10), async {
            while hub.channels.contains_key("default") {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .expect("the idle hub stops");

        let mut rx = hub.subscribe("default", 0, state.clone());
        let id = db.create_node(&["Back"]).unwrap().as_u64();
        assert_eq!(recv(&mut rx).await.unwrap().id, id);
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
                    since: 5,
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
                assert_eq!(notice.since, 7);
                assert_eq!(
                    serde_json::to_value(&notice).unwrap(),
                    serde_json::json!({"skipped": 3, "since": 7})
                );
            }
            other => panic!("expected a lag, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_lag_at_epoch_zero_resumes_at_epoch_zero() {
        let (sender, mut rx) = broadcast::channel(2);
        let mut cursor = LiveCursor::new(0);
        for _ in 0..5 {
            sender.send(event_at(0)).unwrap();
        }
        match cursor.next(&mut rx).await {
            LiveItem::Lagged(notice) => assert_eq!(notice.since, 0),
            other => panic!("expected a lag, got {other:?}"),
        }

        // The same after events of epoch 0 were sent.
        let (sender, mut rx) = broadcast::channel(2);
        let mut cursor = LiveCursor::new(0);
        sender.send(event_at(0)).unwrap();
        assert!(matches!(cursor.next(&mut rx).await, LiveItem::Change(_)));
        for _ in 0..5 {
            sender.send(event_at(0)).unwrap();
        }
        match cursor.next(&mut rx).await {
            LiveItem::Lagged(notice) => assert_eq!(notice.since, 0),
            other => panic!("expected a lag, got {other:?}"),
        }
    }

    #[test]
    fn polls_deliver_a_burst_larger_than_the_poll_limit_once() {
        let mgr = cdc_manager();
        let (sender, mut rx) = broadcast::channel(CHANNEL_CAPACITY);
        let mut cursor = HubCursor::new(0, None);
        // Three epochs of 300 events: more than POLL_LIMIT in all.
        for label in ["A", "B", "C"] {
            insert_nodes(&mgr, label, 300);
        }

        let mut ids = Vec::new();
        for _ in 0..3 {
            poll_once(&mgr, "default", &sender, &mut cursor).unwrap();
            ids.extend(received(&mut rx).into_iter().map(|e| e.id));
        }
        assert_eq!(ids.len(), 900, "every event once");
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 900, "no event twice");
    }
}
