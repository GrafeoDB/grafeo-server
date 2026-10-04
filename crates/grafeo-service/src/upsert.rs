//! Upserts by key (engine 0.5.44): create or update many nodes or edges in
//! one all-or-nothing statement.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use grafeo_common::types::{PropertyKey, Value};
use grafeo_common::utils::error::{Error, QueryErrorKind, TransactionError};
use grafeo_engine::GrafeoDB;
use grafeo_engine::database::{EdgeUpsertOptions, GraphHandle, UpsertSummary};

use crate::database::DatabaseManager;
use crate::error::ServiceError;
use crate::types::{UpsertEdgesRequest, UpsertNodesRequest, UpsertResponse};

/// Upsert endpoints' service. Stateless.
pub struct UpsertService;

impl UpsertService {
    /// Creates or updates one node per row, matched by `key` and all labels.
    pub async fn upsert_nodes(
        databases: &DatabaseManager,
        db_name: &str,
        req: UpsertNodesRequest,
    ) -> Result<UpsertResponse, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }
        let entry = databases.get_available(db_name)?;
        let rows = rows_from_json(req.rows)?;
        run_blocking(move || {
            let db = entry.db();
            let labels: Vec<&str> = req.labels.iter().map(String::as_str).collect();
            let summary = match req.graph.as_deref() {
                None => db.upsert_nodes(&labels, &req.key, rows, req.replace),
                Some(graph) => {
                    graph_handle(&db, graph)?.upsert_nodes(&labels, &req.key, rows, req.replace)
                }
            };
            summary.map_err(upsert_error)
        })
        .await
    }

    /// Creates or updates one edge per row between the nodes its endpoint
    /// fields name; endpoints are never created.
    pub async fn upsert_edges(
        databases: &DatabaseManager,
        db_name: &str,
        req: UpsertEdgesRequest,
    ) -> Result<UpsertResponse, ServiceError> {
        if databases.is_read_only() {
            return Err(ServiceError::ReadOnly);
        }
        let entry = databases.get_available(db_name)?;
        let rows = rows_from_json(req.rows)?;
        let options = EdgeUpsertOptions {
            key: req.key,
            endpoint_key: req.endpoint_key,
            endpoint_labels: req.endpoint_labels,
            src_field: req.src_field,
            dst_field: req.dst_field,
            replace: req.replace,
        };
        run_blocking(move || {
            let db = entry.db();
            let summary = match req.graph.as_deref() {
                None => db.upsert_edges(&req.edge_type, rows, &options),
                Some(graph) => {
                    graph_handle(&db, graph)?.upsert_edges(&req.edge_type, rows, &options)
                }
            };
            summary.map_err(upsert_error)
        })
        .await
    }
}

/// Runs an upsert off the async runtime and converts its summary.
async fn run_blocking(
    task: impl FnOnce() -> Result<UpsertSummary, ServiceError> + Send + 'static,
) -> Result<UpsertResponse, ServiceError> {
    let summary = tokio::task::spawn_blocking(task)
        .await
        .map_err(|e| ServiceError::Internal(e.to_string()))??;
    Ok(UpsertResponse {
        created: summary.created,
        updated: summary.updated,
        skipped: summary.skipped,
        skipped_rows: summary.skipped_rows,
    })
}

/// A handle on a named graph of the current schema; 404 only when the graph
/// is missing (the engine reports that as a semantic query error), anything
/// else is an internal failure.
fn graph_handle<'db>(db: &'db GrafeoDB, graph: &str) -> Result<GraphHandle<'db>, ServiceError> {
    db.graph(graph).map_err(|e| match e {
        Error::Query(ref q) if q.kind == QueryErrorKind::Semantic => {
            ServiceError::NotFound(format!("graph '{graph}' not found"))
        }
        other => upsert_error(other),
    })
}

