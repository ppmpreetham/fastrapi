//! The native `fastrapi.encoders` Python module.
//! It contains `jsonable_encoder`, which converts Python objects into values that JSON can encode.

use pyo3::exceptions::{PyAssertionError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{
    PyBool, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet, PyString, PyTuple, PyType,
};

use fastrapi_pyutil::cached_py_import;
use pyo3::Bound;

cached_py_import!(BASE_MODEL, "pydantic", "BaseModel");
cached_py_import!(ENUM, "enum", "Enum");
cached_py_import!(PURE_PATH, "pathlib", "PurePath");
cached_py_import!(UNDEFINED_TYPE, "pydantic_core", "PydanticUndefinedType");
cached_py_import!(GENERATOR_TYPE, "types", "GeneratorType");
cached_py_import!(DEQUE, "collections", "deque");
cached_py_import!(IS_DATACLASS, "dataclasses", "is_dataclass");
cached_py_import!(AS_DICT, "dataclasses", "asdict");
cached_py_import!(VARS, "builtins", "vars");

static TABLES: PyOnceLock<(Py<PyDict>, Py<PyDict>)> = PyOnceLock::new();

#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors the boolean flags of FastAPI's jsonable_encoder"
)]
#[derive(Clone, Copy)]
struct Options<'a, 'py> {
    by_alias: bool,
    exclude_unset: bool,
    exclude_defaults: bool,
    exclude_none: bool,
    custom_encoder: Option<&'a Bound<'py, PyAny>>,
    sqlalchemy_safe: bool,
}

fn tables(py: Python<'_>) -> PyResult<&(Py<PyDict>, Py<PyDict>)> {
    TABLES.get_or_try_init(py, || {
        let helpers = super::compat::py_helpers(py)?;
        Ok((
            helpers.getattr("ENCODERS_BY_TYPE")?.cast_into::<PyDict>()?.unbind(),
            helpers
                .getattr("encoders_by_class_tuples")?
                .cast_into::<PyDict>()?
                .unbind(),
        ))
    })
}

fn as_key_set<'py>(value: Option<&Bound<'py, PyAny>>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(value) = value.filter(|value| !value.is_none()) else {
        return Ok(None);
    };
    if value.is_instance_of::<PySet>() || value.is_instance_of::<PyDict>() {
        return Ok(Some(value.clone()));
    }
    Ok(Some(value.py().get_type::<PySet>().call1((value,))?))
}

fn custom_encoded<'py>(
    obj: &Bound<'py, PyAny>,
    custom: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let kind = obj.get_type();
    if custom.contains(&kind)? {
        return custom.get_item(&kind)?.call1((obj,)).map(Some);
    }
    for item in custom.call_method0("items")?.try_iter()? {
        let (encoder_type, encoder) = item?.extract::<(Bound<'py, PyAny>, Bound<'py, PyAny>)>()?;
        if obj.is_instance(&encoder_type)? {
            return encoder.call1((obj,)).map(Some);
        }
    }
    Ok(None)
}

fn encode_dict<'py>(
    dict: &Bound<'py, PyDict>,
    include: Option<&Bound<'py, PyAny>>,
    exclude: Option<&Bound<'py, PyAny>>,
    opts: Options<'_, 'py>,
) -> PyResult<Bound<'py, PyAny>> {
    let encoded = PyDict::new(dict.py());
    for (key, value) in dict.iter() {
        let sqlalchemy_key = opts.sqlalchemy_safe
            && key
                .cast::<PyString>()
                .is_ok_and(|key| key.to_str().is_ok_and(|key| key.starts_with("_sa")));
        if sqlalchemy_key
            || (opts.exclude_none && value.is_none())
            || include.is_some_and(|include| !include.contains(&key).unwrap_or(false))
            || exclude.is_some_and(|exclude| exclude.contains(&key).unwrap_or(false))
        {
            continue;
        }
        encoded.set_item(encode(&key, None, None, opts)?, encode(&value, None, None, opts)?)?;
    }
    Ok(encoded.into_any())
}

fn is_sequence(py: Python<'_>, obj: &Bound<'_, PyAny>) -> bool {
    obj.is_instance_of::<PyList>()
        || obj.is_instance_of::<PySet>()
        || obj.is_instance_of::<PyFrozenSet>()
        || obj.is_instance_of::<PyTuple>()
        || GENERATOR_TYPE.is_instance(py, obj)
        || DEQUE.is_instance(py, obj)
}

enum Fallback<'py> {
    Encoded(Bound<'py, PyAny>),
    Data(Bound<'py, PyAny>),
}

fn encode_fallback<'py>(obj: &Bound<'py, PyAny>) -> PyResult<Fallback<'py>> {
    let py = obj.py();
    let (by_type, by_class) = tables(py)?;
    if let Some(encoder) = by_type.bind(py).get_item(obj.get_type())? {
        return encoder.call1((obj,)).map(Fallback::Encoded);
    }
    for (encoder, classes) in by_class.bind(py).iter() {
        if obj.is_instance(&classes)? {
            return encoder.call1((obj,)).map(Fallback::Encoded);
        }
    }
    let data = match py.get_type::<PyDict>().call1((obj,)) {
        Ok(data) => Ok(data),
        Err(dict_error) => VARS.get(py)?.call1((obj,)).map_err(|vars_error| {
            let errors = PyList::new(
                py,
                [dict_error.into_value(py), vars_error.clone_ref(py).into_value(py)],
            );
            let error = match errors {
                Ok(errors) => PyValueError::new_err((errors.unbind(),)),
                Err(error) => error,
            };
            error.set_cause(py, Some(vars_error));
            error
        }),
    }?;
    Ok(Fallback::Data(data))
}

