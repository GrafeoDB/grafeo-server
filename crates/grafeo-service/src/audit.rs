//! Audit log: a structured (JSON-lines) record of security-relevant events.
//!
//! Opt-in at run time with `--audit` (`GRAFEO_AUDIT=true`): failed
//! authentication attempts with the source IP, token and database lifecycle
//! events, and — with `--audit-data-changes` — every write statement that
//! changed data. Events are one JSON object per line, written to stdout or,
//! with `--audit-log-file`, to a file that rotates at `--audit-max-size-mb`
//! (keeping `--audit-keep` rotated files as `audit.log.1`, `audit.log.2`, ...).
//!
//! Secrets (token plaintexts, passwords) are never logged: token events name
//! the token's id and name only.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// Audit log configuration (from `--audit*` flags / `GRAFEO_AUDIT*` env vars).
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// File to write to; `None` writes to stdout (no rotation).
    pub log_file: Option<PathBuf>,
    /// Rotate the file once it grows past this size. 0 turns rotation off.
    pub max_size_bytes: u64,
    /// How many rotated files to keep.
    pub keep: u32,
    /// Also audit data changes (write statements), not just security events.
    pub data_changes: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            log_file: None,
            max_size_bytes: 100 * 1024 * 1024,
            keep: 5,
            data_changes: false,
        }
    }
}

/// One audit record, serialized as a single JSON line.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditEvent {
    /// RFC 3339 UTC timestamp.
    pub ts: String,
    /// Event name: `auth_failure`, `access_denied`, `token_created`,
    /// `token_revoked`, `database_created`, `database_deleted`, `data_change`.
    pub event: String,
    /// Who acted: the token name (or `basic-auth` user), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Transport the event came in on: `http`, `gwp`, `bolt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ip: Option<String>,
    /// Free-form context (method and path, statement, counters). Never a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEvent {
    #[must_use]
    pub fn new(event: impl Into<String>) -> Self {
        Self {
            ts: rfc3339_now(),
            event: event.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn actor(mut self, actor: Option<impl Into<String>>) -> Self {
        self.actor = actor.map(Into::into);
        self
    }

    #[must_use]
    pub fn database(mut self, database: impl Into<String>) -> Self {
        self.database = Some(database.into());
        self
    }

    #[must_use]
    pub fn transport(mut self, transport: &str) -> Self {
        self.transport = Some(transport.to_string());
        self
    }

    #[must_use]
    pub fn source_ip(mut self, ip: Option<impl std::fmt::Display>) -> Self {
        self.source_ip = ip.map(|ip| ip.to_string());
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A borrowed handle for the query path: the logger plus who is asking.
pub struct AuditRef<'a> {
    pub logger: &'a AuditLogger,
    pub actor: Option<&'a str>,
    pub transport: &'a str,
}

enum AuditSink {
    Stdout,
    File {
        file: std::fs::File,
        path: PathBuf,
        size: u64,
    },
}

/// The audit log writer. Cheap to clone via `Arc`; writes are serialized
/// through a mutex so events from concurrent requests never interleave.
pub struct AuditLogger {
    config: AuditConfig,
    sink: Mutex<AuditSink>,
}

impl AuditLogger {
    /// Opens the configured sink. Fails when the log file cannot be created.
    pub fn new(config: AuditConfig) -> std::io::Result<Self> {
        let sink = match &config.log_file {
            None => AuditSink::Stdout,
            Some(path) => {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent)?;
                }
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                let size = file.metadata()?.len();
                AuditSink::File {
                    file,
                    path: path.clone(),
                    size,
                }
            }
        };
        Ok(Self {
            config,
            sink: Mutex::new(sink),
        })
    }

    /// Whether data changes (write statements) are audited too.
    pub fn data_changes_enabled(&self) -> bool {
        self.config.data_changes
    }

    /// Writes one event as a JSON line, rotating the file first when needed.
    /// A write error is reported to the tracing log, never to the caller:
    /// auditing must not break the request it records.
    pub fn log(&self, event: AuditEvent) {
        let mut line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!("audit event did not serialize: {e}");
                return;
            }
        };
        line.push('\n');
        let mut sink = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        if let AuditSink::File { file, path, size } = &mut *sink {
            if self.config.max_size_bytes > 0
                && size.saturating_add(line.len() as u64) > self.config.max_size_bytes
            {
                Self::rotate(path, self.config.keep);
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path.as_path())
                {
                    Ok(f) => {
                        *file = f;
                        *size = 0;
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), "audit log rotation failed: {e}");
                        return;
                    }
                }
            }
            match file.write_all(line.as_bytes()) {
                Ok(()) => *size += line.len() as u64,
                Err(e) => tracing::warn!("audit log write failed: {e}"),
            }
            let _ = file.flush();
        } else if let Err(e) = std::io::stdout().write_all(line.as_bytes()) {
            tracing::warn!("audit log write to stdout failed: {e}");
        }
    }

    /// Shifts `path.1`..`path.keep` one up and removes the oldest, so `path`
    /// can be renamed to `path.1`.
    fn rotate(path: &Path, keep: u32) {
        if keep == 0 {
            return;
        }
        let oldest = rotated_path(path, keep);
        let _ = std::fs::remove_file(&oldest);
        for i in (1..keep).rev() {
            let from = rotated_path(path, i);
            let to = rotated_path(path, i + 1);
            if from.exists() {
                let _ = std::fs::rename(&from, &to);
            }
        }
        let _ = std::fs::rename(path, rotated_path(path, 1));
    }

    /// Audits a write statement that changed data (`result.counters` says so).
    pub fn data_change(
        &self,
        actor: Option<&str>,
        database: &str,
        transport: &str,
        statement: &str,
        counters: &grafeo_engine::database::WriteCounters,
    ) {
        if !self.config.data_changes {
            return;
        }
        let detail = format!("{} | counters: {counters:?}", truncate(statement, 500));
        self.log(
            AuditEvent::new("data_change")
                .actor(actor)
                .database(database)
                .transport(transport)
                .detail(detail),
        );
    }
}

