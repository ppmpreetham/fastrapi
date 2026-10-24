//! Reads the validation constraints from a Python parameter object.
//! The constraints are `gt`, `ge`, `lt`, `le`, `min_length`, `max_length`, and `pattern`.
//! The result is a `ParameterConstraints` value that the request validation uses.

use fastrapi_core::params::ParameterConstraints;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use regex::Regex;
use std::sync::Arc;

pub fn extract_constraints(param_obj: &Bound<'_, PyAny>) -> ParameterConstraints {
    fn extract_opt<T: for<'a, 'py> FromPyObject<'a, 'py>>(
        obj: &Bound<'_, PyAny>,
        attr: &str,
    ) -> Option<T> {
        obj.getattr(attr)
            .ok()
            .and_then(|value| value.extract::<T>().ok())
    }

    let pattern = param_obj
        .getattr("pattern")
        .ok()
        .and_then(|value| value.extract::<Option<String>>().ok())
        .flatten()
        .and_then(|pattern| Regex::new(&format!("^(?:{pattern})$")).ok())
        .map(Arc::new);

    ParameterConstraints {
        gt: extract_opt(param_obj, "gt"),
        ge: extract_opt(param_obj, "ge"),
        lt: extract_opt(param_obj, "lt"),
        le: extract_opt(param_obj, "le"),
        min_length: extract_opt(param_obj, "min_length"),
        max_length: extract_opt(param_obj, "max_length"),
        pattern,
    }
}
