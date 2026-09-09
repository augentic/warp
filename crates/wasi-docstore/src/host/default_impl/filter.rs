//! Pure JSON evaluation of [`FilterTree`](crate::host::resource::FilterTree)
//! for the default in-memory backend.
//!
//! Semantics follow the backend-portable filter contract:
//! - Field paths are dotted (`"a.b"` descends into nested objects); a missing
//!   field reads as JSON `null`, so `is-null` matches absent fields and
//!   `ne` matches documents lacking the field.
//! - Ordering comparisons (`gt`/`gte`/`lt`/`lte`) only match when the stored
//!   value and the target are of comparable types (numbers with numbers,
//!   strings with strings); `eq`/`ne` treat incomparable types as unequal.
//! - `binary` scalars never match JSON-derived values (JSON has no binary
//!   type), mirroring the previous default backend.

use std::cmp::Ordering;

use serde_json::Value;

use crate::host::generated::wasi::docstore::types::{ComparisonOp, ScalarValue, SortField};
use crate::host::resource::FilterTree;

/// Evaluate `tree` against a JSON document body.
#[must_use]
pub fn matches(tree: &FilterTree, doc: &Value) -> bool {
    match tree {
        FilterTree::Compare { field, op, value } => {
            let stored = lookup(doc, field);
            match op {
                ComparisonOp::Eq => scalar_cmp(stored, value) == Some(Ordering::Equal),
                ComparisonOp::Ne => scalar_cmp(stored, value) != Some(Ordering::Equal),
                ComparisonOp::Gt => scalar_cmp(stored, value) == Some(Ordering::Greater),
                ComparisonOp::Gte => {
                    matches!(scalar_cmp(stored, value), Some(Ordering::Greater | Ordering::Equal))
                }
                ComparisonOp::Lt => scalar_cmp(stored, value) == Some(Ordering::Less),
                ComparisonOp::Lte => {
                    matches!(scalar_cmp(stored, value), Some(Ordering::Less | Ordering::Equal))
                }
            }
        }
        FilterTree::InList { field, values } => {
            let stored = lookup(doc, field);
            values.iter().any(|v| scalar_cmp(stored, v) == Some(Ordering::Equal))
        }
        FilterTree::NotInList { field, values } => {
            let stored = lookup(doc, field);
            values.iter().all(|v| scalar_cmp(stored, v) != Some(Ordering::Equal))
        }
        FilterTree::IsNull(field) => lookup(doc, field).is_null(),
        FilterTree::IsNotNull(field) => !lookup(doc, field).is_null(),
        FilterTree::Contains { field, pattern } => {
            lookup(doc, field).as_str().is_some_and(|s| s.contains(pattern))
        }
        FilterTree::StartsWith { field, pattern } => {
            lookup(doc, field).as_str().is_some_and(|s| s.starts_with(pattern))
        }
        FilterTree::EndsWith { field, pattern } => {
            lookup(doc, field).as_str().is_some_and(|s| s.ends_with(pattern))
        }
        FilterTree::And(children) => children.iter().all(|c| matches(c, doc)),
        FilterTree::Or(children) => children.iter().any(|c| matches(c, doc)),
        FilterTree::Not(inner) => !matches(inner, doc),
    }
}

/// Compare two documents under `order_by`, field by field.
///
/// Values of different JSON types order by a fixed type rank
/// (null < bool < number < string < array < object) so the sort is total and
/// deterministic; callers break remaining ties on the document id.
#[must_use]
pub fn compare_documents(a: &Value, b: &Value, order_by: &[SortField]) -> Ordering {
    for sort in order_by {
        let ord = json_cmp(lookup(a, &sort.field), lookup(b, &sort.field));
        let ord = if sort.descending { ord.reverse() } else { ord };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

const NULL: Value = Value::Null;

// Resolve a dotted path inside a JSON document; absent segments read as null.
fn lookup<'a>(doc: &'a Value, path: &str) -> &'a Value {
    let mut current = doc;
    for segment in path.split('.') {
        match current.get(segment) {
            Some(value) => current = value,
            None => return &NULL,
        }
    }
    current
}

