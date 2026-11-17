//! This module contains `TypeCache`, a bounded cache of a value computed once per Python type.

use papaya::HashMap;
use pyo3::types::PyType;
use pyo3::{Bound, Py};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A value cached per Python type. The cache holds a strong reference to each type, so it
/// stops growing at `limit` to keep classes made at runtime from piling up. Types past the
/// limit are probed each time.
pub struct TypeCache<V> {
    entries: HashMap<usize, (Py<PyType>, V)>,
    len: AtomicUsize,
    limit: usize,
}

impl<V: Copy> TypeCache<V> {
    #[must_use]
    pub fn new(capacity: usize, limit: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            len: AtomicUsize::new(0),
            limit,
        }
    }

    /// Returns the cached value of the type, or the result of `probe` when it is not cached.
    #[inline]
    pub fn get_or_probe(
        &self,
        ty: Bound<'_, PyType>,
        probe: impl FnOnce(&Bound<'_, PyType>) -> V,
    ) -> V {
        let type_ptr = ty.as_ptr() as usize;

        let guard = self.entries.guard();
        if let Some((_, value)) = self.entries.get(&type_ptr, &guard) {
            return *value;
        }
        drop(guard);

        let value = probe(&ty);
        if self.len.load(Ordering::Relaxed) < self.limit
            && self
                .entries
                .pin()
                .try_insert(type_ptr, (ty.unbind(), value))
                .is_ok()
        {
            self.len.fetch_add(1, Ordering::Relaxed);
        }
        value
    }
}
