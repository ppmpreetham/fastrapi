use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::utils::{json_to_py_object, py_any_to_json};
use pyo3::Bound;

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "FastAPI's jsonable_encoder accepts multiple boolean filtering flags"
)]
#[pyfunction]
#[pyo3(signature = (obj, *, include=None, exclude=None, by_alias=true, exclude_unset=false, exclude_defaults=false, exclude_none=false, custom_encoder=None, sqlalchemy_safe=true))]
fn jsonable_encoder(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    include: Option<Py<PyAny>>,
    exclude: Option<Py<PyAny>>,
    by_alias: bool,
    exclude_unset: bool,
    exclude_defaults: bool,
    exclude_none: bool,
    custom_encoder: Option<Py<PyAny>>,
    sqlalchemy_safe: bool,
) -> PyResult<Py<PyAny>> {
    _ = sqlalchemy_safe;
    if let Some(encoder) = custom_encoder
        && let Ok(dict) = encoder.bind(py).cast::<PyDict>()
        && let Ok(Some(callback)) = dict.get_item(obj.get_type())
    {
        return callback.call1((obj,)).map(Bound::unbind);
    }
    let value = if obj.hasattr("model_dump")? {
        let kwargs = PyDict::new(py);
        if let Some(value) = include {
            kwargs.set_item("include", value)?;
        }
        if let Some(value) = exclude {
            kwargs.set_item("exclude", value)?;
        }
        kwargs.set_item("by_alias", by_alias)?;
        kwargs.set_item("exclude_unset", exclude_unset)?;
        kwargs.set_item("exclude_defaults", exclude_defaults)?;
        kwargs.set_item("exclude_none", exclude_none)?;
        obj.call_method("model_dump", (), Some(&kwargs))?
    } else {
        obj.clone()
    };
    Ok(json_to_py_object(py, &py_any_to_json(py, &value)))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(jsonable_encoder, module)?)
}
