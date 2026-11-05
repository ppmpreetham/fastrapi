//! Reads the validation constraints from a Python parameter object.
//! The constraints are `gt`, `ge`, `lt`, `le`, `min_length`, `max_length`, and `pattern`.
//! The result is a `ParameterConstraints` value that the OpenAPI schema uses.

use fastrapi_core::params::ParameterConstraints;
use pyo3::prelude::*;
use pyo3::types::PyAny;

pub fn extract_constraints(constraints: &Bound<'_, PyAny>) -> ParameterConstraints {
    fn read<T: for<'a, 'py> FromPyObject<'a, 'py>>(
        constraints: &Bound<'_, PyAny>,
        key: &str,
    ) -> Option<T> {
        constraints
            .get_item(key)
            .ok()
            .and_then(|value| value.extract::<T>().ok())
    }

    ParameterConstraints {
        gt: read(constraints, "gt"),
        ge: read(constraints, "ge"),
        lt: read(constraints, "lt"),
        le: read(constraints, "le"),
        min_length: read(constraints, "min_length"),
        max_length: read(constraints, "max_length"),
        pattern: read::<String>(constraints, "pattern").map(Into::into),
    }
}
