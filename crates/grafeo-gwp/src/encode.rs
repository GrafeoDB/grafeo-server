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
