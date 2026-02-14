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
#[derive(Clone, Copy)]
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
    pub fn render(
        &self,
        databases_total: usize,
        nodes_total: usize,
        edges_total: usize,
        active_sessions: usize,
        uptime_seconds: u64,
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

        out
    }
}

fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    writeln!(out, "# HELP {name} {help}").unwrap();
    writeln!(out, "# TYPE {name} gauge").unwrap();
    writeln!(out, "{name} {value}").unwrap();
}

/// Map a language string to a `Language` enum variant.
pub fn determine_language(lang: Option<&str>) -> Language {
    match lang {
        Some("cypher") => Language::Cypher,
        Some("graphql") => Language::Graphql,
        Some("gremlin") => Language::Gremlin,
        Some("sparql") => Language::Sparql,
        Some("sql" | "sql-pgq") => Language::SqlPgq,
        _ => Language::Gql,
    }
}