/// Maps an engine failure of an upsert: internal kinds (internal, I/O,
/// serialization and storage errors) are 500; a write-write conflict with a
/// concurrent transaction (or a serialization failure or deadlock) is 409,
/// which the caller can retry; constraint violations, query errors and bad
/// input stay 400, a timeout is a timeout.
fn upsert_error(error: Error) -> ServiceError {
    match error {
        Error::Internal(_) | Error::Io(_) | Error::Serialization(_) | Error::Storage(_) => {
            ServiceError::Internal(error.to_string())
        }
        Error::Transaction(
            TransactionError::WriteConflict(_)
            | TransactionError::Conflict
            | TransactionError::Aborted
            | TransactionError::SerializationFailure(_)
            | TransactionError::Deadlock,
        ) => ServiceError::Conflict(error.to_string()),
        Error::Transaction(TransactionError::Timeout) => ServiceError::Timeout,
        Error::Query(ref q) if q.kind == QueryErrorKind::Timeout => ServiceError::Timeout,
        Error::NodeNotFound(_)
        | Error::EdgeNotFound(_)
        | Error::PropertyNotFound(_)
        | Error::LabelNotFound(_)
        | Error::TypeMismatch { .. }
        | Error::InvalidValue(_)
        | Error::Transaction(_)
        | Error::Query(_) => ServiceError::BadRequest(error.to_string()),
        // `Error` is non-exhaustive: a variant added later is not known to be
        // the caller's fault.
        _ => ServiceError::Internal(error.to_string()),
    }
}

/// Converts request rows (plain JSON objects) to engine rows.
fn rows_from_json(
    rows: Vec<serde_json::Value>,
) -> Result<Vec<HashMap<PropertyKey, Value>>, ServiceError> {
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| match row {
            serde_json::Value::Object(fields) => fields
                .into_iter()
                .map(|(key, value)| {
                    let converted = json_to_value(value).map_err(|reason| {
                        ServiceError::BadRequest(format!("row {index}, property '{key}': {reason}"))
                    })?;
                    Ok((PropertyKey::new(key.as_str()), converted))
                })
                .collect(),
            other => Err(ServiceError::BadRequest(format!(
                "row {index} is not a JSON object: {other}"
            ))),
        })
        .collect()
}

