//! Value encoding: bridges grafeo-service types to HTTP JSON types.

use std::collections::HashMap;

use grafeo_engine::database::QueryResult;

use crate::error::ApiError;
use crate::types::QueryResponse;

/// Converts a Grafeo `Value` to a JSON value.
pub fn value_to_json(value: &grafeo_common::Value) -> serde_json::Value {
    use grafeo_common::Value;
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int64(i) => serde_json::json!(i),
        Value::Float64(f) => serde_json::json!(f),
        Value::String(s) => serde_json::Value::String(s.to_string()),
        Value::Bytes(b) => serde_json::json!(b.as_ref()),
        Value::Timestamp(t) => serde_json::Value::String(format!("{t:?}")),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.to_string(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Vector(v) => serde_json::json!(v.as_ref()),
    }
}

/// Converts an engine `QueryResult` to an HTTP `QueryResponse` with JSON values.
pub fn query_result_to_response(result: &QueryResult) -> QueryResponse {
    QueryResponse {
        columns: result.columns.clone(),
        rows: result
            .rows
            .iter()
            .map(|row| row.iter().map(value_to_json).collect())
            .collect(),
        execution_time_ms: result.execution_time_ms,
        rows_scanned: result.rows_scanned,
    }
}

/// Converts optional JSON params to engine param format.
pub fn convert_json_params(
    params: Option<&serde_json::Value>,
) -> Result<Option<HashMap<String, grafeo_common::Value>>, ApiError> {
    match params {
        Some(v) => {
            let map: HashMap<String, grafeo_common::Value> = serde_json::from_value(v.clone())
                .map_err(|e| ApiError::bad_request(format!("invalid params: {e}")))?;
            Ok(Some(map))
        }
        None => Ok(None),
    }
}
