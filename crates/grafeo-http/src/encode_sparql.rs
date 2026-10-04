//! SPARQL-specific response encoding.
//!
//! Implements [W3C SPARQL 1.1 Query Results JSON Format](https://www.w3.org/TR/sparql11-results-json/)
//! for SELECT and ASK query results.

use axum::body::Body;
use axum::response::Response;
use grafeo_engine::database::QueryResult;

use crate::encode::value_to_json;

/// Encodes a `QueryResult` as W3C SPARQL Results JSON.
///
/// SELECT responses use the `head`/`results`/`bindings` structure.
/// ASK responses use the `head`/`boolean` structure (detected by a single
/// unnamed boolean column).
pub fn sparql_results_json_response(result: QueryResult) -> Response<Body> {
    let json = sparql_results_json(&result);
    Response::builder()
        .header("content-type", "application/sparql-results+json")
        .body(Body::from(json))
        .expect("response builder with valid header is infallible")
}

/// Serializes a `QueryResult` to the W3C SPARQL Results JSON string.
fn sparql_results_json(result: &QueryResult) -> String {
    // Detect ASK queries: single boolean column.
    if is_ask_result(result) {
        let boolean = result
            .rows()
            .first()
            .and_then(|row| row.first())
            .and_then(|v| {
                if let grafeo_common::Value::Bool(b) = *v {
                    Some(b)
                } else {
                    None
                }
            })
            .unwrap_or(false);

        return serde_json::json!({
            "head": {},
            "boolean": boolean
        })
        .to_string();
    }

    // SELECT result: build bindings array.
    let mut bindings = Vec::with_capacity(result.rows().len());
    for row in result.rows() {
        let mut binding = serde_json::Map::new();
        for (i, value) in row.iter().enumerate() {
            if matches!(value, grafeo_common::Value::Null) {
                continue; // Unbound variables are omitted per spec.
            }
            if let Some(col_name) = result.columns.get(i) {
                binding.insert(col_name.clone(), sparql_term_json(value));
            }
        }
        bindings.push(serde_json::Value::Object(binding));
    }

    serde_json::json!({
        "head": {
            "vars": result.columns
        },
        "results": {
            "bindings": bindings
        }
    })
    .to_string()
}

/// Converts a single value to a SPARQL Results JSON term.
///
/// Per the spec, each binding value has `type`, `value`, and optionally
/// `datatype` or `xml:lang`.
fn sparql_term_json(value: &grafeo_common::Value) -> serde_json::Value {
    use grafeo_common::Value;
    match value {
        Value::String(s) => {
            let s_str = s.to_string();
            // Detect IRIs (heuristic: starts with known scheme or < bracket).
            if s_str.starts_with("http://")
                || s_str.starts_with("https://")
                || s_str.starts_with("urn:")
            {
                serde_json::json!({ "type": "uri", "value": s_str })
            } else if let Some(bnode_id) = s_str.strip_prefix("_:") {
                serde_json::json!({ "type": "bnode", "value": bnode_id })
            } else {
                serde_json::json!({ "type": "literal", "value": s_str })
            }
        }
        Value::Int64(i) => serde_json::json!({
            "type": "literal",
            "value": i.to_string(),
            "datatype": "http://www.w3.org/2001/XMLSchema#integer"
        }),
        Value::Float64(f) => serde_json::json!({
            "type": "literal",
            "value": f.to_string(),
            "datatype": "http://www.w3.org/2001/XMLSchema#double"
        }),
        Value::Bool(b) => serde_json::json!({
            "type": "literal",
            "value": b.to_string(),
            "datatype": "http://www.w3.org/2001/XMLSchema#boolean"
        }),
        _ => {
            // Fallback: use the generic JSON conversion.
            let json_val = value_to_json(value);
            serde_json::json!({ "type": "literal", "value": json_val.to_string() })
        }
    }
}

/// Detects if a `QueryResult` represents an ASK query.
///
/// ASK queries produce a single row with a single boolean column.
fn is_ask_result(result: &QueryResult) -> bool {
    if result.columns.len() != 1 || result.rows().len() > 1 {
        return false;
    }
    // Check if the single column is a boolean.
    result
        .rows()
        .first()
        .and_then(|row| row.first())
        .is_some_and(|v| matches!(v, grafeo_common::Value::Bool(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::Value;

    fn make_select_result() -> QueryResult {
        QueryResult::from_rows(
            vec!["s".to_string(), "name".to_string()],
            vec![
                vec![
                    Value::String("http://example.org/alix".into()),
                    Value::String("Alix".into()),
                ],
                vec![
                    Value::String("http://example.org/gus".into()),
                    Value::String("Gus".into()),
                ],
            ],
        )
        .unwrap()
        .with_metrics(1.0, 2)
    }

    #[test]
    fn select_result_has_correct_structure() {
        let result = make_select_result();
        let json_str = sparql_results_json(&result);
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(json["head"]["vars"], serde_json::json!(["s", "name"]));
        let bindings = json["results"]["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 2);

        // First binding: IRI subject.
        assert_eq!(bindings[0]["s"]["type"], "uri");
        assert_eq!(bindings[0]["s"]["value"], "http://example.org/alix");
        // String literal.
        assert_eq!(bindings[0]["name"]["type"], "literal");
        assert_eq!(bindings[0]["name"]["value"], "Alix");
    }

    #[test]
    fn ask_result_has_boolean() {
        let result =
            QueryResult::from_rows(vec!["_ask".to_string()], vec![vec![Value::Bool(true)]])
                .unwrap();
        let json_str = sparql_results_json(&result);
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(json["boolean"], true);
        assert!(json["results"].is_null());
    }

    #[test]
    fn null_values_omitted_from_bindings() {
        let result = QueryResult::from_rows(
            vec!["s".to_string(), "o".to_string()],
            vec![vec![
                Value::String("http://example.org/s".into()),
                Value::Null,
            ]],
        )
        .unwrap();
        let json_str = sparql_results_json(&result);
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let binding = &json["results"]["bindings"][0];
        assert!(binding["s"].is_object());
        assert!(binding["o"].is_null()); // Omitted, so null in JSON lookup.
    }

    #[test]
    fn numeric_literals_have_datatype() {
        let result =
            QueryResult::from_rows(vec!["count".to_string()], vec![vec![Value::Int64(42)]])
                .unwrap();
        let json_str = sparql_results_json(&result);
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let binding = &json["results"]["bindings"][0]["count"];
        assert_eq!(binding["type"], "literal");
        assert_eq!(binding["value"], "42");
        assert_eq!(
            binding["datatype"],
            "http://www.w3.org/2001/XMLSchema#integer"
        );
    }
}
