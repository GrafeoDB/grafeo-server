//! Route authorization by method and path, applied once a credential is
//! verified and before the request body is read.
//!
//! One table decides what a token needs for each route that names its
//! database in the path, so a route added under `/db/{name}/` or
//! `/admin/{db}/` is covered without a change here:
//!
//! | Route | Needs |
//! | --- | --- |
//! | `/admin/tokens`, `GET`/`HEAD`/`DELETE /admin/tokens/{id}`, `/admin/replication`, `/backups...` | an admin token with no database list |
//! | other methods under `/admin/tokens/...`, `/admin/{db}/...` | admin, `{db}` in scope |
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
    // Exactly the token-management routes, by shape and method:
    // `/admin/tokens` (GET list, POST create) and `/admin/tokens/{id}`
    // (GET, DELETE). Other methods under `/admin/tokens/...` are the
    // database-scoped routes of a database named `tokens`.
    let is_token_route = match segments.as_slice() {
        ["admin", "tokens"] => true,
        // Axum serves HEAD through the GET handler, so HEAD counts here too.
        ["admin", "tokens", _] => matches!(*method, Method::GET | Method::HEAD | Method::DELETE),
        _ => false,
    };
    match segments.as_slice() {
        _ if is_token_route => Need::ServerAdmin,
        ["admin", "replication"] | ["backups", ..] => Need::ServerAdmin,
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
        // Compare the decoded name: `req.uri().path()` is percent-encoded,
        // while the `Path` extractors decode it, so `/admin/%61/stats` acts
        // on database `a` and the scope must be checked against `a`.
        Some(db) => {
            let db = percent_decode(db);
            if !databases.is_empty() && !databases.iter().any(|d| d == &db) {
                return Err(ApiError::forbidden(format!(
                    "not authorized for database '{db}'"
                )));
            }
            Ok(())
        }
        None => Ok(()),
    }
}

/// Percent-decodes one path segment, as `Path` extractors decode it.
fn percent_decode(segment: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
            // The token routes, by method: GET/HEAD/DELETE on /admin/tokens/{id}.
            (Method::GET, "/admin/tokens/stats", Need::ServerAdmin),
            (Method::HEAD, "/admin/tokens/tok-1", Need::ServerAdmin),
            (Method::DELETE, "/admin/tokens/tok-1", Need::ServerAdmin),
            // Other methods under /admin/tokens/... belong to a database
            // named `tokens`.
            (
                Method::POST,
                "/admin/tokens/backup",
                Need::Admin(Some("tokens")),
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

    #[test]
    fn token_routes_are_exactly_token_management() {
        // A scoped admin of database `tokens` reaches its database-scoped
        // routes under /admin/tokens/..., but token management itself still
        // needs the server-wide admin.
        let admin_tokens = info(Role::Admin, &["tokens"]);
        assert!(authorize(&Method::POST, "/admin/tokens/backup", &admin_tokens).is_ok());
        assert!(authorize(&Method::GET, "/admin/tokens", &admin_tokens).is_err());
        assert!(authorize(&Method::DELETE, "/admin/tokens/tok-1", &admin_tokens).is_err());
        assert!(authorize(&Method::HEAD, "/admin/tokens/tok-1", &admin_tokens).is_err());
        assert!(authorize(&Method::GET, "/admin/tokens/stats", &admin_tokens).is_err());
    }

    #[test]
    fn the_scope_check_compares_the_decoded_database_name() {
        // `/admin/%61/stats` acts on database `a` (the Path extractor decodes),
        // so a scope of `%61` must not grant it and a scope of `a` must.
        let scoped_percent = info(Role::Admin, &["%61"]);
        let scoped_a = info(Role::Admin, &["a"]);
        assert!(authorize(&Method::GET, "/admin/%61/stats", &scoped_percent).is_err());
        assert!(authorize(&Method::GET, "/admin/%61/stats", &scoped_a).is_ok());
        assert!(authorize(&Method::GET, "/admin/a/stats", &scoped_a).is_ok());
        assert!(authorize(&Method::DELETE, "/db/%61", &scoped_a).is_ok());
        assert!(authorize(&Method::DELETE, "/db/%61", &scoped_percent).is_err());
    }

    #[test]
    fn percent_decode_works() {
        assert_eq!(percent_decode("%61"), "a");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%6"), "%6");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
