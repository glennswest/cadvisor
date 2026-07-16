//! Helpers backing `skip_serializing_if` for Go `omitempty` semantics.

/// Go's `omitempty` for numeric types: omit when equal to the zero value.
pub(crate) fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}
