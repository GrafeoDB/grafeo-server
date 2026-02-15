//! Value encoding: bridges grafeo-service types to HTTP JSON types.
//!
//! Includes [`StreamingQueryBody`] for incremental JSON encoding of
//! large query results, producing output byte-identical to the
//! materialized `QueryResponse` serialization.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use futures_util::Stream;
use grafeo_engine::database::QueryResult;
use grafeo_service::stream::DEFAULT_BATCH_SIZE;

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

// ---------------------------------------------------------------------------
// Streaming JSON body
// ---------------------------------------------------------------------------

/// Phase of the JSON streaming state machine.
enum JsonStreamPhase {
    /// Next poll yields the JSON prefix: `{"columns":[...],"rows":[`
    Prefix,
    /// Next poll encodes rows starting at `offset` into a JSON array chunk.
    Rows { offset: usize, needs_comma: bool },
    /// Next poll yields the closing fields and brace.
    Suffix,
    /// Stream exhausted.
    Done,
}

/// Streaming HTTP body that produces a `QueryResponse`-compatible JSON
/// document incrementally.
///
/// Each `poll_next` encodes at most `batch_size` rows, keeping peak
/// memory for the encoded output proportional to the batch size rather
/// than the total row count.  The concatenated output is byte-identical
/// to `serde_json::to_string(&QueryResponse { ... })`.
pub struct StreamingQueryBody {
    result: QueryResult,
    batch_size: usize,
    phase: JsonStreamPhase,
}

impl StreamingQueryBody {
    /// Creates a new streaming body from a `QueryResult`.
    pub fn new(result: QueryResult) -> Self {
        Self {
            result,
            batch_size: DEFAULT_BATCH_SIZE,
            phase: JsonStreamPhase::Prefix,
        }
    }
}

impl Stream for StreamingQueryBody {
    type Item = Result<String, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        match this.phase {
            JsonStreamPhase::Prefix => {
                let columns_json = serde_json::to_string(&this.result.columns)
                    .expect("column names are always serializable");
                let prefix = format!(r#"{{"columns":{columns_json},"rows":["#);

                this.phase = if this.result.rows.is_empty() {
                    JsonStreamPhase::Suffix
                } else {
                    JsonStreamPhase::Rows {
                        offset: 0,
                        needs_comma: false,
                    }
                };

                Poll::Ready(Some(Ok(prefix)))
            }

            JsonStreamPhase::Rows {
                offset,
                needs_comma,
            } => {
                let end = (offset + this.batch_size).min(this.result.rows.len());
                let mut buf = String::new();

                for (i, row) in this.result.rows[offset..end].iter().enumerate() {
                    if needs_comma || i > 0 {
                        buf.push(',');
                    }
                    let json_row: Vec<serde_json::Value> = row.iter().map(value_to_json).collect();
                    buf.push_str(
                        &serde_json::to_string(&json_row)
                            .expect("serde_json::Value is always serializable"),
                    );
                }

                this.phase = if end >= this.result.rows.len() {
                    JsonStreamPhase::Suffix
                } else {
                    JsonStreamPhase::Rows {
                        offset: end,
                        needs_comma: true,
                    }
                };

                Poll::Ready(Some(Ok(buf)))
            }

            JsonStreamPhase::Suffix => {
                let mut suffix = String::from("]");

                if let Some(ms) = this.result.execution_time_ms {
                    suffix.push_str(r#","execution_time_ms":"#);
                    // Use serde_json to match QueryResponse serialization format
                    let v = serde_json::json!(ms);
                    suffix.push_str(&v.to_string());
                }
                if let Some(scanned) = this.result.rows_scanned {
                    suffix.push_str(r#","rows_scanned":"#);
                    let v = serde_json::json!(scanned);
                    suffix.push_str(&v.to_string());
                }
                suffix.push('}');

                this.phase = JsonStreamPhase::Done;
                Poll::Ready(Some(Ok(suffix)))
            }

            JsonStreamPhase::Done => Poll::Ready(None),
        }
    }
}

/// Creates a streaming HTTP `Response<Body>` from a `QueryResult`.
///
/// The response produces JSON identical to `Json<QueryResponse>`, but
/// encoded incrementally in batches to reduce peak memory.
pub fn streaming_json_response(result: QueryResult) -> Response<Body> {
    let stream = StreamingQueryBody::new(result);
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from_stream(stream))
        .expect("response builder with valid header is infallible")
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

    // --- Streaming tests ---

    use futures_util::StreamExt;
    use grafeo_common::types::LogicalType;

    fn make_result(num_rows: usize) -> QueryResult {
        QueryResult {
            columns: vec!["x".to_string()],
            column_types: vec![LogicalType::Int64],
            rows: (0..num_rows)
                .map(|i| vec![Value::Int64(i as i64)])
                .collect(),
            execution_time_ms: Some(1.0),
            rows_scanned: Some(num_rows as u64),
        }
    }

    /// Collects all chunks from a `StreamingQueryBody` into a single string.
    async fn collect_stream(mut stream: StreamingQueryBody) -> String {
        let mut chunks = Vec::new();
        while let Some(Ok(chunk)) = stream.next().await {
            chunks.push(chunk);
        }
        chunks.join("")
    }

    #[tokio::test]
    async fn streaming_empty_result_produces_valid_json() {
        let result = make_result(0);
        let expected = serde_json::to_string(&query_result_to_response(&result)).unwrap();
        let actual = collect_stream(StreamingQueryBody::new(result)).await;
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn streaming_matches_materialized_output() {
        let result = make_result(5);
        let expected = serde_json::to_string(&query_result_to_response(&result)).unwrap();
        let actual = collect_stream(StreamingQueryBody::new(result)).await;
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn streaming_large_result_produces_multiple_chunks() {
        let result = make_result(2500);
        let mut stream = StreamingQueryBody::new(result);
        stream.batch_size = 1000;
        let mut chunk_count = 0;
        let mut full = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            chunk_count += 1;
            full.push_str(&chunk);
        }
        // Prefix + 3 row batches (1000+1000+500) + Suffix = 5 chunks
        assert_eq!(chunk_count, 5);
        // Verify valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(parsed["rows"].as_array().unwrap().len(), 2500);
    }
}
