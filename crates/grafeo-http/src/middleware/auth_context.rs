//! AuthContext extractor: pulls token identity from request extensions.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use grafeo_engine::auth::Identity;

use crate::error::ApiError;

/// Token identity extracted from request extensions.
///
/// `None` when auth is disabled (no restrictions).
/// `Some(info)` when a token was validated by the auth middleware.
pub struct AuthContext(pub Option<grafeo_service::auth::TokenInfo>);

impl AuthContext {
    /// Check that the token can access the given database. No-op when auth is off.
    pub fn check_db_access(&self, db_name: &str) -> Result<(), ApiError> {
        if let Some(ref info) = self.0
            && !info.scope.databases.is_empty()
            && !info.scope.databases.iter().any(|d| d == db_name)
        {
            return Err(ApiError::forbidden(format!(
                "token not authorized for database '{db_name}'"
            )));
        }
        Ok(())
    }

    /// Check that the token has admin role. No-op when auth is off.
    pub fn check_admin(&self) -> Result<(), ApiError> {
        if let Some(ref info) = self.0
            && !info.identity().can_admin()
        {
            return Err(ApiError::forbidden("admin access required".to_string()));
        }
        Ok(())
    }

    /// Check that the token can write (admin or read-write). No-op when auth is off.
    pub fn check_write(&self) -> Result<(), ApiError> {
        if let Some(ref info) = self.0
            && !info.identity().can_write()
        {
            return Err(ApiError::forbidden("write access required".to_string()));
        }
        Ok(())
    }

    /// Build an engine [`Identity`] from the token, or anonymous if auth is off.
    ///
    /// When `server_read_only` is true, the identity is capped to [`Role::ReadOnly`]
    /// regardless of the token's role.
    pub fn identity(&self, server_read_only: bool) -> Identity {
        let base = match &self.0 {
            Some(info) => info.identity(),
            None => Identity::anonymous(),
        };
        grafeo_service::auth::cap_identity_read_only(base, server_read_only)
    }
}

impl<S> FromRequestParts<S> for AuthContext
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let info = parts
            .extensions
            .get::<grafeo_service::auth::TokenInfo>()
            .cloned();
        Ok(AuthContext(info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_engine::auth::Role;
    use grafeo_service::auth::{TokenInfo, TokenScope};

    fn admin_ctx() -> AuthContext {
        AuthContext(Some(TokenInfo {
            id: "t1".into(),
            name: "admin".into(),
            scope: TokenScope {
                role: Role::Admin,
                databases: vec![],
            },
        }))
    }

    fn scoped_ctx(dbs: Vec<String>) -> AuthContext {
        AuthContext(Some(TokenInfo {
            id: "t2".into(),
            name: "scoped".into(),
            scope: TokenScope {
                role: Role::ReadWrite,
                databases: dbs,
            },
        }))
    }

    fn readonly_ctx() -> AuthContext {
        AuthContext(Some(TokenInfo {
            id: "t3".into(),
            name: "reader".into(),
            scope: TokenScope {
                role: Role::ReadOnly,
                databases: vec![],
            },
        }))
    }

    #[test]
    fn check_db_access_none_is_noop() {
        AuthContext(None).check_db_access("anything").unwrap();
    }

    #[test]
    fn check_db_access_admin_all_dbs() {
        admin_ctx().check_db_access("any-db").unwrap();
    }

    #[test]
    fn check_db_access_scoped_allowed() {
        scoped_ctx(vec!["mydb".into()])
            .check_db_access("mydb")
            .unwrap();
    }

    #[test]
    fn check_db_access_scoped_denied() {
        scoped_ctx(vec!["mydb".into()])
            .check_db_access("other")
            .unwrap_err();
    }

    #[test]
    fn check_admin_none_is_noop() {
        AuthContext(None).check_admin().unwrap();
    }

    #[test]
    fn check_admin_admin_ok() {
        admin_ctx().check_admin().unwrap();
    }

    #[test]
    fn check_admin_readonly_denied() {
        readonly_ctx().check_admin().unwrap_err();
    }

    #[test]
    fn check_write_none_is_noop() {
        AuthContext(None).check_write().unwrap();
    }

    #[test]
    fn check_write_admin_ok() {
        admin_ctx().check_write().unwrap();
    }

    #[test]
    fn check_write_readwrite_ok() {
        scoped_ctx(vec![]).check_write().unwrap();
    }

    #[test]
    fn check_write_readonly_denied() {
        readonly_ctx().check_write().unwrap_err();
    }

    #[test]
    fn identity_none_returns_anonymous_admin() {
        let ctx = AuthContext(None);
        let id = ctx.identity(false);
        assert!(id.can_admin());
    }

    #[test]
    fn identity_none_read_only_server() {
        let ctx = AuthContext(None);
        let id = ctx.identity(true);
        assert!(id.can_read());
        assert!(!id.can_write());
    }

    #[test]
    fn identity_admin_capped_by_server_read_only() {
        let ctx = admin_ctx();
        let id = ctx.identity(true);
        assert!(id.can_read());
        assert!(!id.can_write());
    }

    #[test]
    fn identity_preserves_role_when_not_read_only() {
        let ctx = readonly_ctx();
        let id = ctx.identity(false);
        assert!(id.can_read());
        assert!(!id.can_write());
    }
}
