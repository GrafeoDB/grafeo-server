//! Audit log: a structured (JSON-lines) record of security-relevant events.
//!
//! Opt-in at run time with `--audit` (`GRAFEO_AUDIT=true`): failed
//! authentication attempts with the source IP, refused requests, token and
//! database lifecycle events, and — with `--audit-data-changes` — every write
//! statement that changed data. Events are one JSON object per line, written
//! to stdout or, with `--audit-log-file`, to a file that rotates at
//! `--audit-max-size-mb` (keeping `--audit-keep` rotated files as
//! `audit.log.1`, `audit.log.2`, ...).
//!
//! Token plaintexts and passwords are never logged: token events name the
//! token's id and name only. Note that with `--audit-data-changes` the write
//! statements themselves are recorded, so a secret written as a statement
//! literal ends up in the log like any other value.
//!
//! Writes happen on a dedicated writer thread (the send is a bounded,
//! non-blocking channel), so a brute-force burst of `auth_failure` events
//! never does file I/O on a Tokio worker; when the channel is full, events
//! are dropped and counted rather than blocking the request path.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use serde::Serialize;

/// Audit log configuration (from `--audit*` flags / `GRAFEO_AUDIT*` env vars).
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// File to write to; `None` writes to stdout (no rotation).
    pub log_file: Option<PathBuf>,
    /// Rotate the file once it grows past this size. 0 turns rotation off.
    pub max_size_bytes: u64,
    /// How many rotated files to keep. 0 truncates the active file instead.
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
    /// Event name: `auth_failure`, `access_denied`, `cross_site_refused`,
    /// `token_created`, `token_revoked`, `database_created`, `database_deleted`,
    /// `data_change`.
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
    /// Free-form context (method and path, reason, statement, counters).
    /// Never a credential.
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

/// An owned audit handle: the logger plus who is asking. Cheap to clone, and
/// `Send`, so it can move into a blocking task.
#[derive(Clone)]
pub struct AuditRef {
    pub logger: Arc<AuditLogger>,
    pub actor: Option<String>,
    pub transport: &'static str,
}

enum Msg {
    Line(String),
    /// Flush the sink and acknowledge, so a test can read the file.
    Flush(mpsc::Sender<()>),
    /// Stop the writer after flushing; `shutdown` joins the thread.
    Shutdown(mpsc::Sender<()>),
}

enum Sink {
    Stdout,
    File {
        file: std::fs::File,
        path: PathBuf,
        size: u64,
    },
}

/// The audit log writer. `log` only enqueues the line; a dedicated thread
/// does the file I/O and rotation.
pub struct AuditLogger {
    tx: mpsc::SyncSender<Msg>,
    handle: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    dropped: Arc<AtomicU64>,
    data_changes: bool,
}

