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

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::Value;

    #[test]
    fn value_to_json_primitives() {
        assert_eq!(value_to_json(&Value::Null), serde_json::Value::Null);
        assert_eq!(value_to_json(&Value::Bool(true)), serde_json::json!(true));
        assert_eq!(value_to_json(&Value::Int64(42)), serde_json::json!(42));
        assert_eq!(
            value_to_json(&Value::Float64(3.14)),
            serde_json::json!(3.14)
        );
        assert_eq!(
            value_to_json(&Value::String("hello".into())),
            serde_json::json!("hello")
        );
    }

    #[test]
    fn value_to_json_list() {
        let list = Value::List(vec![Value::Int64(1), Value::Int64(2)].into());
        assert_eq!(value_to_json(&list), serde_json::json!([1, 2]));
    }

    #[test]
    fn value_to_json_map() {
        use std::collections::BTreeMap;
        use std::sync::Arc;
        let mut m = BTreeMap::new();
        m.insert("key".into(), Value::String("val".into()));
        let map = Value::Map(Arc::new(m));
        let json = value_to_json(&map);
        assert_eq!(json["key"], "val");
    }

    #[test]
    fn value_to_json_vector() {
        let vec = Value::Vector(vec![1.0f32, 2.0, 3.0].into());
        let json = value_to_json(&vec);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn query_result_to_response_basic() {
        let result = QueryResult {
            columns: vec!["name".to_string()],
            column_types: vec![grafeo_common::types::LogicalType::String],
            rows: vec![vec![Value::String("Alice".into())]],
            execution_time_ms: Some(1.5),
            rows_scanned: Some(10),
        };
        let resp = query_result_to_response(&result);
        assert_eq!(resp.columns, vec!["name"]);
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(resp.rows[0][0], serde_json::json!("Alice"));
        assert_eq!(resp.execution_time_ms, Some(1.5));
        assert_eq!(resp.rows_scanned, Some(10));
    }

    #[test]
    fn convert_json_params_none() {
        assert!(convert_json_params(None).unwrap().is_none());
    }
}
