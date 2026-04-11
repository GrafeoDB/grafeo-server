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
    /// Token ID of the authenticated user who created this session.
    /// `None` when auth is disabled. When set, only requests from
    /// the same token may access this session.
    pub owner_token_id: Option<String>,
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
    ///
    /// `owner_token_id` ties the session to the authenticated token. When
    /// set, only requests bearing the same token ID may access this session.
    /// Pass `None` when auth is disabled.
    pub fn create(
        &self,
        engine_session: grafeo_engine::Session,
        db_name: &str,
        owner_token_id: Option<String>,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let now = Instant::now();
        let session = Arc::new(Mutex::new(ManagedSession {
            engine_session,
            db_name: db_name.to_string(),
            owner_token_id,
            created_at: now,
            last_used: now,
        }));
        self.sessions.insert(id.clone(), session);
        id
    }

    /// Returns a clone of the session `Arc` if the session exists, is not
    /// expired, and the caller is the session owner.
    ///
    /// `caller_token_id` is the token ID of the current request. When the
    /// session has an owner, the caller must match. Pass `None` when auth
    /// is disabled (ownership check is skipped).
    pub fn get(
        &self,
        session_id: &str,
        ttl_secs: u64,
        caller_token_id: Option<&str>,
    ) -> Option<Arc<Mutex<ManagedSession>>> {
        let entry = self.sessions.get(session_id)?;
        let arc = Arc::clone(entry.value());
        drop(entry); // release DashMap shard lock

        let mut session = arc.lock();
        if session.last_used.elapsed().as_secs() > ttl_secs {
            drop(session);
            self.sessions.remove(session_id);
            return None;
        }

        // Verify session ownership: if the session has an owner, the
        // caller must present the same token ID.
        if let Some(ref owner) = session.owner_token_id {
            match caller_token_id {
                Some(caller) if caller == owner => {}
                _ => return None,
            }
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

        let id = reg.create(sess, &db, None);
        assert!(!id.is_empty(), "create should return a non-empty id");

        let arc = reg.get(&id, 300, None);
        assert!(arc.is_some(), "get should return the session within TTL");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let reg = SessionRegistry::new();
        assert!(reg.get("no-such-id", 300, None).is_none());
    }

    #[test]
    fn get_expired_returns_none_and_removes_session() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, None);

        // Wait >1 s so elapsed().as_secs() > 0 == ttl_secs
        std::thread::sleep(std::time::Duration::from_secs(2));

        let arc = reg.get(&id, 0, None);
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
        let id = reg.create(sess, "mydb", None);

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
        let id = reg.create(sess, &db, None);
        assert!(reg.exists(&id));
    }

    #[test]
    fn exists_false_after_remove() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, None);
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
        let id1 = reg.create(s1, &db, None);
        assert_eq!(reg.active_count(), 1);

        let (s2, _) = make_session("default");
        let id2 = reg.create(s2, "other", None);
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
        let id1 = reg.create(s1, &db, None);
        let (s2, _) = make_session("default");
        let _id2 = reg.create(s2, &db, None);
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
        let id = reg.create(sess, &db, None);

        // TTL of u64::MAX: nothing should be removed
        let removed = reg.cleanup_expired(u64::MAX);
        assert_eq!(removed, 0);
        assert!(reg.exists(&id));
    }

    #[test]
    fn remove_by_database_removes_matching_sessions() {
        let reg = SessionRegistry::new();
        let (s1, _) = make_session("default");
        let id1 = reg.create(s1, "alpha", None);
        let (s2, _) = make_session("default");
        let id2 = reg.create(s2, "beta", None);
        let (s3, _) = make_session("default");
        let id3 = reg.create(s3, "alpha", None);

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
        let id = reg.create(sess, &db, None);

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

    // --- Session ownership tests ---

    #[test]
    fn get_with_matching_owner_returns_session() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, Some("token-abc".to_string()));

        let result = reg.get(&id, 300, Some("token-abc"));
        assert!(
            result.is_some(),
            "get should return the session when caller token matches owner"
        );
    }

    #[test]
    fn get_with_mismatched_owner_returns_none() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, Some("token-abc".to_string()));

        let result = reg.get(&id, 300, Some("token-xyz"));
        assert!(
            result.is_none(),
            "get should return None when caller token does not match owner"
        );
        // Session should still exist (not removed, just denied)
        assert!(
            reg.exists(&id),
            "session should not be removed on ownership mismatch"
        );
    }

    #[test]
    fn get_with_no_caller_token_on_owned_session_returns_none() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, Some("token-abc".to_string()));

        let result = reg.get(&id, 300, None);
        assert!(
            result.is_none(),
            "get should return None when session has owner but caller has no token"
        );
    }

    #[test]
    fn get_with_no_owner_ignores_caller_token() {
        let reg = SessionRegistry::new();
        let (sess, db) = make_session("default");
        let id = reg.create(sess, &db, None);

        // No caller token: should work
        let result = reg.get(&id, 300, None);
        assert!(
            result.is_some(),
            "unowned session should be accessible without token"
        );

        // With caller token: should also work (ownership check is skipped)
        let result = reg.get(&id, 300, Some("any-token"));
        assert!(
            result.is_some(),
            "unowned session should be accessible even with a caller token"
        );
    }
}
