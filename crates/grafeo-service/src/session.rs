//! Unified session registry for transaction management.
//!
//! Replaces the dual-registry pattern (per-database `SessionManager` +
//! `session_db_map` in `DatabaseManager`) with a single global registry.
//! Each session tracks which database it belongs to.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::Mutex;
use uuid::Uuid;

/// A managed transaction session with its owning database name.
pub struct ManagedSession {
    /// The underlying engine session with an open transaction.
    pub engine_session: grafeo_engine::Session,
    /// Name of the database this session belongs to.
    pub db_name: String,
    /// When the session was created.
    pub created_at: Instant,
    /// Last time the session was accessed.
    last_used: Instant,
}

/// Thread-safe global registry of open transaction sessions.
///
/// Single `DashMap` replaces the old dual-map pattern:
/// - `SessionManager` (per-database) → merged here
/// - `session_db_map` (DatabaseManager) → `db_name` field on `ManagedSession`
///
/// Benefits:
/// - One lookup instead of three for transaction resolution
/// - Session cleanup is a single loop
/// - No stale session-to-db mappings possible
pub struct SessionRegistry {
    sessions: DashMap<String, Arc<Mutex<ManagedSession>>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    /// Creates a new empty session registry.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    /// Registers an engine session (with an already-open transaction) and
    /// returns a UUID session ID.
    pub fn create(&self, engine_session: grafeo_engine::Session, db_name: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let now = Instant::now();
        let session = Arc::new(Mutex::new(ManagedSession {
            engine_session,
            db_name: db_name.to_string(),
            created_at: now,
            last_used: now,
        }));
        self.sessions.insert(id.clone(), session);
        id
    }

    /// Returns a clone of the session `Arc` if the session exists and is
    /// not expired. Touches the session timestamp on success.
    pub fn get(&self, session_id: &str, ttl_secs: u64) -> Option<Arc<Mutex<ManagedSession>>> {
        let entry = self.sessions.get(session_id)?;
        let arc = Arc::clone(entry.value());
        drop(entry); // release DashMap shard lock

        let mut session = arc.lock();
        if session.last_used.elapsed().as_secs() > ttl_secs {
            drop(session);
            self.sessions.remove(session_id);
            return None;
        }
        session.last_used = Instant::now();
        drop(session);

        Some(arc)
    }

    /// Returns the database name for a session, if it exists.
    pub fn db_name(&self, session_id: &str) -> Option<String> {
        let entry = self.sessions.get(session_id)?;
        let arc = Arc::clone(entry.value());
        drop(entry);
        let session = arc.lock();
        Some(session.db_name.clone())
    }

    /// Removes a session.
    pub fn remove(&self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    /// Removes all expired sessions. Returns the count removed.
    pub fn cleanup_expired(&self, ttl_secs: u64) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|_, session| {
            let s = session.lock();
            s.last_used.elapsed().as_secs() <= ttl_secs
        });
        before - self.sessions.len()
    }

    /// Returns whether a session exists (regardless of expiry).
    pub fn exists(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Returns the number of active sessions.
    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Removes all sessions belonging to a given database.
    pub fn remove_by_database(&self, db_name: &str) {
        self.sessions.retain(|_, session| {
            let s = session.lock();
            s.db_name != db_name
        });
    }
}
