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

        // The engine reports a permission error without a kind of its own
        // (a semantic query error, or a read-only transaction), so its
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
}
