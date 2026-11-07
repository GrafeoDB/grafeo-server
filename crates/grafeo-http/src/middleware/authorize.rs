//! Route authorization by method and path, applied once a credential is
//! verified and before the request body is read.
//!
//! One table decides what a token needs for each route that names its
//! database in the path, so a route added under `/db/{name}/` or
//! `/admin/{db}/` is covered without a change here:
//!
//! | Route | Needs |
//! | --- | --- |
//! | `/admin/tokens...`, `/admin/replication`, `/backups...` | an admin token with no database list |
//! | `/admin/{db}/...` | admin, `{db}` in scope |
//! | `POST /db` | admin (the handler checks the new name against the scope) |
//! | `DELETE /db/{name}` | admin, `{name}` in scope |
//! | `/db/{name}/sparql` | `{name}` in scope (the engine session refuses writes for read-only tokens) |
//! | `GET`/`HEAD /db/{name}/...` | `{name}` in scope |
//! | other `/db/{name}/...` | write role, `{name}` in scope |
//!
//! Routes that carry their database in the body or a session (queries,
//! batches, transactions, search, the WebSocket) check it in their handlers.
//! Handlers keep their own checks as a second layer.

use axum::http::Method;
use grafeo_service::auth::TokenInfo;

use crate::error::ApiError;

/// What a route needs from the token.
#[derive(Debug, PartialEq, Eq)]
enum Need<'a> {
    Nothing,
    /// Read access to a database.
    Read(&'a str),
    /// The write role on a database.
    Write(&'a str),
    /// The admin role, on a database when one is named.
    Admin(Option<&'a str>),
    /// The admin role with no database list.
    ServerAdmin,
}

fn need<'a>(method: &Method, path: &'a str) -> Need<'a> {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let read = matches!(*method, Method::GET | Method::HEAD);
    match segments.as_slice() {
        ["admin", "tokens", ..] | ["admin", "replication"] | ["backups", ..] => Need::ServerAdmin,
        ["admin", db, ..] => Need::Admin(Some(db)),
        ["db"] if *method == Method::POST => Need::Admin(None),
        ["db", db] if *method == Method::DELETE => Need::Admin(Some(db)),
        ["db", db, "sparql"] => Need::Read(db),
        ["db", db, ..] if read => Need::Read(db),
        ["db", db, ..] => Need::Write(db),
        _ => Need::Nothing,
    }
}

/// Refuses a request the token may not make (see the module docs).
pub(crate) fn authorize(method: &Method, path: &str, info: &TokenInfo) -> Result<(), ApiError> {
    let identity = info.identity();
    let databases = &info.scope.databases;
    // The role first, then the database scope.
    let (role_ok, role_error, database) = match need(method, path) {
        Need::Nothing => return Ok(()),
        Need::Read(db) => (true, "", Some(db)),
        Need::Write(db) => (identity.can_write(), "write access required", Some(db)),
        Need::Admin(db) => (identity.can_admin(), "admin access required", db),
        Need::ServerAdmin => (
            identity.can_admin() && databases.is_empty(),
            "admin access to every database required",
            None,
        ),
    };
    if !role_ok {
        return Err(ApiError::forbidden(role_error.to_string()));
    }
    match database {
        Some(db) if !databases.is_empty() && !databases.iter().any(|d| d == db) => Err(
            ApiError::forbidden(format!("not authorized for database '{db}'")),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_engine::auth::Role;
    use grafeo_service::auth::TokenScope;

    fn info(role: Role, dbs: &[&str]) -> TokenInfo {
        TokenInfo {
            id: "t".into(),
            name: "t".into(),
            scope: TokenScope {
                role,
                databases: dbs.iter().map(|d| (*d).to_string()).collect(),
            },
        }
    }

    #[test]
    fn the_table() {
        let cases: &[(Method, &str, Need)] = &[
            (Method::GET, "/admin/tokens", Need::ServerAdmin),
            (Method::DELETE, "/admin/tokens/tok-1", Need::ServerAdmin),
            (Method::GET, "/admin/replication", Need::ServerAdmin),
            (Method::GET, "/backups", Need::ServerAdmin),
            (
                Method::GET,
                "/admin/replication/stats",
                Need::Admin(Some("replication")),
            ),
            (Method::GET, "/admin/a/stats", Need::Admin(Some("a"))),
            (Method::POST, "/admin/a/restore", Need::Admin(Some("a"))),
            (Method::POST, "/db", Need::Admin(None)),
            (Method::GET, "/db", Need::Nothing),
            (Method::DELETE, "/db/a", Need::Admin(Some("a"))),
            (Method::GET, "/db/a", Need::Read("a")),
            (Method::POST, "/db/a/sparql", Need::Read("a")),
            (Method::GET, "/db/a/changes", Need::Read("a")),
            (Method::POST, "/db/a/graphs", Need::Write("a")),
            (Method::PUT, "/db/a/graph-store", Need::Write("a")),
            (Method::DELETE, "/db/a/graphs/g", Need::Write("a")),
            (Method::POST, "/query", Need::Nothing),
            (Method::GET, "/health", Need::Nothing),
        ];
        for (method, path, expected) in cases {
            assert_eq!(&need(method, path), expected, "{method} {path}");
        }
    }

    #[test]
    fn decisions() {
        let root = info(Role::Admin, &[]);
        let admin_a = info(Role::Admin, &["a"]);
        let rw_a = info(Role::ReadWrite, &["a"]);
        let ro = info(Role::ReadOnly, &[]);

        assert!(authorize(&Method::GET, "/admin/tokens", &root).is_ok());
        assert!(authorize(&Method::GET, "/admin/tokens", &admin_a).is_err());
        assert!(authorize(&Method::GET, "/admin/a/stats", &admin_a).is_ok());
        assert!(authorize(&Method::GET, "/admin/b/stats", &admin_a).is_err());
        assert!(authorize(&Method::GET, "/admin/a/stats", &rw_a).is_err());
        assert!(authorize(&Method::POST, "/db", &rw_a).is_err());
        assert!(authorize(&Method::DELETE, "/db/a", &rw_a).is_err());
        assert!(authorize(&Method::DELETE, "/db/a", &admin_a).is_ok());
        assert!(authorize(&Method::POST, "/db/a/graphs", &rw_a).is_ok());
        assert!(authorize(&Method::POST, "/db/b/graphs", &rw_a).is_err());
        assert!(authorize(&Method::POST, "/db/a/graphs", &ro).is_err());
        assert!(authorize(&Method::GET, "/db/a", &ro).is_ok());
        assert!(authorize(&Method::GET, "/db/b", &rw_a).is_err());
        assert!(authorize(&Method::POST, "/db/a/sparql", &ro).is_ok());
    }
}
