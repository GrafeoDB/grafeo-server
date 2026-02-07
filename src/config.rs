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

    /// CORS allowed origins (comma-separated). Empty for no CORS.
    #[arg(long, env = "GRAFEO_CORS_ORIGINS", value_delimiter = ',')]
    pub cors_origins: Vec<String>,

    /// Query execution timeout in seconds (0 = disabled).
    #[arg(long, default_value_t = 30, env = "GRAFEO_QUERY_TIMEOUT")]
    pub query_timeout: u64,

    /// Bearer token for API authentication. If set, non-exempt endpoints require it.
    #[arg(long, env = "GRAFEO_AUTH_TOKEN")]
    pub auth_token: Option<String>,

    /// Log level.
    #[arg(long, default_value = "info", env = "GRAFEO_LOG_LEVEL")]
    pub log_level: String,
}

impl Config {
    /// Parses configuration from CLI args and env vars.
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}