fn encode<'py>(
    obj: &Bound<'py, PyAny>,
    include: Option<&Bound<'py, PyAny>>,
    exclude: Option<&Bound<'py, PyAny>>,
    opts: Options<'_, 'py>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = obj.py();
    if let Some(custom) = opts
        .custom_encoder
        .filter(|custom| custom.is_truthy().unwrap_or(false))
        && let Some(encoded) = custom_encoded(obj, custom)?
    {
        return Ok(encoded);
    }
    if obj.is_none()
        || obj.is_exact_instance_of::<PyString>()
        || obj.is_exact_instance_of::<PyInt>()
        || obj.is_exact_instance_of::<PyFloat>()
        || obj.is_exact_instance_of::<PyBool>()
    {
        return Ok(obj.clone());
    }
    let include = as_key_set(include)?;
    let exclude = as_key_set(exclude)?;
    if BASE_MODEL.is_instance(py, obj) {
        let kwargs = PyDict::new(py);
        kwargs.set_item("mode", "json")?;
        kwargs.set_item("include", include.as_ref())?;
        kwargs.set_item("exclude", exclude.as_ref())?;
        kwargs.set_item("by_alias", opts.by_alias)?;
        kwargs.set_item("exclude_unset", opts.exclude_unset)?;
        kwargs.set_item("exclude_none", opts.exclude_none)?;
        kwargs.set_item("exclude_defaults", opts.exclude_defaults)?;
        let dumped = obj.call_method("model_dump", (), Some(&kwargs))?;
        let nested = Options {
            by_alias: true,
            exclude_unset: false,
            custom_encoder: None,
            ..opts
        };
        return encode(&dumped, None, None, nested);
    }
    if IS_DATACLASS.get(py)?.call1((obj,))?.is_truthy()? {
        if obj.is_instance_of::<PyType>() {
            return Err(PyAssertionError::new_err(()));
        }
        let data = AS_DICT.get(py)?.call1((obj,))?;
        return encode(&data, include.as_ref(), exclude.as_ref(), opts);
    }
    if ENUM.is_instance(py, obj) {
        return obj.getattr("value");
    }
    if PURE_PATH.is_instance(py, obj) {
        return Ok(obj.str()?.into_any());
    }
    if obj.is_instance_of::<PyString>()
        || obj.is_instance_of::<PyInt>()
        || obj.is_instance_of::<PyFloat>()
    {
        return Ok(obj.clone());
    }
    if UNDEFINED_TYPE.is_instance(py, obj) {
        return Ok(py.None().into_bound(py));
    }
    if let Ok(dict) = obj.cast::<PyDict>() {
        return encode_dict(dict, include.as_ref(), exclude.as_ref(), opts);
    }
    if is_sequence(py, obj) {
        let encoded = PyList::empty(py);
        for item in obj.try_iter()? {
            encoded.append(encode(&item?, include.as_ref(), exclude.as_ref(), opts)?)?;
        }
        return Ok(encoded.into_any());
    }
    match encode_fallback(obj)? {
        Fallback::Encoded(value) => Ok(value),
        Fallback::Data(data) => encode(&data, include.as_ref(), exclude.as_ref(), opts),
    }
}

/// Convert any object to something that can be encoded in JSON.
///
/// This is used internally by FastAPI to make sure anything you return can be
/// encoded as JSON before it is sent to the client.
///
/// You can also use it yourself, for example to convert objects before saving them
/// in a database that supports only JSON.
///
/// Read more about it in the
/// [FastAPI docs for JSON Compatible Encoder](https://fastapi.tiangolo.com/tutorial/encoder/).
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "FastAPI's jsonable_encoder accepts multiple boolean filtering flags"
)]
#[pyfunction]
#[pyo3(signature = (obj, *, include=None, exclude=None, by_alias=true, exclude_unset=false, exclude_defaults=false, exclude_none=false, custom_encoder=None, sqlalchemy_safe=true))]
fn jsonable_encoder<'py>(
    obj: &Bound<'py, PyAny>,
    include: Option<&Bound<'py, PyAny>>,
    exclude: Option<&Bound<'py, PyAny>>,
    by_alias: bool,
    exclude_unset: bool,
    exclude_defaults: bool,
    exclude_none: bool,
    custom_encoder: Option<&Bound<'py, PyAny>>,
    sqlalchemy_safe: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let opts = Options {
        by_alias,
        exclude_unset,
        exclude_defaults,
        exclude_none,
        custom_encoder,
        sqlalchemy_safe,
    };
    encode(obj, include, exclude, opts)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(jsonable_encoder, module)?)
}

pub(crate) fn register_tables(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let helpers = super::compat::py_helpers(module.py())?;
    for name in [
        "ENCODERS_BY_TYPE",
        "encoders_by_class_tuples",
        "generate_encoders_by_class_tuples",
        "isoformat",
        "decimal_encoder",
    ] {
        module.add(name, helpers.getattr(name)?)?;
    }
    Ok(())
}
