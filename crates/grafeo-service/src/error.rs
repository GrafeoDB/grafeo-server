//! Service-layer error types.
//!
//! `ServiceError` is transport-agnostic. Each transport crate maps it to
//! its own wire format (HTTP status codes, GQL error codes, Bolt failures).

/// Service error shared across all transports.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Query execution failed (bad syntax, type mismatch, etc.).
    #[error("{0}")]
    BadRequest(String),

    /// Transaction session not found or expired.
    #[error("session not found or expired")]
    SessionNotFound,

    /// Resource not found.
    #[error("{0}")]
    NotFound(String),

    /// Resource already exists, or is in a state that refuses the request.
    #[error("{0}")]
    Conflict(String),

    /// A transaction conflicted with a concurrent one (a write-write conflict,
    /// a serialization failure, a deadlock): nothing of it took effect and the
    /// client can retry it. HTTP reports it as 409 like [`Conflict`](Self::Conflict);
    /// Bolt as a transient error and GWP as GQLSTATUS 40001, so drivers retry.
    #[error("{0}")]
    TransactionConflict(String),

    /// Query execution timed out.
    #[error("query execution timed out")]
    Timeout,

    /// Missing or invalid authentication.
    #[error("unauthorized")]
    Unauthorized,

    /// Rate limit exceeded.
    #[error("too many requests")]
    TooManyRequests,

    /// Token does not have permission for this operation or database.
    #[error("{0}")]
    Forbidden(String),

    /// Operation rejected because server is in read-only mode.
    #[error("server is in read-only mode")]
    ReadOnly,

    /// Resource temporarily unavailable (e.g. database restoring).
    #[error("{0}")]
    Unavailable(String),

    /// Internal server error.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ServiceError {
    /// Maps an engine error to the service error every transport reports.
    ///
    /// One mapping for queries, commits, rollbacks, upserts and the direct
    /// graph and index calls:
    ///
    /// - a write-write conflict with a concurrent transaction (or a
    ///   serialization failure, deadlock or abort) is
    ///   [`TransactionConflict`](Self::TransactionConflict), which the client
    ///   can retry;
    /// - a database that takes no writes for now (closed by a restore, a
    ///   delete or a shutdown, an earlier commit that did not complete, a
    ///   write-ahead log that failed) is [`Unavailable`](Self::Unavailable);
    /// - a timeout is [`Timeout`](Self::Timeout), and a permission error
    ///   [`Forbidden`](Self::Forbidden);
    /// - internal kinds (internal, I/O, serialization, storage and corruption
    ///   errors) are [`Internal`](Self::Internal);
    /// - constraint violations, query errors and bad input stay
    ///   [`BadRequest`](Self::BadRequest).
    #[must_use]
    pub fn from_engine(error: grafeo_common::Error) -> Self {
        use grafeo_common::Error;
        use grafeo_common::utils::error::{QueryErrorKind, TransactionError};

        // A statement reaching outside the database without external access
        // (GRAFEO-Q008): the engine names its own Rust API, so say what the
        // operator of this server can do instead.
        if error.error_code() == grafeo_common::utils::error::ErrorCode::QueryExternalAccessDenied {
            return Self::Forbidden(format!(
                "{error}. On this server it needs --allow-external-access \
                 (GRAFEO_ALLOW_EXTERNAL_ACCESS), and then an admin token"
            ));
        }
        // The engine reports other permission errors without a kind of their
        // own (a semantic query error, or a read-only transaction), so the
        // message is the only sign.
        if error.to_string().contains("permission denied") {
            return Self::Forbidden(error.to_string());
        }
        match error {
            Error::Internal(_) | Error::Io(_) | Error::Serialization(_) | Error::Storage(_) => {
                Self::Internal(error.to_string())
            }
            Error::Transaction(
                TransactionError::WriteConflict(_)
                | TransactionError::Conflict
                | TransactionError::Aborted
                | TransactionError::SerializationFailure(_)
                | TransactionError::Deadlock,
            ) => Self::TransactionConflict(error.to_string()),
            Error::Transaction(
                TransactionError::DatabaseClosed
                | TransactionError::IncompleteCommit
                | TransactionError::LogWriteFailed(_)
                | TransactionError::CommitOutcomeUnknown(_)
                | TransactionError::LogUnavailable(_),
            ) => Self::Unavailable(error.to_string()),
            Error::Transaction(TransactionError::Timeout) => Self::Timeout,
            Error::Query(ref q) if q.kind == QueryErrorKind::Timeout => Self::Timeout,
            Error::NodeNotFound(_)
            | Error::EdgeNotFound(_)
            | Error::PropertyNotFound(_)
            | Error::LabelNotFound(_)
            | Error::TypeMismatch { .. }
            | Error::InvalidValue(_)
            | Error::Transaction(_)
            | Error::Query(_) => Self::BadRequest(error.to_string()),
            // `Error` is non-exhaustive: a variant added later (corruption
            // included) is not known to be the caller's fault.
            _ => Self::Internal(error.to_string()),
        }
    }

    /// The error of the batch statement at `index`: the same kind, so a
    /// refused statement is still a 403 and a conflict still retryable, with
    /// the statement's position in front of the message.
    #[must_use]
    pub fn in_batch(self, index: usize) -> Self {
        let at = |message: String| format!("query at index {index} failed: {message}");
        match self {
            Self::BadRequest(m) => Self::BadRequest(at(m)),
            Self::NotFound(m) => Self::NotFound(at(m)),
            Self::Conflict(m) => Self::Conflict(at(m)),
            Self::TransactionConflict(m) => Self::TransactionConflict(at(m)),
            Self::Forbidden(m) => Self::Forbidden(at(m)),
            Self::Unavailable(m) => Self::Unavailable(at(m)),
            Self::Internal(m) => Self::Internal(at(m)),
            // Kinds with a fixed message keep it; the status says enough.
            other @ (Self::SessionNotFound
            | Self::Timeout
            | Self::Unauthorized
            | Self::TooManyRequests
            | Self::ReadOnly) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_error_keeps_its_kind_and_names_the_statement() {
        let forbidden = ServiceError::Forbidden("no external access".into()).in_batch(2);
        assert!(matches!(&forbidden, ServiceError::Forbidden(_)));
        assert_eq!(
            forbidden.to_string(),
            "query at index 2 failed: no external access"
        );

        for (error, same_kind) in [
            (
                ServiceError::BadRequest("x".into()),
                (|e: &ServiceError| matches!(e, ServiceError::BadRequest(_)))
                    as fn(&ServiceError) -> bool,
            ),
            (ServiceError::NotFound("x".into()), |e| {
                matches!(e, ServiceError::NotFound(_))
            }),
            (ServiceError::Conflict("x".into()), |e| {
                matches!(e, ServiceError::Conflict(_))
            }),
            (ServiceError::TransactionConflict("x".into()), |e| {
                matches!(e, ServiceError::TransactionConflict(_))
            }),
            (ServiceError::Unavailable("x".into()), |e| {
                matches!(e, ServiceError::Unavailable(_))
            }),
            (ServiceError::Internal("x".into()), |e| {
                matches!(e, ServiceError::Internal(_))
            }),
            (ServiceError::Timeout, |e| {
                matches!(e, ServiceError::Timeout)
            }),
            (ServiceError::ReadOnly, |e| {
                matches!(e, ServiceError::ReadOnly)
            }),
        ] {
            let before = error.to_string();
            let fixed_message = matches!(error, ServiceError::Timeout | ServiceError::ReadOnly);
            let after = error.in_batch(0);
            assert!(same_kind(&after), "{before} kept its kind: {after:?}");
            if fixed_message {
                assert_eq!(after.to_string(), before);
            } else {
                assert!(
                    after.to_string().contains("query at index 0 failed: x"),
                    "{after}"
                );
            }
        }
    }
}
