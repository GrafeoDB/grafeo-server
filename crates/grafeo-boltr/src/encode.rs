//! Value conversion between `grafeo_common::Value` and `boltr::types::BoltValue`.

use std::collections::HashMap;

use boltr::types::{BoltDict, BoltValue};

/// Converts a Grafeo value to a Bolt value.
pub fn grafeo_to_bolt(value: &grafeo_common::Value) -> BoltValue {
    use grafeo_common::Value;
    match value {
        Value::Null => BoltValue::Null,
        Value::Bool(b) => BoltValue::Boolean(*b),
        Value::Int64(i) => BoltValue::Integer(*i),
        Value::Float64(f) => BoltValue::Float(*f),
        Value::String(s) => BoltValue::String(s.to_string()),
        Value::Bytes(b) => BoltValue::Bytes(b.to_vec()),
        Value::Timestamp(t) => BoltValue::String(format!("{t:?}")),
        Value::List(items) => BoltValue::List(items.iter().map(grafeo_to_bolt).collect()),
        Value::Map(map) => {
            let dict: BoltDict = map
                .iter()
                .map(|(k, v)| (k.to_string(), grafeo_to_bolt(v)))
                .collect();
            BoltValue::Dict(dict)
        }
        Value::Vector(v) => {
            BoltValue::List(v.iter().map(|f| BoltValue::Float(f64::from(*f))).collect())
        }
    }
}

/// Converts a Bolt value to a Grafeo value. Returns `None` for unsupported types.
fn bolt_to_grafeo(value: &BoltValue) -> Option<grafeo_common::Value> {
    use grafeo_common::Value;
    match value {
        BoltValue::Null => Some(Value::Null),
        BoltValue::Boolean(b) => Some(Value::Bool(*b)),
        BoltValue::Integer(i) => Some(Value::Int64(*i)),
        BoltValue::Float(f) => Some(Value::Float64(*f)),
        BoltValue::String(s) => Some(Value::String(s.as_str().into())),
        BoltValue::Bytes(b) => Some(Value::Bytes(b.clone().into())),
        BoltValue::List(items) => {
            let converted: Vec<_> = items.iter().filter_map(bolt_to_grafeo).collect();
            Some(Value::List(converted.into()))
        }
        BoltValue::Dict(dict) => {
            let map: std::collections::BTreeMap<_, _> = dict
                .iter()
                .filter_map(|(k, v)| {
                    bolt_to_grafeo(v)
                        .map(|gv| (grafeo_common::PropertyKey::new(k.as_str()), gv))
                })
                .collect();
            Some(Value::Map(std::sync::Arc::new(map)))
        }
        // Graph structures and temporals are not supported as query parameters.
        _ => None,
    }
}

/// Converts a Bolt value to a Grafeo value, returning `Null` for unsupported types.
#[allow(dead_code)]
pub fn bolt_to_grafeo_common(value: &BoltValue) -> grafeo_common::Value {
    bolt_to_grafeo(value).unwrap_or(grafeo_common::Value::Null)
}

/// Converts a map of Bolt parameter values to Grafeo values.
pub fn convert_params(
    params: &HashMap<String, BoltValue>,
) -> HashMap<String, grafeo_common::Value> {
    params
        .iter()
        .filter_map(|(k, v)| bolt_to_grafeo(v).map(|gv| (k.clone(), gv)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grafeo_to_bolt_primitives() {
        assert_eq!(grafeo_to_bolt(&grafeo_common::Value::Null), BoltValue::Null);
        assert_eq!(
            grafeo_to_bolt(&grafeo_common::Value::Bool(true)),
            BoltValue::Boolean(true),
        );
        assert_eq!(
            grafeo_to_bolt(&grafeo_common::Value::Int64(42)),
            BoltValue::Integer(42),
        );
        assert_eq!(
            grafeo_to_bolt(&grafeo_common::Value::Float64(1.23)),
            BoltValue::Float(1.23),
        );
    }

    #[test]
    fn grafeo_to_bolt_string() {
        let val = grafeo_common::Value::String("hello".into());
        assert_eq!(grafeo_to_bolt(&val), BoltValue::String("hello".into()));
    }

    #[test]
    fn grafeo_to_bolt_vector() {
        let val = grafeo_common::Value::Vector(vec![1.0f32, 2.0, 3.0].into());
        let bolt = grafeo_to_bolt(&val);
        assert_eq!(
            bolt,
            BoltValue::List(vec![
                BoltValue::Float(1.0),
                BoltValue::Float(2.0),
                BoltValue::Float(3.0),
            ]),
        );
    }

    #[test]
    fn bolt_to_grafeo_roundtrip() {
        let bolt = BoltValue::String("test".into());
        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        assert_eq!(grafeo, grafeo_common::Value::String("test".into()));

        let bolt = BoltValue::Integer(99);
        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        assert_eq!(grafeo, grafeo_common::Value::Int64(99));
    }

    #[test]
    fn bolt_to_grafeo_unsupported_returns_none() {
        let bolt = BoltValue::Date(boltr::types::BoltDate { days: 100 });
        assert!(bolt_to_grafeo(&bolt).is_none());
    }

    #[test]
    fn convert_params_filters_unsupported() {
        let mut params = HashMap::new();
        params.insert("name".into(), BoltValue::String("Alice".into()));
        params.insert("date".into(), BoltValue::Date(boltr::types::BoltDate { days: 1 }));

        let result = convert_params(&params);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("name"));
    }
}
