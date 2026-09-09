//! Guards on the plain component values that cross the link seam.

use wasmtime::component::Val;

/// Recursively reports whether a value carries a live resource handle.
#[must_use]
pub fn contains_resource(value: &Val) -> bool {
    match value {
        Val::Resource(_) => true,
        Val::List(values) | Val::Tuple(values) => values.iter().any(contains_resource),
        Val::Record(fields) => fields.iter().any(|(_, value)| contains_resource(value)),
        Val::Variant(_, Some(value))
        | Val::Option(Some(value))
        | Val::Result(Ok(Some(value)) | Err(Some(value))) => contains_resource(value),
        _ => false,
    }
}
