//! Lightweight Prometheus-compatible metrics using atomic counters.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-language query statistics.
struct LanguageMetrics {
    queries_total: AtomicU64,
    query_errors_total: AtomicU64,
    /// Accumulated query duration stored as microseconds.
    query_duration_us_sum: AtomicU64,
    query_duration_count: AtomicU64,
}

impl LanguageMetrics {
    const fn new() -> Self {
        Self {
            queries_total: AtomicU64::new(0),
            query_errors_total: AtomicU64::new(0),
            query_duration_us_sum: AtomicU64::new(0),
            query_duration_count: AtomicU64::new(0),
        }
    }
}

/// Recognized query language for metrics labelling.
#[derive(Clone, Copy, Debug)]
pub enum Language {
    Gql,
    Cypher,
    Graphql,
    Gremlin,
    Sparql,
    SqlPgq,
}

impl Language {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gql => "gql",
            Self::Cypher => "cypher",
            Self::Graphql => "graphql",
            Self::Gremlin => "gremlin",
            Self::Sparql => "sparql",
            Self::SqlPgq => "sql-pgq",
        }
    }
}

const ALL_LANGUAGES: [Language; 6] = [
    Language::Gql,
    Language::Cypher,
    Language::Graphql,
    Language::Gremlin,
    Language::Sparql,
    Language::SqlPgq,
];

/// Application-wide metrics collected via atomic counters.
pub struct Metrics {
    gql: LanguageMetrics,
    cypher: LanguageMetrics,
    graphql: LanguageMetrics,
    gremlin: LanguageMetrics,
    sparql: LanguageMetrics,
    sql_pgq: LanguageMetrics,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub const fn new() -> Self {
        Self {
            gql: LanguageMetrics::new(),
            cypher: LanguageMetrics::new(),
            graphql: LanguageMetrics::new(),
            gremlin: LanguageMetrics::new(),
            sparql: LanguageMetrics::new(),
            sql_pgq: LanguageMetrics::new(),
        }
    }

    fn lang(&self, lang: Language) -> &LanguageMetrics {
        match lang {
            Language::Gql => &self.gql,
            Language::Cypher => &self.cypher,
            Language::Graphql => &self.graphql,
            Language::Gremlin => &self.gremlin,
            Language::Sparql => &self.sparql,
            Language::SqlPgq => &self.sql_pgq,
        }
    }

