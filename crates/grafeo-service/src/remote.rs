//! The engine sessions remote clients' statements run in.
//!
//! Every transport (HTTP, the WebSocket, GWP, Bolt) opens the engine session
//! for a client's statements through [`remote_session`], so what a remote
//! client may do is decided in one place: the identity its credentials gave
//! (anonymous admin when the server does not authenticate), capped to
//! read-only on a read-only server or replica. The server's own work
//! (backups, the change feed, replication apply, schema loading) uses
//! `GrafeoDB::session` directly and is not a remote session.

use grafeo_engine::auth::Identity;
use grafeo_engine::{GrafeoDB, Session};

use crate::auth::cap_identity_read_only;

/// Opens the engine session for a remote client acting as `identity`, or as
/// the anonymous admin when the server does not authenticate (`None`),
/// capped to read-only when `read_only`.
pub fn remote_session(db: &GrafeoDB, identity: Option<Identity>, read_only: bool) -> Session {
    let identity = cap_identity_read_only(identity.unwrap_or_else(Identity::anonymous), read_only);
    db.session_with_identity(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_engine::auth::Role;

    fn db() -> GrafeoDB {
        GrafeoDB::new_in_memory()
    }

    fn writes(session: &Session) -> bool {
        session.execute("INSERT (:Probe {v: 1})").is_ok()
    }

    #[test]
    fn without_authentication_a_client_is_the_anonymous_admin() {
        let db = db();
        let s = remote_session(&db, None, false);
        assert!(writes(&s));
        assert!(
            s.execute("CREATE GRAPH TYPE probe_t {(n :Probe)}").is_ok(),
            "admin may run DDL"
        );
    }

    #[test]
    fn a_read_only_server_caps_every_client() {
        let db = db();
        assert!(!writes(&remote_session(&db, None, true)));
        let admin = Identity::new("alice", [Role::Admin]);
        assert!(!writes(&remote_session(&db, Some(admin), true)));
    }

    #[test]
    fn a_client_acts_as_its_identity() {
        let db = db();
        assert!(writes(&remote_session(
            &db,
            Some(Identity::new("w", [Role::ReadWrite])),
            false
        )));
        assert!(!writes(&remote_session(
            &db,
            Some(Identity::new("r", [Role::ReadOnly])),
            false
        )));
    }
}