impl AuditLogger {
    /// Opens the configured sink (failing when the log file cannot be created
    /// or opened, so audit data is never silently redirected) and starts the
    /// writer thread.
    pub fn new(config: AuditConfig) -> std::io::Result<Self> {
        let data_changes = config.data_changes;
        let sink = match &config.log_file {
            None => Sink::Stdout,
            Some(path) => {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent)?;
                }
                let file = open_audit_file(path)?;
                let size = file.metadata().map_or(0, |m| m.len());
                Sink::File {
                    file,
                    path: path.clone(),
                    size,
                }
            }
        };
        let (tx, rx) = mpsc::sync_channel::<Msg>(1024);
        let dropped = Arc::new(AtomicU64::new(0));
        let handle = {
            let dropped = Arc::clone(&dropped);
            std::thread::Builder::new()
                .name("grafeo-audit".to_string())
                .spawn(move || writer_loop(config, sink, rx, dropped))?
        };
        Ok(Self {
            tx,
            handle: std::sync::Mutex::new(Some(handle)),
            dropped,
            data_changes,
        })
    }

    /// Whether data changes (write statements) are audited too.
    pub fn data_changes_enabled(&self) -> bool {
        self.data_changes
    }

    /// Enqueues one event as a JSON line. Never blocks: when the writer
    /// thread is behind, the event is dropped and counted (a warn every
    /// 1,000th drop), so a brute-force burst cannot stall request workers.
    pub fn log(&self, event: AuditEvent) {
        let mut line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!("audit event did not serialize: {e}");
                return;
            }
        };
        line.push('\n');
        match self.tx.try_send(Msg::Line(line)) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed);
                if n.is_multiple_of(1000) {
                    tracing::warn!(
                        dropped = n + 1,
                        "audit log writer is behind; dropped events (back-pressure)"
                    );
                }
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!("audit log writer thread is gone");
            }
        }
    }

    /// Waits until every event logged so far is written to the sink.
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// How many events were dropped because the writer thread was behind.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Flushes queued events and stops the writer thread, so a graceful
    /// shutdown does not abandon audit events still in the channel.
    pub fn shutdown(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        let _ = self.tx.send(Msg::Shutdown(ack_tx));
        let _ = ack_rx.recv();
        if let Some(handle) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
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
        if !self.data_changes {
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

/// Opens the audit file owner-only, so logged values are not world-readable.
fn open_audit_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn writer_loop(
    config: AuditConfig,
    mut sink: Sink,
    rx: mpsc::Receiver<Msg>,
    _dropped: Arc<AtomicU64>,
) {
    use std::io::Write as _;
    loop {
        let msg = match rx.recv() {
            Ok(msg) => msg,
            Err(_) => break, // all senders dropped
        };
        match msg {
            Msg::Line(line) => {
                if let Sink::File { file, path, size } = &mut sink {
                    if config.max_size_bytes > 0
                        && size.saturating_add(line.len() as u64) > config.max_size_bytes
                    {
                        rotate(path, config.keep);
                        match open_audit_file(path) {
                            Ok(f) => {
                                *file = f;
                                *size = 0;
                            }
                            Err(e) => {
                                tracing::warn!(path = %path.display(), "audit log rotation failed: {e}");
                                continue;
                            }
                        }
                    }
                    match file.write_all(line.as_bytes()) {
                        Ok(()) => *size += line.len() as u64,
                        Err(e) => tracing::warn!("audit log write failed: {e}"),
                    }
                    let _ = file.flush();
                } else {
                    match std::io::stdout().write_all(line.as_bytes()) {
                        Ok(()) => {}
                        Err(e) => tracing::warn!("audit log write to stdout failed: {e}"),
                    }
                }
            }
            Msg::Flush(ack) => {
                if let Sink::File { file, .. } = &mut sink {
                    let _ = file.flush();
                }
                let _ = ack.send(());
            }
            Msg::Shutdown(ack) => {
                if let Sink::File { file, .. } = &mut sink {
                    let _ = file.flush();
                }
                let _ = ack.send(());
                break;
            }
        }
    }
}

/// Rotates the log file: with `keep > 0`, shifts `path.1`..`path.keep` one up
/// (removing the oldest) so `path` can be renamed to `path.1`; with
/// `keep == 0`, removes the active file so the next open starts a fresh one.
fn rotate(path: &Path, keep: u32) {
    if keep == 0 {
        let _ = std::fs::remove_file(path);
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

fn rotated_path(path: &Path, i: u32) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(format!(".{i}"));
    PathBuf::from(p)
}

/// How many write statements a transaction session queues for the commit-time
/// audit before further ones are only counted as dropped.
pub(crate) const MAX_PENDING_WRITES: usize = 1000;

/// Truncates to `max` bytes, on a char boundary, with an ellipsis marker.
pub(crate) fn truncate(s: &str, max: usize) -> String {
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
        logger.flush();
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
    fn keep_zero_truncates_instead_of_growing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let cfg = AuditConfig {
            log_file: Some(path.clone()),
            max_size_bytes: 100,
            keep: 0,
            data_changes: false,
        };
        let logger = AuditLogger::new(cfg).unwrap();
        for i in 0..20 {
            logger.log(AuditEvent::new("auth_failure").detail(format!("event {i}")));
        }
        logger.flush();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(
            size < 400,
            "the active file must not grow past a rotation: {size}"
        );
        assert!(!path.with_extension("log.1").exists());
    }

    #[cfg(unix)]
    #[test]
    fn audit_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let logger = AuditLogger::new(AuditConfig {
            log_file: Some(path.clone()),
            ..Default::default()
        })
        .unwrap();
        logger.log(AuditEvent::new("auth_failure"));
        logger.flush();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "audit log must be owner-only, got {mode:o}");
    }

    #[test]
    fn stdout_config_needs_no_file() {
        let logger = AuditLogger::new(AuditConfig::default()).unwrap();
        assert!(!logger.data_changes_enabled());
        logger.log(AuditEvent::new("auth_failure"));
        logger.flush();
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
