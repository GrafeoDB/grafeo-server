//! Error type for the grafeo-sync client.

/// Errors that can occur during sync operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The provided base URL or database name could not be parsed into a valid URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// An HTTP transport error occurred (connection refused, timeout, etc.).
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The server returned a non-2xx status code.
    #[error("server returned {status}: {body}")]
    ServerError { status: u16, body: String },
}