    /// Record a successful query execution.
    pub fn record_query(&self, lang: Language, duration_us: u64) {
        let m = self.lang(lang);
        m.queries_total.fetch_add(1, Ordering::Relaxed);
        m.query_duration_us_sum
            .fetch_add(duration_us, Ordering::Relaxed);
        m.query_duration_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a failed query.
    pub fn record_query_error(&self, lang: Language) {
        let m = self.lang(lang);
        m.queries_total.fetch_add(1, Ordering::Relaxed);
        m.query_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Render all metrics in Prometheus text exposition format.
    ///
    /// When `engine_metrics` is provided, the engine's own Prometheus output
    /// is appended after a blank separator line.
    pub fn render(
        &self,
        databases_total: usize,
        nodes_total: usize,
        edges_total: usize,
        active_sessions: usize,
        uptime_seconds: u64,
        engine_metrics: Option<&str>,
    ) -> String {
        let mut out = String::with_capacity(2048);

        // Gauges (live values)
        gauge(
            &mut out,
            "grafeo_databases_total",
            "Current number of databases",
            databases_total,
        );
        gauge(
            &mut out,
            "grafeo_nodes_total",
            "Total nodes across all databases",
            nodes_total,
        );
        gauge(
            &mut out,
            "grafeo_edges_total",
            "Total edges across all databases",
            edges_total,
        );
        gauge(
            &mut out,
            "grafeo_active_sessions_total",
            "Active transaction sessions",
            active_sessions,
        );
        gauge(
            &mut out,
            "grafeo_uptime_seconds",
            "Server uptime in seconds",
            uptime_seconds,
        );

        // Per-language counters
        writeln!(out, "# HELP grafeo_queries_total Total queries executed.").unwrap();
        writeln!(out, "# TYPE grafeo_queries_total counter").unwrap();
        for lang in &ALL_LANGUAGES {
            let m = self.lang(*lang);
            let label = lang.label();
            let total = m.queries_total.load(Ordering::Relaxed);
            writeln!(out, "grafeo_queries_total{{language=\"{label}\"}} {total}").unwrap();
        }

        writeln!(
            out,
            "# HELP grafeo_query_errors_total Total failed queries."
        )
        .unwrap();
        writeln!(out, "# TYPE grafeo_query_errors_total counter").unwrap();
        for lang in &ALL_LANGUAGES {
            let m = self.lang(*lang);
            let label = lang.label();
            let errors = m.query_errors_total.load(Ordering::Relaxed);
            writeln!(
                out,
                "grafeo_query_errors_total{{language=\"{label}\"}} {errors}"
            )
            .unwrap();
        }

        writeln!(
            out,
            "# HELP grafeo_query_duration_seconds_sum Total query execution time in seconds."
        )
        .unwrap();
        writeln!(out, "# TYPE grafeo_query_duration_seconds_sum counter").unwrap();
        for lang in &ALL_LANGUAGES {
            let m = self.lang(*lang);
            let label = lang.label();
            let us = m.query_duration_us_sum.load(Ordering::Relaxed);
            let secs = us as f64 / 1_000_000.0;
            writeln!(
                out,
                "grafeo_query_duration_seconds_sum{{language=\"{label}\"}} {secs:.6}"
            )
            .unwrap();
        }

        writeln!(
            out,
            "# HELP grafeo_query_duration_seconds_count Total number of timed queries."
        )
        .unwrap();
        writeln!(out, "# TYPE grafeo_query_duration_seconds_count counter").unwrap();
        for lang in &ALL_LANGUAGES {
            let m = self.lang(*lang);
            let label = lang.label();
            let count = m.query_duration_count.load(Ordering::Relaxed);
            writeln!(
                out,
                "grafeo_query_duration_seconds_count{{language=\"{label}\"}} {count}"
            )
            .unwrap();
        }

        // Append engine-level Prometheus metrics (when available)
        if let Some(engine) = engine_metrics {
            out.push('\n');
            out.push_str(engine);
        }

        out
    }
}

fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    writeln!(out, "# HELP {name} {help}").unwrap();
    writeln!(out, "# TYPE {name} gauge").unwrap();
    writeln!(out, "{name} {value}").unwrap();
}

/// The languages a request may name, for error messages.
pub const SUPPORTED_LANGUAGES: &str = "gql, cypher, graphql, gremlin, sparql, sql-pgq";

/// Maps a request's `language` to a [`Language`]: `None` is GQL, the names
/// match without regard to case (`"Cypher"`, `"SPARQL"`), and `sql` is an
/// alias of `sql-pgq`. Any other name is a bad request; it used to run as
/// GQL, so a Cypher query sent as `"Cypher"` ran with GQL semantics.
///
/// A language this build leaves out is accepted here and refused by the
/// dispatcher with a bad request that names the missing feature.
pub fn determine_language(lang: Option<&str>) -> Result<Language, crate::error::ServiceError> {
    let Some(name) = lang else {
        return Ok(Language::Gql);
    };
    match name.to_ascii_lowercase().as_str() {
        "gql" => Ok(Language::Gql),
        "cypher" => Ok(Language::Cypher),
        "graphql" => Ok(Language::Graphql),
        "gremlin" => Ok(Language::Gremlin),
        "sparql" => Ok(Language::Sparql),
        "sql" | "sql-pgq" => Ok(Language::SqlPgq),
        _ => Err(crate::error::ServiceError::BadRequest(format!(
            "unsupported language '{name}' (supported: {SUPPORTED_LANGUAGES})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determine_language_mapping() {
        let determine_language = |lang| determine_language(lang).unwrap();
        assert!(matches!(determine_language(None), Language::Gql));
        assert!(matches!(determine_language(Some("gql")), Language::Gql));
        assert!(matches!(
            determine_language(Some("cypher")),
            Language::Cypher
        ));
        assert!(matches!(
            determine_language(Some("graphql")),
            Language::Graphql
        ));
        assert!(matches!(
            determine_language(Some("gremlin")),
            Language::Gremlin
        ));
        assert!(matches!(
            determine_language(Some("sparql")),
            Language::Sparql
        ));
        assert!(matches!(determine_language(Some("sql")), Language::SqlPgq));
        assert!(matches!(
            determine_language(Some("sql-pgq")),
            Language::SqlPgq
        ));
        assert!(matches!(determine_language(Some("GQL")), Language::Gql));
        assert!(matches!(
            determine_language(Some("Cypher")),
            Language::Cypher
        ));
        assert!(matches!(
            determine_language(Some("SPARQL")),
            Language::Sparql
        ));
        assert!(matches!(
            determine_language(Some("SQL-PGQ")),
            Language::SqlPgq
        ));
    }

    #[test]
    fn unknown_languages_are_bad_requests() {
        for name in ["unknown", "aql", "cyper", "", "sql_pgq"] {
            let err = determine_language(Some(name)).unwrap_err();
            let crate::error::ServiceError::BadRequest(msg) = err else {
                panic!("expected a bad request for {name:?}, got {err:?}");
            };
            assert!(
                msg.contains(&format!("'{name}'")) && msg.contains(SUPPORTED_LANGUAGES),
                "{msg}"
            );
        }
    }

    #[test]
    fn metrics_record_and_render() {
        let m = Metrics::new();
        m.record_query(Language::Gql, 1_500); // 1.5ms
        m.record_query(Language::Gql, 2_500);
        m.record_query_error(Language::Cypher);

        let output = m.render(2, 100, 50, 3, 60, None);
        assert!(output.contains("grafeo_databases_total 2"));
        assert!(output.contains("grafeo_nodes_total 100"));
        assert!(output.contains("grafeo_edges_total 50"));
        assert!(output.contains("grafeo_active_sessions_total 3"));
        assert!(output.contains("grafeo_uptime_seconds 60"));
        assert!(output.contains("grafeo_queries_total{language=\"gql\"} 2"));
        assert!(output.contains("grafeo_query_errors_total{language=\"cypher\"} 1"));
    }
}