/// Converts plain JSON (`42`, `"Alix"`, `[1, 2]`, `{"a": 1}`) to an engine
/// value. Accepted: integers in `i64` range (`Int64`) and other finite
/// numbers (`Float64`). Rejected: integers from `i64::MAX + 1` to `u64::MAX`,
/// because rounding them to `Float64` would make distinct keys collide.
/// Limitation: `serde_json` (without `arbitrary_precision`, which is not
/// enabled) parses integers below `i64::MIN` or above `u64::MAX` as floats,
/// so those cannot be told apart from real floats and still become `Float64`.
fn json_to_value(json: serde_json::Value) -> Result<Value, String> {
    Ok(match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int64(i)
            } else if let Some(u) = n.as_u64() {
                return Err(format!("integer {u} is out of range (max {})", i64::MAX));
            } else {
                match n.as_f64() {
                    Some(f) if f.is_finite() => Value::Float64(f),
                    // Defensive: only reachable with serde_json's `arbitrary_precision`.
                    _ => return Err(format!("number {n} is not representable")),
                }
            }
        }
        serde_json::Value::String(s) => Value::from(s),
        serde_json::Value::Array(items) => Value::List(
            items
                .into_iter()
                .map(json_to_value)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        serde_json::Value::Object(fields) => Value::Map(Arc::new(
            fields
                .into_iter()
                .map(|(key, value)| Ok((PropertyKey::new(key.as_str()), json_to_value(value)?)))
                .collect::<Result<BTreeMap<_, _>, String>>()?,
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::utils::error::{QueryError, StorageError};

    #[test]
    fn integers_beyond_i64_are_rejected_with_row_and_property() {
        let rows = vec![
            serde_json::json!({"id": 1}),
            serde_json::json!({"id": 2}),
            serde_json::json!({"id": 3}),
            serde_json::json!({"id": 18_446_744_073_709_551_615_u64}),
        ];
        let err = rows_from_json(rows).unwrap_err();
        let ServiceError::BadRequest(msg) = err else {
            panic!("expected BadRequest");
        };
        assert_eq!(
            msg,
            "row 3, property 'id': integer 18446744073709551615 is out of range \
             (max 9223372036854775807)"
        );
    }

    #[test]
    fn nested_out_of_range_integers_are_rejected() {
        let message = |row: serde_json::Value| match rows_from_json(vec![row]) {
            Err(ServiceError::BadRequest(msg)) => msg,
            other => panic!("expected BadRequest, got {other:?}"),
        };
        let in_list = message(serde_json::json!({"ids": [1, 9_223_372_036_854_775_808_u64]}));
        assert!(
            in_list.starts_with("row 0, property 'ids': integer 9223372036854775808"),
            "got: {in_list}"
        );
        let in_map = message(serde_json::json!({"m": {"k": 18_446_744_073_709_551_614_u64}}));
        assert!(
            in_map.starts_with("row 0, property 'm': integer 18446744073709551614"),
            "got: {in_map}"
        );
        let max = vec![serde_json::json!({"id": i64::MAX})];
        assert!(rows_from_json(max).is_ok());
    }

    #[test]
    fn engine_errors_map_to_service_errors() {
        let q = |kind| Error::Query(QueryError::new(kind, "x"));
        let tx = Error::Transaction;
        let table: Vec<(Error, &str)> = vec![
            (Error::Internal("x".into()), "internal"),
            (Error::Io(std::io::Error::other("x")), "internal"),
            (Error::Serialization("x".into()), "internal"),
            (Error::Storage(StorageError::Full), "internal"),
            (q(QueryErrorKind::Semantic), "bad_request"),
            (q(QueryErrorKind::Execution), "bad_request"),
            (q(QueryErrorKind::Syntax), "bad_request"),
            (q(QueryErrorKind::Timeout), "timeout"),
            (Error::InvalidValue("x".into()), "bad_request"),
            (Error::PropertyNotFound("x".into()), "bad_request"),
            (
                Error::TypeMismatch {
                    expected: "a".into(),
                    found: "b".into(),
                },
                "bad_request",
            ),
            (tx(TransactionError::WriteConflict("x".into())), "conflict"),
            (tx(TransactionError::Conflict), "conflict"),
            (tx(TransactionError::Aborted), "conflict"),
            (
                tx(TransactionError::SerializationFailure("x".into())),
                "conflict",
            ),
            (tx(TransactionError::Deadlock), "conflict"),
            (tx(TransactionError::Timeout), "timeout"),
            (tx(TransactionError::ReadOnly), "bad_request"),
            (
                tx(TransactionError::InvalidState("x".into())),
                "bad_request",
            ),
        ];
        for (error, expected) in table {
            let label = format!("{error:?}");
            let got = match upsert_error(error) {
                ServiceError::Internal(_) => "internal",
                ServiceError::BadRequest(_) => "bad_request",
                ServiceError::Conflict(_) => "conflict",
                ServiceError::Timeout => "timeout",
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(got, expected, "{label}");
        }
    }

    #[test]
    fn a_missing_graph_is_404() {
        let db = GrafeoDB::new_in_memory();
        let err = graph_handle(&db, "nope").map(|_| ()).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(ref m) if m.contains("nope")));
    }

    #[test]
    fn json_rows_convert_to_engine_values() {
        let rows = rows_from_json(vec![serde_json::json!({
            "i": 7, "neg": -3, "f": 1.5, "s": "Alix",
            "b": true, "n": null, "list": [1, "two"], "map": {"k": 1}
        })])
        .unwrap();
        let row = &rows[0];
        let get = |k: &str| row[&PropertyKey::new(k)].clone();
        assert_eq!(get("i"), Value::Int64(7));
        assert_eq!(get("neg"), Value::Int64(-3));
        assert_eq!(get("f"), Value::Float64(1.5));
        assert_eq!(get("s"), Value::from("Alix"));
        assert_eq!(get("b"), Value::Bool(true));
        assert_eq!(get("n"), Value::Null);
        assert_eq!(
            get("list"),
            Value::List(vec![Value::Int64(1), Value::from("two")].into())
        );
        let Value::Map(map) = get("map") else {
            panic!("expected a map");
        };
        assert_eq!(map.get(&PropertyKey::new("k")), Some(&Value::Int64(1)));
    }

    #[test]
    fn non_object_row_is_rejected_with_its_index() {
        let err =
            rows_from_json(vec![serde_json::json!({"a": 1}), serde_json::json!(5)]).unwrap_err();
        assert!(matches!(err, ServiceError::BadRequest(ref msg) if msg.contains("row 1")));
    }

    #[tokio::test]
    async fn upsert_is_rejected_on_a_read_only_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        drop(DatabaseManager::new(Some(path), false));
        let mgr = DatabaseManager::new(Some(path), true);
        let req = UpsertNodesRequest {
            labels: vec!["P".to_string()],
            key: "id".to_string(),
            rows: vec![serde_json::json!({"id": 1})],
            replace: false,
            graph: None,
        };
        let err = UpsertService::upsert_nodes(&mgr, "default", req)
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::ReadOnly));
    }
}
