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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseManager;

    fn make_session(db_name: &str) -> (grafeo_engine::Session, String) {
        let mgr = DatabaseManager::new(None, false);
        let entry = mgr.get(db_name).unwrap();
        (entry.db().session(), db_name.to_string())
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");

        let id = reg.create(sess, &db);
        assert!(!id.is_empty(), "create should return a non-empty id");

        let arc = reg.get(&id, 300);
        assert!(arc.is_some(), "get should return the session within TTL");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let reg = SessionRegistry::new();
        assert!(reg.get("no-such-id", 300).is_none());
    }

    #[test]
    fn get_expired_returns_none_and_removes_session() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db);

        // Wait >1 s so elapsed().as_secs() > 0 == ttl_secs
        std::thread::sleep(std::time::Duration::from_secs(2));

        let arc = reg.get(&id, 0);
        assert!(arc.is_none(), "expired session should return None");
        assert!(
            !reg.exists(&id),
            "expired session should be removed from registry"
        );
    }

    #[test]
    fn db_name_returns_correct_name() {
        let reg = SessionRegistry::new();
        let (sess, _) = make_session("default");
        let id = reg.create(sess, "mydb");

        assert_eq!(reg.db_name(&id).as_deref(), Some("mydb"));
    }

    #[test]
    fn db_name_nonexistent_returns_none() {
        let reg = SessionRegistry::new();
        assert!(reg.db_name("ghost").is_none());
    }

    #[test]
    fn exists_true_after_create() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db);
        assert!(reg.exists(&id));
    }

    #[test]
    fn exists_false_after_remove() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db);
        reg.remove(&id);
        assert!(!reg.exists(&id));
    }

    #[test]
    fn exists_false_for_unknown() {
        let reg = SessionRegistry::new();
        assert!(!reg.exists("unknown"));
    }

    #[test]
    fn active_count_tracks_creates_and_removes() {
        let reg = SessionRegistry::new();
        assert_eq!(reg.active_count(), 0);

        let (s1, db) = make_session("default");
        let id1 = reg.create(s1, &db);
        assert_eq!(reg.active_count(), 1);

        let (s2, _) = make_session("default");
        let id2 = reg.create(s2, "other");
        assert_eq!(reg.active_count(), 2);

        reg.remove(&id1);
        assert_eq!(reg.active_count(), 1);

        reg.remove(&id2);
        assert_eq!(reg.active_count(), 0);
    }

    #[test]
    fn cleanup_expired_removes_stale_sessions() {
        let reg = SessionRegistry::new();
        let (s1, db) = make_session("default");
        let id1 = reg.create(s1, &db);
        let (s2, _) = make_session("default");
        let _id2 = reg.create(s2, &db);
        assert_eq!(reg.active_count(), 2);

        // Wait so both sessions are older than 0 s TTL
        std::thread::sleep(std::time::Duration::from_secs(2));

        let removed = reg.cleanup_expired(0);
        assert_eq!(removed, 2, "both sessions should be cleaned up");
        assert_eq!(reg.active_count(), 0);
        assert!(!reg.exists(&id1));
    }

    #[test]
    fn cleanup_expired_keeps_fresh_sessions() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db);

        // TTL of u64::MAX: nothing should be removed
        let removed = reg.cleanup_expired(u64::MAX);
        assert_eq!(removed, 0);
        assert!(reg.exists(&id));
    }

    #[test]
    fn remove_by_database_removes_matching_sessions() {
        let reg = SessionRegistry::new();
        let (s1, _) = make_session("default");
        let id1 = reg.create(s1, "alpha");
        let (s2, _) = make_session("default");
        let id2 = reg.create(s2, "beta");
        let (s3, _) = make_session("default");
        let id3 = reg.create(s3, "alpha");

        reg.remove_by_database("alpha");

        assert!(!reg.exists(&id1), "alpha session 1 should be removed");
        assert!(!reg.exists(&id3), "alpha session 3 should be removed");
        assert!(reg.exists(&id2), "beta session should remain");
        assert_eq!(reg.active_count(), 1);
    }

    #[test]
    fn remove_by_database_noop_when_no_match() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db);

        reg.remove_by_database("nonexistent");
        assert!(
            reg.exists(&id),
            "unmatched remove_by_database must not touch other sessions"
        );
    }

    #[test]
    fn default_creates_empty_registry() {
        let reg = SessionRegistry::default();
        assert_eq!(reg.active_count(), 0);
    }
}