// Compare a stored JSON value with a target scalar; `None` = incomparable.
fn scalar_cmp(stored: &Value, target: &ScalarValue) -> Option<Ordering> {
    match (stored, target) {
        (Value::Null, ScalarValue::Null) => Some(Ordering::Equal),
        (Value::Bool(a), ScalarValue::Boolean(b)) => Some(a.cmp(b)),
        (Value::Number(n), _) => {
            let target = match target {
                ScalarValue::Int32(i) => f64::from(*i),
                #[expect(clippy::cast_precision_loss, reason = "dev backend accepts f64 range")]
                ScalarValue::Int64(i) => *i as f64,
                ScalarValue::Float64(f) => *f,
                _ => return None,
            };
            n.as_f64().and_then(|stored| stored.partial_cmp(&target))
        }
        (Value::String(s), ScalarValue::Str(t) | ScalarValue::Timestamp(t)) => {
            Some(s.as_str().cmp(t.as_str()))
        }
        _ => None,
    }
}

// Total order over JSON values: type rank first, then value.
fn json_cmp(a: &Value, b: &Value) -> Ordering {
    const fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(_) => 2,
            Value::String(_) => 3,
            Value::Array(_) => 4,
            Value::Object(_) => 5,
        }
    }

    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .zip(y.as_f64())
            .and_then(|(x, y)| x.partial_cmp(&y))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn compare(field: &str, op: ComparisonOp, value: ScalarValue) -> FilterTree {
        FilterTree::Compare {
            field: field.to_string(),
            op,
            value,
        }
    }

    #[test]
    fn and_eq_with_not_null() {
        let docs = [
            json!({"wb": 1, "zone_id": "z1"}),
            json!({"wb": 1, "zone_id": "z2"}),
            json!({"wb": 0, "zone_id": "z3"}),
            json!({"wb": 1, "zone_id": null}),
        ];
        let filter = FilterTree::And(vec![
            compare("wb", ComparisonOp::Eq, ScalarValue::Int32(1)),
            FilterTree::IsNotNull("zone_id".to_string()),
        ]);
        let matched = docs.iter().filter(|d| matches(&filter, d)).count();
        assert_eq!(matched, 2, "wb=1 AND zone_id IS NOT NULL");
    }

    #[test]
    fn ne_matches_missing_field() {
        let filter = compare("zone_id", ComparisonOp::Ne, ScalarValue::Str("z1".into()));
        assert!(matches(&filter, &json!({})), "missing field is not equal to a string");
        assert!(matches(&filter, &json!({"zone_id": null})), "null is not equal to a string");
        assert!(!matches(&filter, &json!({"zone_id": "z1"})));
    }

    #[test]
    fn ordering_needs_comparable() {
        let filter = compare("lat", ComparisonOp::Gte, ScalarValue::Float64(1.0));
        assert!(matches(&filter, &json!({"lat": 1.5})));
        assert!(!matches(&filter, &json!({"lat": "1.5"})), "string does not order against number");
        assert!(!matches(&filter, &json!({})), "missing field does not order");
    }

    #[test]
    fn starts_with_and_ends_with() {
        let doc = json!({"name": "Northern Line"});
        let starts = FilterTree::StartsWith {
            field: "name".to_string(),
            pattern: "Northern".to_string(),
        };
        let ends = FilterTree::EndsWith {
            field: "name".to_string(),
            pattern: "Line".to_string(),
        };
        assert!(matches(&starts, &doc));
        assert!(matches(&ends, &doc));
        assert!(!matches(
            &FilterTree::StartsWith {
                field: "name".to_string(),
                pattern: "Line".to_string(),
            },
            &doc
        ));
    }

    #[test]
    fn not_in_list() {
        let filter = FilterTree::NotInList {
            field: "route_type".to_string(),
            values: vec![ScalarValue::Int32(1), ScalarValue::Int32(2)],
        };
        assert!(matches(&filter, &json!({"route_type": 3})));
        assert!(!matches(&filter, &json!({"route_type": 2})));
        assert!(matches(&filter, &json!({})), "missing field is in no list");
    }

    #[test]
    fn contains_substring() {
        let filter = FilterTree::Contains {
            field: "name".to_string(),
            pattern: "Station".to_string(),
        };
        assert!(matches(&filter, &json!({"name": "Newmarket Station"})));
        assert!(!matches(&filter, &json!({"name": "Britomart"})));
        assert!(!matches(&filter, &json!({"name": 42})), "non-string never contains");
        assert!(!matches(&filter, &json!({})), "missing field never contains");
    }

    #[test]
    fn in_list_membership() {
        let filter = FilterTree::InList {
            field: "route_type".to_string(),
            values: vec![ScalarValue::Int32(2), ScalarValue::Int32(3)],
        };
        assert!(matches(&filter, &json!({"route_type": 2})));
        assert!(!matches(&filter, &json!({"route_type": 4})));
        assert!(!matches(&filter, &json!({})), "missing field is in no list");
    }

    #[test]
    fn is_null() {
        let filter = FilterTree::IsNull("parent".to_string());
        assert!(matches(&filter, &json!({"parent": null})));
        assert!(matches(&filter, &json!({})), "absent field reads as null");
        assert!(!matches(&filter, &json!({"parent": "stop-1"})));
    }

    #[test]
    fn or_across_fields() {
        let filter = FilterTree::Or(vec![
            FilterTree::Contains {
                field: "short_name".to_string(),
                pattern: "NEX".to_string(),
            },
            FilterTree::Contains {
                field: "long_name".to_string(),
                pattern: "Northern".to_string(),
            },
        ]);
        assert!(matches(&filter, &json!({"short_name": "NEX", "long_name": "x"})));
        assert!(matches(&filter, &json!({"short_name": "x", "long_name": "Northern Express"})));
        assert!(!matches(&filter, &json!({"short_name": "x", "long_name": "y"})));
    }

    #[test]
    fn not_of_and_conjunction() {
        // De Morgan: NOT(agency = AT AND type = 3) admits everything except
        // documents satisfying both conjuncts.
        let filter = FilterTree::Not(Box::new(FilterTree::And(vec![
            compare("agency", ComparisonOp::Eq, ScalarValue::Str("AT".into())),
            compare("route_type", ComparisonOp::Eq, ScalarValue::Int32(3)),
        ])));
        assert!(!matches(&filter, &json!({"agency": "AT", "route_type": 3})));
        assert!(matches(&filter, &json!({"agency": "AT", "route_type": 2})));
        assert!(matches(&filter, &json!({"agency": "Fullers", "route_type": 3})));
        assert!(matches(&filter, &json!({})));
    }

    #[test]
    fn nested_path_lookup() {
        let doc = json!({"stop": {"zone": {"id": "z9"}}});
        let filter = compare("stop.zone.id", ComparisonOp::Eq, ScalarValue::Str("z9".into()));
        assert!(matches(&filter, &doc));
    }

    #[test]
    fn multi_field_desc_sort() {
        let order = vec![
            SortField {
                field: "a".to_string(),
                descending: false,
            },
            SortField {
                field: "b".to_string(),
                descending: true,
            },
        ];
        let low = json!({"a": 1, "b": 1});
        let high_b = json!({"a": 1, "b": 2});
        let high_a = json!({"a": 2, "b": 0});

        assert_eq!(compare_documents(&high_b, &low, &order), Ordering::Less, "b descends");
        assert_eq!(compare_documents(&low, &high_a, &order), Ordering::Less, "a ascends first");
    }

    #[test]
    fn mixed_types_sort_by_rank() {
        let order = vec![SortField {
            field: "v".to_string(),
            descending: false,
        }];
        let null = json!({});
        let number = json!({"v": 5});
        let string = json!({"v": "5"});

        assert_eq!(compare_documents(&null, &number, &order), Ordering::Less);
        assert_eq!(compare_documents(&number, &string, &order), Ordering::Less);
    }
}
