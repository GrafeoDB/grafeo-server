//! Value conversion: grafeo_common::Value <-> gwp::Value

use std::collections::HashMap;

use gwp::types::Value as GwpValue;

/// Converts a grafeo-engine `Value` to a GWP `Value`.
pub fn grafeo_to_gwp(value: &grafeo_common::Value) -> GwpValue {
    use grafeo_common::Value;
    match value {
        Value::Null => GwpValue::Null,
        Value::Bool(b) => GwpValue::Boolean(*b),
        Value::Int64(i) => GwpValue::Integer(*i),
        Value::Float64(f) => GwpValue::Float(*f),
        Value::String(s) => GwpValue::String(s.to_string()),
        Value::Bytes(b) => GwpValue::Bytes(b.to_vec()),
        Value::Timestamp(t) => GwpValue::String(format!("{t:?}")),
        Value::List(items) => GwpValue::List(items.iter().map(grafeo_to_gwp).collect()),
        Value::Map(map) => {
            let fields: Vec<gwp::types::Field> = map
                .iter()
                .map(|(k, v)| gwp::types::Field {
                    name: k.to_string(),
                    value: grafeo_to_gwp(v),
                })
                .collect();
            GwpValue::Record(gwp::types::Record { fields })
        }
        Value::Vector(v) => {
            GwpValue::List(v.iter().map(|f| GwpValue::Float(f64::from(*f))).collect())
        }
    }
}

/// Converts GWP parameters to grafeo-engine parameters.
pub fn convert_params(params: &HashMap<String, GwpValue>) -> HashMap<String, grafeo_common::Value> {
    params
        .iter()
        .filter_map(|(k, v)| gwp_to_grafeo(v).map(|gv| (k.clone(), gv)))
        .collect()
}

/// Converts a GWP `Value` to a `grafeo_common::Value`, using `Null` for
/// unsupported types. Used by the search filter conversion path.
pub fn gwp_to_grafeo_common(value: &GwpValue) -> grafeo_common::Value {
    gwp_to_grafeo(value).unwrap_or(grafeo_common::Value::Null)
}

/// Converts a GWP `Value` to a grafeo-engine `Value`.
/// Returns None for types that grafeo-engine doesn't support as parameters.
fn gwp_to_grafeo(value: &GwpValue) -> Option<grafeo_common::Value> {
    use grafeo_common::Value;
    match value {
        GwpValue::Null => Some(Value::Null),
        GwpValue::Boolean(b) => Some(Value::Bool(*b)),
        GwpValue::Integer(i) => Some(Value::Int64(*i)),
        GwpValue::UnsignedInteger(u) => Some(Value::Int64(*u as i64)),
        GwpValue::Float(f) => Some(Value::Float64(*f)),
        GwpValue::String(s) => Some(Value::String(s.as_str().into())),
        GwpValue::Bytes(b) => Some(Value::Bytes(b.clone().into())),
        GwpValue::List(items) => {
            let converted: Vec<_> = items.iter().filter_map(gwp_to_grafeo).collect();
            Some(Value::List(converted.into()))
        }
        // Temporal and graph types: not supported as engine parameters
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::Value;

    #[test]
    fn grafeo_to_gwp_primitives() {
        assert!(matches!(grafeo_to_gwp(&Value::Null), GwpValue::Null));
        assert!(matches!(
            grafeo_to_gwp(&Value::Bool(true)),
            GwpValue::Boolean(true)
        ));
        assert!(matches!(
            grafeo_to_gwp(&Value::Int64(42)),
            GwpValue::Integer(42)
        ));
        assert!(matches!(
            grafeo_to_gwp(&Value::Float64(3.14)),
            GwpValue::Float(f) if (f - 3.14).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn grafeo_to_gwp_string() {
        let val = grafeo_to_gwp(&Value::String("hello".into()));
        assert!(matches!(val, GwpValue::String(s) if s == "hello"));
    }

    #[test]
    fn grafeo_to_gwp_list() {
        let list = Value::List(vec![Value::Int64(1), Value::Int64(2)].into());
        let gwp = grafeo_to_gwp(&list);
        if let GwpValue::List(items) = gwp {
            assert_eq!(items.len(), 2);
        } else {
            panic!("expected GwpValue::List");
        }
    }

    #[test]
    fn grafeo_to_gwp_vector() {
        let vec = Value::Vector(vec![1.0f32, 2.0].into());
        let gwp = grafeo_to_gwp(&vec);
        if let GwpValue::List(items) = gwp {
            assert_eq!(items.len(), 2);
            assert!(matches!(items[0], GwpValue::Float(f) if (f - 1.0).abs() < f64::EPSILON));
        } else {
            panic!("expected GwpValue::List for vector");
        }
    }

    #[test]
    fn gwp_to_grafeo_roundtrip() {
        let params = HashMap::from([
            ("str".to_string(), GwpValue::String("hello".to_string())),
            ("num".to_string(), GwpValue::Integer(42)),
            ("flag".to_string(), GwpValue::Boolean(true)),
        ]);
        let converted = convert_params(&params);
        assert_eq!(converted.len(), 3);
        assert!(matches!(
            converted.get("str"),
            Some(Value::String(s)) if s.as_str() == "hello"
        ));
        assert!(matches!(converted.get("num"), Some(Value::Int64(42))));
        assert!(matches!(converted.get("flag"), Some(Value::Bool(true))));
    }

    #[test]
    fn gwp_to_grafeo_unsupported_returns_none() {
        // Temporal types are not supported — should be filtered out
        let params = HashMap::from([(
            "time".to_string(),
            GwpValue::Duration(gwp::types::Duration {
                months: 0,
                nanoseconds: 86_400_000_000_000,
            }),
        )]);
        let converted = convert_params(&params);
        assert!(converted.is_empty());
    }
}
