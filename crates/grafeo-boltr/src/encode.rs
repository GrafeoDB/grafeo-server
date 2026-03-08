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
        Value::Date(d) => BoltValue::Date(boltr::types::BoltDate {
            days: i64::from(d.as_days()),
        }),
        Value::Time(t) => match t.offset_seconds() {
            Some(off) => BoltValue::Time(boltr::types::BoltTime {
                nanoseconds: t.as_nanos() as i64,
                tz_offset_seconds: i64::from(off),
            }),
            None => BoltValue::LocalTime(boltr::types::BoltLocalTime {
                nanoseconds: t.as_nanos() as i64,
            }),
        },
        Value::Duration(d) => {
            let total_nanos = d.nanos();
            let secs = total_nanos / 1_000_000_000;
            let nanos = total_nanos % 1_000_000_000;
            BoltValue::Duration(boltr::types::BoltDuration {
                months: d.months(),
                days: d.days(),
                seconds: secs,
                nanoseconds: nanos,
            })
        }
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
        Value::ZonedDatetime(zdt) => {
            let local_date = zdt.to_local_date();
            let local_time = zdt.to_local_time();
            let epoch_days = i64::from(local_date.as_days());
            let nanos_of_day = local_time.as_nanos() as i64;
            let epoch_seconds = epoch_days * 86400 + nanos_of_day / 1_000_000_000;
            let nanoseconds = nanos_of_day % 1_000_000_000;
            BoltValue::DateTime(boltr::types::BoltDateTime {
                seconds: epoch_seconds,
                nanoseconds,
                tz_offset_seconds: i64::from(zdt.offset_seconds()),
            })
        }
        Value::Path { nodes, edges } => {
            let dict: BoltDict = vec![
                (
                    "nodes".to_string(),
                    BoltValue::List(nodes.iter().map(grafeo_to_bolt).collect()),
                ),
                (
                    "edges".to_string(),
                    BoltValue::List(edges.iter().map(grafeo_to_bolt).collect()),
                ),
            ]
            .into_iter()
            .collect();
            BoltValue::Dict(dict)
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
                    bolt_to_grafeo(v).map(|gv| (grafeo_common::PropertyKey::new(k.as_str()), gv))
                })
                .collect();
            Some(Value::Map(std::sync::Arc::new(map)))
        }
        BoltValue::Date(d) => Some(Value::Date(grafeo_common::types::Date::from_days(
            d.days as i32,
        ))),
        BoltValue::Time(t) => grafeo_common::types::Time::from_nanos(t.nanoseconds as u64)
            .map(|time| Value::Time(time.with_offset(t.tz_offset_seconds as i32))),
        BoltValue::LocalTime(t) => {
            grafeo_common::types::Time::from_nanos(t.nanoseconds as u64).map(Value::Time)
        }
        BoltValue::Duration(d) => Some(Value::Duration(grafeo_common::types::Duration::new(
            d.months,
            d.days,
            d.seconds * 1_000_000_000 + d.nanoseconds,
        ))),
        // Graph structures and datetime not yet supported as query parameters
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
        // Spatial types are still unsupported
        let bolt = BoltValue::Point2D(boltr::types::BoltPoint2D {
            srid: 4326,
            x: 1.0,
            y: 2.0,
        });
        assert!(bolt_to_grafeo(&bolt).is_none());
    }

    #[test]
    fn bolt_to_grafeo_date_roundtrip() {
        let bolt = BoltValue::Date(boltr::types::BoltDate { days: 100 });
        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        assert!(matches!(grafeo, grafeo_common::Value::Date(_)));
    }

    #[test]
    fn temporal_round_trip_date() {
        let date = grafeo_common::types::Date::from_ymd(2024, 6, 15).unwrap();
        let bolt = grafeo_to_bolt(&grafeo_common::Value::Date(date));
        let BoltValue::Date(d) = &bolt else {
            panic!("expected BoltValue::Date");
        };
        assert_eq!(d.days, i64::from(date.as_days()));

        // Round-trip back
        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        assert_eq!(grafeo, grafeo_common::Value::Date(date));
    }

    #[test]
    fn temporal_round_trip_local_time() {
        let time = grafeo_common::types::Time::from_hms_nano(14, 30, 45, 0).unwrap();
        let bolt = grafeo_to_bolt(&grafeo_common::Value::Time(time));
        let BoltValue::LocalTime(lt) = &bolt else {
            panic!("expected BoltValue::LocalTime, got {bolt:?}");
        };
        assert_eq!(lt.nanoseconds, time.as_nanos() as i64);

        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        if let grafeo_common::Value::Time(t) = grafeo {
            assert_eq!((t.hour(), t.minute(), t.second()), (14, 30, 45));
            assert!(t.offset_seconds().is_none());
        } else {
            panic!("expected Value::Time");
        }
    }

    #[test]
    fn temporal_round_trip_zoned_time() {
        let time = grafeo_common::types::Time::from_hms(10, 0, 0)
            .unwrap()
            .with_offset(3600); // +01:00
        let bolt = grafeo_to_bolt(&grafeo_common::Value::Time(time));
        let BoltValue::Time(bt) = &bolt else {
            panic!("expected BoltValue::Time, got {bolt:?}");
        };
        assert_eq!(bt.tz_offset_seconds, 3600);

        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        if let grafeo_common::Value::Time(t) = grafeo {
            assert_eq!(t.offset_seconds(), Some(3600));
        } else {
            panic!("expected Value::Time");
        }
    }

    #[test]
    fn temporal_round_trip_duration() {
        let dur = grafeo_common::types::Duration::new(2, 10, 500_000_000);
        let bolt = grafeo_to_bolt(&grafeo_common::Value::Duration(dur));
        let BoltValue::Duration(bd) = &bolt else {
            panic!("expected BoltValue::Duration");
        };
        assert_eq!(bd.months, 2);
        assert_eq!(bd.days, 10);
        // 500_000_000 nanos = 0 seconds + 500_000_000 nanos
        assert_eq!(bd.seconds, 0);
        assert_eq!(bd.nanoseconds, 500_000_000);

        // Round-trip back
        let grafeo = bolt_to_grafeo(&bolt).unwrap();
        if let grafeo_common::Value::Duration(rt) = grafeo {
            assert_eq!((rt.months(), rt.days(), rt.nanos()), (2, 10, 500_000_000));
        } else {
            panic!("expected Value::Duration");
        }
    }

    #[test]
    fn zoned_datetime_encodes_to_bolt_datetime() {
        let zdt = grafeo_common::types::ZonedDatetime::parse("2024-06-15T10:30:00+05:30").unwrap();
        let bolt = grafeo_to_bolt(&grafeo_common::Value::ZonedDatetime(zdt));
        let BoltValue::DateTime(dt) = &bolt else {
            panic!("expected BoltValue::DateTime");
        };
        assert_eq!(dt.tz_offset_seconds, 19800); // 5h30m in seconds
        // Verify the epoch math: seconds + nanoseconds should reconstruct the local time
        assert_eq!(dt.nanoseconds, 0); // no sub-second component
    }

    #[test]
    fn path_encodes_as_dict() {
        let path = grafeo_common::Value::Path {
            nodes: vec![
                grafeo_common::Value::String("n1".into()),
                grafeo_common::Value::String("n2".into()),
            ]
            .into(),
            edges: vec![grafeo_common::Value::String("e1".into())].into(),
        };
        let bolt = grafeo_to_bolt(&path);
        let BoltValue::Dict(dict) = &bolt else {
            panic!("expected BoltValue::Dict for path");
        };
        assert!(dict.get("nodes").is_some());
        assert!(dict.get("edges").is_some());
    }

    #[test]
    fn convert_params_includes_temporal() {
        let mut params = HashMap::new();
        params.insert("name".into(), BoltValue::String("Alice".into()));
        params.insert(
            "date".into(),
            BoltValue::Date(boltr::types::BoltDate { days: 1 }),
        );

        let result = convert_params(&params);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("name"));
        assert!(result.contains_key("date"));
    }
}
