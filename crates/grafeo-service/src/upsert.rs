//! Upserts by key (engine 0.5.44): create or update many nodes or edges in
//! one all-or-nothing statement.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use grafeo_common::types::{PropertyKey, Value};
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
            summary.map_err(|e| ServiceError::BadRequest(e.to_string()))
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
            summary.map_err(|e| ServiceError::BadRequest(e.to_string()))
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

/// A handle on a named graph of the current schema; 404 when it is missing.
fn graph_handle<'db>(db: &'db GrafeoDB, graph: &str) -> Result<GraphHandle<'db>, ServiceError> {
    db.graph(graph)
        .map_err(|_| ServiceError::NotFound(format!("graph '{graph}' not found")))
}

/// Converts request rows (plain JSON objects) to engine rows.
fn rows_from_json(
    rows: Vec<serde_json::Value>,
) -> Result<Vec<HashMap<PropertyKey, Value>>, ServiceError> {
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| match row {
            serde_json::Value::Object(fields) => Ok(fields
                .into_iter()
                .map(|(key, value)| (PropertyKey::new(key.as_str()), json_to_value(value)))
                .collect()),
            other => Err(ServiceError::BadRequest(format!(
                "row {index} is not a JSON object: {other}"
            ))),
        })
        .collect()
}

/// Converts plain JSON (`42`, `"Alix"`, `[1, 2]`, `{"a": 1}`) to an engine
/// value. Integers beyond `i64` become `Float64`.
fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || Value::Float64(n.as_f64().unwrap_or_default()),
            Value::Int64,
        ),
        serde_json::Value::String(s) => Value::from(s),
        serde_json::Value::Array(items) => Value::List(
            items
                .into_iter()
                .map(json_to_value)
                .collect::<Vec<_>>()
                .into(),
        ),
        serde_json::Value::Object(fields) => Value::Map(Arc::new(
            fields
                .into_iter()
                .map(|(key, value)| (PropertyKey::new(key.as_str()), json_to_value(value)))
                .collect::<BTreeMap<_, _>>(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rows_convert_to_engine_values() {
        let rows = rows_from_json(vec![serde_json::json!({
            "i": 7, "big": 18_446_744_073_709_551_615_u64, "f": 1.5, "s": "Alix",
            "b": true, "n": null, "list": [1, "two"], "map": {"k": 1}
        })])
        .unwrap();
        let row = &rows[0];
        let get = |k: &str| row[&PropertyKey::new(k)].clone();
        assert_eq!(get("i"), Value::Int64(7));
        assert_eq!(
            get("big"),
            Value::Float64(18_446_744_073_709_551_615_u64 as f64)
        );
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