fn rotated_path(path: &Path, i: u32) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(format!(".{i}"));
    PathBuf::from(p)
}

/// Truncates to `max` chars, on a char boundary, with an ellipsis marker.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Formats the current UTC time as RFC 3339 (`2026-11-08T18:17:46Z`).
fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    rfc3339_from_unix(secs)
}

fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let (hour, min, sec) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_epoch() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_known_dates() {
        assert_eq!(rfc3339_from_unix(10_957 * 86_400), "2000-01-01T00:00:00Z");
        // 2024 is a leap year: 2024-02-29 is day 19_782.
        assert_eq!(rfc3339_from_unix(19_782 * 86_400), "2024-02-29T00:00:00Z");
        assert_eq!(rfc3339_from_unix(19_783 * 86_400), "2024-03-01T00:00:00Z");
    }

    #[test]
    fn event_serializes_omitting_none_fields() {
        let line =
            serde_json::to_string(&AuditEvent::new("auth_failure").transport("http")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "auth_failure");
        assert_eq!(v["transport"], "http");
        assert!(v.get("actor").is_none());
        assert!(v.get("source_ip").is_none());
        assert!(v["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn writes_and_rotates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let cfg = AuditConfig {
            log_file: Some(path.clone()),
            max_size_bytes: 200,
            keep: 2,
            data_changes: false,
        };
        let logger = AuditLogger::new(cfg).unwrap();
        for i in 0..10 {
            logger.log(AuditEvent::new("token_created").detail(format!("event {i}")));
        }
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("event 9"));
        let rotated = std::fs::read_to_string(path.with_extension("log.1")).unwrap();
        assert!(
            rotated.contains("event"),
            "rotated file holds earlier events"
        );
        // keep = 2: at most audit.log.1 and audit.log.2 exist.
        assert!(path.with_extension("log.2").exists());
        assert!(!path.with_extension("log.3").exists());
    }

    #[test]
    fn stdout_config_needs_no_file() {
        let logger = AuditLogger::new(AuditConfig::default()).unwrap();
        assert!(!logger.data_changes_enabled());
        logger.log(AuditEvent::new("auth_failure"));
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate("hello", 10), "hello");
        let s = "aé‱b".repeat(100);
        let t = truncate(&s, 10);
        assert!(t.ends_with("..."));
        assert!(t.len() <= 13);
    }
}
