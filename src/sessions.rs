//! Transaction session manager with TTL-based expiry.
//!
//! Maps opaque session tokens to engine sessions with open transactions,
//! enabling multi-statement transactions over stateless HTTP.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::Mutex;
use uuid::Uuid;

/// A registered transaction session holding an engine session with an open transaction.
pub struct Session {
    pub engine_session: grafeo_engine::Session,
    last_used: Instant,
}

/// Thread-safe registry of open transaction sessions.
///
/// Each entry wraps the session in `Arc<Mutex<_>>` so it can be passed into
/// `spawn_blocking` without holding the DashMap shard lock during I/O.
pub struct SessionManager {
    sessions: DashMap<String, Arc<Mutex<Session>>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    /// Creates a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    /// Registers an engine session (with an already-open transaction) and returns its ID.
    pub fn create(&self, engine_session: grafeo_engine::Session) -> String {
        let id = Uuid::new_v4().to_string();
        let session = Arc::new(Mutex::new(Session {
            engine_session,
            last_used: Instant::now(),
        }));
        self.sessions.insert(id.clone(), session);
        id
    }

    /// Returns a clone of the session Arc if the session exists and is not expired.
    /// Touches the session timestamp on success.
    pub fn get(&self, session_id: &str, ttl_secs: u64) -> Option<Arc<Mutex<Session>>> {
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
}
