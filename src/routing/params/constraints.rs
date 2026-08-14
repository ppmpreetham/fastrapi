use super::super::types::ParameterConstraints;
use pyo3::prelude::*;
use pyo3::types::PyAny;
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
        .and_then(|pattern| regex::Regex::new(&format!("^(?:{pattern})$")).ok())
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
