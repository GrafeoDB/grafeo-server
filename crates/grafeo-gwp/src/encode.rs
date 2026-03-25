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
        Value::Date(d) => GwpValue::Date(gwp::types::Date {
            year: d.year(),
            month: d.month(),
            day: d.day(),
        }),
        Value::Time(t) => {
            let local = gwp::types::LocalTime {
                hour: t.hour(),
                minute: t.minute(),
                second: t.second(),
                nanosecond: t.nanosecond(),
            };
            match t.offset_seconds() {
                Some(off) => GwpValue::ZonedTime(gwp::types::ZonedTime {
                    time: local,
                    offset_minutes: off / 60,
                }),
                None => GwpValue::LocalTime(local),
            }
        }
        Value::Duration(d) => GwpValue::Duration(gwp::types::Duration {
            months: d.months(),
            nanoseconds: d.days() * 86_400_000_000_000 + d.nanos(),
        }),
        Value::ZonedDatetime(zdt) => {
            let local_date = zdt.to_local_date();
            let local_time = zdt.to_local_time();
            GwpValue::ZonedDateTime(gwp::types::ZonedDateTime {
                date: gwp::types::Date {
                    year: local_date.year(),
                    month: local_date.month(),
                    day: local_date.day(),
                },
                time: gwp::types::LocalTime {
                    hour: local_time.hour(),
                    minute: local_time.minute(),
                    second: local_time.second(),
                    nanosecond: local_time.nanosecond(),
                },
                offset_minutes: zdt.offset_seconds() / 60,
            })
        }
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
        Value::Path { nodes, edges } => {
            let fields = vec![
                gwp::types::Field {
                    name: "nodes".to_string(),
                    value: GwpValue::List(nodes.iter().map(grafeo_to_gwp).collect()),
                },
                gwp::types::Field {
                    name: "edges".to_string(),
                    value: GwpValue::List(edges.iter().map(grafeo_to_gwp).collect()),
                },
            ];
            GwpValue::Record(gwp::types::Record { fields })
        }
        Value::GCounter(counts) => {
            let total: u64 = counts.values().sum();
            let mut fields: Vec<gwp::types::Field> = counts
                .iter()
                .map(|(k, v)| gwp::types::Field {
                    name: k.clone(),
                    value: GwpValue::Integer(*v as i64),
                })
                .collect();
            fields.sort_by(|a, b| a.name.cmp(&b.name));
            let replicas = GwpValue::Record(gwp::types::Record { fields });
            GwpValue::Record(gwp::types::Record {
                fields: vec![
                    gwp::types::Field {
                        name: "$gcounter".to_string(),
                        value: replicas,
                    },
                    gwp::types::Field {
                        name: "$value".to_string(),
                        value: GwpValue::Integer(total as i64),
                    },
                ],
            })
        }
        Value::OnCounter { pos, neg } => {
            let pos_sum: i64 = pos.values().copied().map(|v| v as i64).sum();
            let neg_sum: i64 = neg.values().copied().map(|v| v as i64).sum();
            GwpValue::Record(gwp::types::Record {
                fields: vec![
                    gwp::types::Field {
                        name: "$pncounter".to_string(),
                        value: GwpValue::Boolean(true),
                    },
                    gwp::types::Field {
                        name: "$value".to_string(),
                        value: GwpValue::Integer(pos_sum - neg_sum),
                    },
                ],
            })
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
        GwpValue::Record(rec) => {
            let map: std::collections::BTreeMap<_, _> = rec
                .fields
                .iter()
                .filter_map(|f| {
                    gwp_to_grafeo(&f.value).map(|gv| (grafeo_common::PropertyKey::new(&f.name), gv))
                })
                .collect();
            Some(Value::Map(std::sync::Arc::new(map)))
        }
        GwpValue::Date(d) => {
            grafeo_common::types::Date::from_ymd(d.year, d.month, d.day).map(Value::Date)
        }
        GwpValue::LocalTime(t) => {
            grafeo_common::types::Time::from_hms_nano(t.hour, t.minute, t.second, t.nanosecond)
                .map(Value::Time)
        }
        GwpValue::ZonedTime(zt) => grafeo_common::types::Time::from_hms_nano(
            zt.time.hour,
            zt.time.minute,
            zt.time.second,
            zt.time.nanosecond,
        )
        .map(|t| Value::Time(t.with_offset(zt.offset_minutes * 60))),
        GwpValue::Duration(d) => {
            let day_nanos = 86_400_000_000_000i64;
            let days = d.nanoseconds / day_nanos;
            let nanos = d.nanoseconds % day_nanos;
            Some(Value::Duration(grafeo_common::types::Duration::new(
                d.months, days, nanos,
            )))
        }
        // Graph types and datetime not yet supported as engine parameters
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
    fn gwp_to_grafeo_duration_supported() {
        let params = HashMap::from([(
            "dur".to_string(),
            GwpValue::Duration(gwp::types::Duration {
                months: 0,
                nanoseconds: 86_400_000_000_000,
            }),
        )]);
        let converted = convert_params(&params);
        assert_eq!(converted.len(), 1);
        assert!(matches!(converted.get("dur"), Some(Value::Duration(_))));
    }

    #[test]
    fn gwp_to_grafeo_record_to_map() {
        let rec = GwpValue::Record(gwp::types::Record {
            fields: vec![
                gwp::types::Field {
                    name: "src".to_string(),
                    value: GwpValue::String("person_0".to_string()),
                },
                gwp::types::Field {
                    name: "tgt".to_string(),
                    value: GwpValue::String("person_1".to_string()),
                },
                gwp::types::Field {
                    name: "weight".to_string(),
                    value: GwpValue::Float(1.5),
                },
            ],
        });
        let grafeo = gwp_to_grafeo(&rec).expect("Record should convert to Map");
        if let Value::Map(map) = grafeo {
            assert_eq!(map.len(), 3);
            assert!(matches!(
                map.get(&grafeo_common::PropertyKey::new("src")),
                Some(Value::String(s)) if s.as_str() == "person_0"
            ));
            assert!(matches!(
                map.get(&grafeo_common::PropertyKey::new("weight")),
                Some(Value::Float64(f)) if (*f - 1.5).abs() < f64::EPSILON
            ));
        } else {
            panic!("expected Value::Map");
        }
    }

    #[test]
    fn temporal_round_trip_date() {
        let date = grafeo_common::types::Date::from_ymd(2024, 6, 15).unwrap();
        let gwp = grafeo_to_gwp(&Value::Date(date));
        let GwpValue::Date(d) = &gwp else {
            panic!("expected GwpValue::Date");
        };
        assert_eq!((d.year, d.month, d.day), (2024, 6, 15));

        // Round-trip back to grafeo
        let grafeo = gwp_to_grafeo(&gwp).unwrap();
        assert_eq!(grafeo, Value::Date(date));
    }

    #[test]
    fn temporal_round_trip_local_time() {
        let time = grafeo_common::types::Time::from_hms_nano(14, 30, 45, 123_000_000).unwrap();
        let gwp = grafeo_to_gwp(&Value::Time(time));
        let GwpValue::LocalTime(t) = &gwp else {
            panic!("expected GwpValue::LocalTime, got {gwp:?}");
        };
        assert_eq!(
            (t.hour, t.minute, t.second, t.nanosecond),
            (14, 30, 45, 123_000_000)
        );

        let grafeo = gwp_to_grafeo(&gwp).unwrap();
        if let Value::Time(t) = grafeo {
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
        let gwp = grafeo_to_gwp(&Value::Time(time));
        let GwpValue::ZonedTime(zt) = &gwp else {
            panic!("expected GwpValue::ZonedTime, got {gwp:?}");
        };
        assert_eq!(zt.offset_minutes, 60);

        let grafeo = gwp_to_grafeo(&gwp).unwrap();
        if let Value::Time(t) = grafeo {
            assert_eq!(t.offset_seconds(), Some(3600));
        } else {
            panic!("expected Value::Time");
        }
    }

    #[test]
    fn temporal_duration_preserves_components() {
        let dur = grafeo_common::types::Duration::new(2, 10, 500_000_000);
        let gwp = grafeo_to_gwp(&Value::Duration(dur));
        let GwpValue::Duration(d) = &gwp else {
            panic!("expected GwpValue::Duration");
        };
        assert_eq!(d.months, 2);
        // GWP packs days into nanoseconds: 10 * 86_400_000_000_000 + 500_000_000
        let expected_nanos = 10 * 86_400_000_000_000i64 + 500_000_000;
        assert_eq!(d.nanoseconds, expected_nanos);

        // Round-trip: should reconstruct the same days and sub-day nanos
        let grafeo = gwp_to_grafeo(&gwp).unwrap();
        if let Value::Duration(rt) = grafeo {
            assert_eq!(rt.months(), 2);
            assert_eq!(rt.days(), 10);
            assert_eq!(rt.nanos(), 500_000_000);
        } else {
            panic!("expected Value::Duration");
        }
    }

    #[test]
    fn zoned_datetime_encodes_correctly() {
        let zdt = grafeo_common::types::ZonedDatetime::parse("2024-06-15T10:30:00+05:30").unwrap();
        let gwp = grafeo_to_gwp(&Value::ZonedDatetime(zdt));
        let GwpValue::ZonedDateTime(dt) = &gwp else {
            panic!("expected GwpValue::ZonedDateTime");
        };
        assert_eq!((dt.date.year, dt.date.month, dt.date.day), (2024, 6, 15));
        assert_eq!((dt.time.hour, dt.time.minute), (10, 30));
        assert_eq!(dt.offset_minutes, 330); // 5h30m = 330 minutes
    }

    #[test]
    fn path_encodes_as_record_with_nodes_and_edges() {
        let path = Value::Path {
            nodes: vec![Value::String("a".into()), Value::String("b".into())].into(),
            edges: vec![Value::String("e1".into())].into(),
        };
        let gwp = grafeo_to_gwp(&path);
        let GwpValue::Record(rec) = &gwp else {
            panic!("expected GwpValue::Record for path");
        };
        assert_eq!(rec.fields.len(), 2);
        assert_eq!(rec.fields[0].name, "nodes");
        assert_eq!(rec.fields[1].name, "edges");
        if let GwpValue::List(nodes) = &rec.fields[0].value {
            assert_eq!(nodes.len(), 2);
        } else {
            panic!("expected List for nodes");
        }
    }

    #[test]
    fn gwp_to_grafeo_list_of_records() {
        // This is the UNWIND $edges pattern: list of dicts
        let edges = GwpValue::List(vec![
            GwpValue::Record(gwp::types::Record {
                fields: vec![gwp::types::Field {
                    name: "id".to_string(),
                    value: GwpValue::String("a".to_string()),
                }],
            }),
            GwpValue::Record(gwp::types::Record {
                fields: vec![gwp::types::Field {
                    name: "id".to_string(),
                    value: GwpValue::String("b".to_string()),
                }],
            }),
        ]);
        let grafeo = gwp_to_grafeo(&edges).expect("List of records should convert");
        if let Value::List(items) = grafeo {
            assert_eq!(items.len(), 2);
            // Each item should be a Map
            assert!(matches!(&items[0], Value::Map(_)));
            assert!(matches!(&items[1], Value::Map(_)));
        } else {
            panic!("expected Value::List");
        }
    }
}
