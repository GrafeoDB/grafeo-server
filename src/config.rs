//! Server configuration via CLI args and environment variables.

use clap::Parser;

/// HTTP server for the Grafeo graph database.
#[derive(Parser, Debug, Clone)]
#[command(name = "grafeo-server", version, about)]
pub struct Config {
    /// Bind address.
    #[arg(long, default_value = "0.0.0.0", env = "GRAFEO_HOST")]
    pub host: String,

    /// Bind port.
    #[arg(long, default_value_t = 7474, env = "GRAFEO_PORT")]
    pub port: u16,

    /// Data directory for persistent storage. Omit for in-memory mode.
    #[arg(long, env = "GRAFEO_DATA_DIR")]
    pub data_dir: Option<String>,

    /// Transaction session timeout in seconds.
    #[arg(long, default_value_t = 300, env = "GRAFEO_SESSION_TTL")]
    pub session_ttl: u64,

    /// CORS allowed origins (comma-separated). Omit to deny cross-origin requests.
    /// Use "*" to allow all origins (not recommended for production).
    #[arg(long, env = "GRAFEO_CORS_ORIGINS", value_delimiter = ',')]
    pub cors_origins: Vec<String>,

    /// Query execution timeout in seconds (0 = disabled).
    #[arg(long, default_value_t = 30, env = "GRAFEO_QUERY_TIMEOUT")]
    pub query_timeout: u64,

    /// Bearer token / API key for authentication. If set, non-exempt endpoints
    /// require `Authorization: Bearer <token>` or `X-API-Key: <token>`.
    #[cfg(feature = "auth")]
    #[arg(long, env = "GRAFEO_AUTH_TOKEN")]
    pub auth_token: Option<String>,

    /// Username for HTTP Basic authentication. Requires --auth-password.
    #[cfg(feature = "auth")]
    #[arg(long, env = "GRAFEO_AUTH_USER", requires = "auth_password")]
    pub auth_user: Option<String>,

    /// Password for HTTP Basic authentication. Requires --auth-user.
    #[cfg(feature = "auth")]
    #[arg(long, env = "GRAFEO_AUTH_PASSWORD", requires = "auth_user")]
    pub auth_password: Option<String>,

    /// GQL Wire Protocol (gRPC) port.
    #[cfg(feature = "gwp")]
    #[arg(long, default_value_t = 7688, env = "GRAFEO_GWP_PORT")]
    pub gwp_port: u16,

    /// Maximum concurrent GWP sessions. 0 = unlimited.
    #[cfg(feature = "gwp")]
    #[arg(long, default_value_t = 0, env = "GRAFEO_GWP_MAX_SESSIONS")]
    pub gwp_max_sessions: usize,

    /// Bolt v5 protocol port.
    #[cfg(feature = "bolt")]
    #[arg(long, default_value_t = 7687, env = "GRAFEO_BOLT_PORT")]
    pub bolt_port: u16,

    /// Maximum concurrent Bolt sessions. 0 = unlimited.
    #[cfg(feature = "bolt")]
    #[arg(long, default_value_t = 0, env = "GRAFEO_BOLT_MAX_SESSIONS")]
    pub bolt_max_sessions: usize,

    /// Log level.
    #[arg(long, default_value = "info", env = "GRAFEO_LOG_LEVEL")]
    pub log_level: String,

    /// Log format: "pretty" (default, human-readable) or "json" (structured).
    #[arg(long, default_value = "pretty", env = "GRAFEO_LOG_FORMAT")]
    pub log_format: String,

    /// Rate limit: max requests per window per IP. 0 = disabled.
    #[arg(long, default_value_t = 0, env = "GRAFEO_RATE_LIMIT")]
    pub rate_limit: u64,

    /// Rate limit window in seconds.
    #[arg(long, default_value_t = 60, env = "GRAFEO_RATE_LIMIT_WINDOW")]
    pub rate_limit_window: u64,

    /// Path to TLS certificate file (PEM format). Enables HTTPS.
    #[arg(long, env = "GRAFEO_TLS_CERT", requires = "tls_key")]
    pub tls_cert: Option<String>,

    /// Path to TLS private key file (PEM format). Requires --tls-cert.
    #[arg(long, env = "GRAFEO_TLS_KEY", requires = "tls_cert")]
    pub tls_key: Option<String>,
}

impl Config {
    /// Parses configuration from CLI args and env vars.
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }

    /// Returns true if TLS is configured.
    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some() && self.tls_key.is_some()
    }
}
