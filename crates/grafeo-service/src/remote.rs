//! The engine sessions remote clients' statements run in.
//!
//! Every transport (HTTP, the WebSocket, GWP, Bolt) opens the engine session
//! for a client's statements through [`remote_session`], so what a remote
//! client may do is decided in one place: the identity its credentials gave
//! (anonymous admin when the server does not authenticate), capped to
//! read-only on a read-only server or replica, and without access outside the
//! database unless the operator allows it. The server's own work (backups,
//! the change feed, replication apply, schema loading) uses
//! `GrafeoDB::session` directly and is not a remote session.

use grafeo_engine::auth::Identity;
use grafeo_engine::{GrafeoDB, Session};

use crate::auth::cap_identity_read_only;

/// What the server allows every remote client, whatever its credentials: the
/// server-wide limits [`remote_session`] applies on top of the identity.
///
/// The default is the safe one: writable (a read-only server sets
/// `read_only`) and without external access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemotePolicy {
    /// Every client is capped to read-only (a read-only server or a replica).
    pub read_only: bool,
    /// Statements may reach outside the database (`LOAD CSV`,
    /// `grafeo.load_nodes_csv`, the other import and export procedures): set
    /// by `--allow-external-access`. Even then only an admin identity may, as
    /// the engine's role rule says; without it nobody may, admin included.
    pub external_access: bool,
}

/// Opens the engine session for a remote client acting as `identity`, or as
/// the anonymous admin when the server does not authenticate (`None`),
/// within `policy`.
pub fn remote_session(db: &GrafeoDB, identity: Option<Identity>, policy: RemotePolicy) -> Session {
    let identity = cap_identity_read_only(
        identity.unwrap_or_else(Identity::anonymous),
        policy.read_only,
    );
    // With external access allowed, the identity's role decides (admin only);
    // without it, nobody reaches outside the database.
    let identity = if policy.external_access {
        identity
    } else {
        identity.with_external_access(false)
    };
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
        let s = remote_session(&db, None, RemotePolicy::default());
        assert!(writes(&s));
        assert!(
            s.execute("CREATE GRAPH TYPE probe_t {(n :Probe)}").is_ok(),
            "admin may run DDL"
        );
    }

    #[test]
    fn a_read_only_server_caps_every_client() {
        let db = db();
        let read_only = RemotePolicy {
            read_only: true,
            ..RemotePolicy::default()
        };
        assert!(!writes(&remote_session(&db, None, read_only)));
        let admin = Identity::new("alice", [Role::Admin]);
        assert!(!writes(&remote_session(&db, Some(admin), read_only)));
    }

    #[test]
    fn a_client_acts_as_its_identity() {
        let db = db();
        assert!(writes(&remote_session(
            &db,
            Some(Identity::new("w", [Role::ReadWrite])),
            RemotePolicy::default()
        )));
        assert!(!writes(&remote_session(
            &db,
            Some(Identity::new("r", [Role::ReadOnly])),
            RemotePolicy::default()
        )));
    }

    #[test]
    fn the_default_policy_is_writable_without_external_access() {
        let policy = RemotePolicy::default();
        assert!(!policy.read_only);
        assert!(!policy.external_access);
    }

    /// A CSV file of two people, its path written as a query writes it.
    fn people_csv() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("people.csv");
        std::fs::write(&path, "id,name\n1,Alix\n2,Gus\n").unwrap();
        let text = path.to_string_lossy().replace('\\', "/");
        (dir, text)
    }

    /// Whether `session` may read the server's file at `path`, by every way a
    /// query can: GQL's load procedure and, with Cypher, `LOAD CSV`. Panics
    /// when the two disagree, or when a refusal is not the external-access one.
    fn reads_files(session: &Session, path: &str) -> bool {
        let procedure = session.execute(&format!(
            "CALL grafeo.load_nodes_csv({{path: '{path}', labels: ['Leak'], key: 'id'}})"
        ));
        let results = [procedure.map(|_| ())];
        #[cfg(feature = "cypher")]
        let results = [
            results[0].clone(),
            session
                .execute_cypher(&format!(
                    "LOAD CSV WITH HEADERS FROM '{path}' AS row RETURN row.name"
                ))
                .map(|_| ()),
        ];
        let allowed: Vec<bool> = results
            .iter()
            .map(|r| match r {
                Ok(()) => true,
                Err(e) => {
                    assert_eq!(
                        e.error_code().as_str(),
                        "GRAFEO-Q008",
                        "a refusal is the external-access one: {e}"
                    );
                    false
                }
            })
            .collect();
        assert!(
            allowed.iter().all(|a| *a == allowed[0]),
            "every way agrees: {allowed:?}"
        );
        allowed[0]
    }

    #[test]
    fn by_default_no_remote_client_reads_server_files_admin_included() {
        let db = db();
        let (_dir, path) = people_csv();
        let policy = RemotePolicy::default();
        assert!(!reads_files(&remote_session(&db, None, policy), &path));
        for role in [Role::Admin, Role::ReadWrite, Role::ReadOnly] {
            let identity = Identity::new("u", [role]);
            assert!(
                !reads_files(&remote_session(&db, Some(identity), policy), &path),
                "{role:?}"
            );
        }
        let leaked = db
            .session()
            .execute("MATCH (n:Leak) RETURN count(n) AS c")
            .unwrap();
        assert_eq!(
            leaked.rows()[0][0],
            grafeo_common::Value::Int64(0),
            "a refused load wrote nothing"
        );
    }

    #[test]
    fn with_external_access_only_admins_read_server_files() {
        let db = db();
        let (_dir, path) = people_csv();
        let policy = RemotePolicy {
            external_access: true,
            ..RemotePolicy::default()
        };
        assert!(
            reads_files(&remote_session(&db, None, policy), &path),
            "the anonymous admin of an unauthenticated server"
        );
        assert!(reads_files(
            &remote_session(&db, Some(Identity::new("a", [Role::Admin])), policy),
            &path
        ));
        for role in [Role::ReadWrite, Role::ReadOnly] {
            let identity = Identity::new("u", [role]);
            assert!(
                !reads_files(&remote_session(&db, Some(identity), policy), &path),
                "{role:?} never reads server files"
            );
        }
    }

    #[test]
    fn the_servers_own_sessions_keep_their_access() {
        // Backups, schema loading and the change feed use `GrafeoDB::session`,
        // not `remote_session`: the policy does not reach them.
        let db = db();
        let (_dir, path) = people_csv();
        assert!(reads_files(&db.session(), &path));
    }
}
