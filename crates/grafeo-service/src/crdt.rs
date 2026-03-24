//! CRDT merge functions for conflict-free replicated data types.
//!
//! Operates on `Value::GCounter` and `Value::PnCounter` directly.
//!
//! ## GCounter
//!
//! A grow-only counter.  `Value::GCounter(HashMap<replica_id, count>)`.
//! Merge: per-replica **max** (grows monotonically).
//!
//! ## PnCounter
//!
//! A positive-negative counter.  `Value::PnCounter { pos, neg }` where
//! each map is `HashMap<replica_id, count>`.
//! Merge: per-replica max over positive AND negative maps separately.
//! Net value: `sum(pos) - sum(neg)`.

use std::collections::HashMap;

use grafeo_common::types::Value;

use crate::sync::CrdtOp;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Applies a CRDT operation to `current`, returning the new CRDT value.
///
/// `current` should be an existing `Value::GCounter` or `Value::PnCounter`,
/// or `Value::Null` (treated as an empty counter of the appropriate type).
pub fn apply_op(current: &Value, op: &CrdtOp) -> Value {
    match op {
        CrdtOp::GrowAdd { amount, replica_id } => apply_grow_add(current, replica_id, *amount),
        CrdtOp::Increment { amount, replica_id } => {
            apply_pn_increment(current, replica_id, *amount)
        }
    }
}

/// Merges two CRDT values of the same type.
///
/// Both `a` and `b` must be the same CRDT variant; mixing types returns
/// a copy of `a` unchanged.
///
/// Merging is commutative and idempotent:
/// `merge(a, b) == merge(b, a)` and `merge(a, a) == a`.
pub fn merge(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::GCounter(a_counts), Value::GCounter(b_counts)) => {
            let mut result = a_counts.clone();
            for (replica, count) in b_counts {
                let entry = result.entry(replica.clone()).or_insert(0);
                *entry = (*entry).max(*count);
            }
            Value::GCounter(result)
        }
        (
            Value::PnCounter { pos: a_pos, neg: a_neg },
            Value::PnCounter { pos: b_pos, neg: b_neg },
        ) => {
            let pos = merge_u64_maps(a_pos, b_pos);
            let neg = merge_u64_maps(a_neg, b_neg);
            Value::PnCounter { pos, neg }
        }
        // Mismatched types: return a unchanged.
        _ => a.clone(),
    }
}

/// Returns the logical integer value of a CRDT counter.
///
/// - `GCounter`: sum of all replica contributions.
/// - `PnCounter`: `sum(pos) - sum(neg)`.
/// - Any other value: 0.
pub fn read(value: &Value) -> i64 {
    match value {
        Value::GCounter(counts) => counts.values().copied().map(|v| v as i64).sum(),
        Value::PnCounter { pos, neg } => {
            let pos_sum: i64 = pos.values().copied().map(|v| v as i64).sum();
            let neg_sum: i64 = neg.values().copied().map(|v| v as i64).sum();
            pos_sum - neg_sum
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn apply_grow_add(current: &Value, replica_id: &str, amount: u64) -> Value {
    let mut counts = match current {
        Value::GCounter(c) => c.clone(),
        _ => HashMap::new(),
    };
    let entry = counts.entry(replica_id.to_string()).or_insert(0);
    *entry += amount;
    Value::GCounter(counts)
}

fn apply_pn_increment(current: &Value, replica_id: &str, amount: i64) -> Value {
    let (mut pos, mut neg) = match current {
        Value::PnCounter { pos, neg } => (pos.clone(), neg.clone()),
        _ => (HashMap::new(), HashMap::new()),
    };
    if amount >= 0 {
        let entry = pos.entry(replica_id.to_string()).or_insert(0);
        *entry += amount as u64;
    } else {
        let entry = neg.entry(replica_id.to_string()).or_insert(0);
        *entry += amount.unsigned_abs();
    }
    Value::PnCounter { pos, neg }
}

fn merge_u64_maps(a: &HashMap<String, u64>, b: &HashMap<String, u64>) -> HashMap<String, u64> {
    let mut result = a.clone();
    for (replica, count) in b {
        let entry = result.entry(replica.clone()).or_insert(0);
        *entry = (*entry).max(*count);
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::CrdtOp;

    #[test]
    fn gcounter_starts_at_zero() {
        let val = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 0,
            replica_id: "device-a".to_string(),
        });
        assert_eq!(read(&val), 0);
    }

    #[test]
    fn gcounter_increments() {
        let val = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 5,
            replica_id: "device-a".to_string(),
        });
        let val = apply_op(&val, &CrdtOp::GrowAdd {
            amount: 3,
            replica_id: "device-b".to_string(),
        });
        assert_eq!(read(&val), 8);
    }

    #[test]
    fn gcounter_merge_takes_max_per_replica() {
        let a = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 5,
            replica_id: "device-a".to_string(),
        });
        let b = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 3,
            replica_id: "device-a".to_string(),
        });
        let merged = merge(&a, &b);
        // Max per replica: device-a = 5.
        assert_eq!(read(&merged), 5);
    }

    #[test]
    fn gcounter_merge_commutative() {
        let a = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 7,
            replica_id: "r1".to_string(),
        });
        let b = apply_op(&Value::Null, &CrdtOp::GrowAdd {
            amount: 3,
            replica_id: "r2".to_string(),
        });
        assert_eq!(read(&merge(&a, &b)), read(&merge(&b, &a)));
    }

    #[test]
    fn pncounter_increment_and_decrement() {
        let val = apply_op(&Value::Null, &CrdtOp::Increment {
            amount: 10,
            replica_id: "device-a".to_string(),
        });
        let val = apply_op(&val, &CrdtOp::Increment {
            amount: -3,
            replica_id: "device-a".to_string(),
        });
        assert_eq!(read(&val), 7);
    }

    #[test]
    fn pncounter_merge_commutative() {
        let a = apply_op(&Value::Null, &CrdtOp::Increment {
            amount: 10,
            replica_id: "r1".to_string(),
        });
        let b = apply_op(&Value::Null, &CrdtOp::Increment {
            amount: -2,
            replica_id: "r2".to_string(),
        });
        assert_eq!(read(&merge(&a, &b)), read(&merge(&b, &a)));
    }
}
