//! Extracts and validates the request data for a route.
//! This module converts raw path, query, header, cookie, form, and body values into Python arguments for the endpoint.
//! When a value is not valid, it makes a 422 error response.

use crate::kernel_validation::{KernelOutcome, kernel_error_object, maybe_validate_with_kernel};
use crate::params::{errors_with_loc, json_error};
use crate::pydantic::parse_json;
use crate::route::{
    EmptyDefault, FastKind, ModelKey, ModelPresence, ParamModel, ParsedParameter, RouteHandler,
};
use crate::security::PySecurityScopes;
use ahash::AHashMap;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use fastrapi_core::body::{BodyField, BodyPayload, FormText};
use fastrapi_core::params::ParameterSource;
use fastrapi_http::exceptions::{
    is_default_handler, request_validation_error, request_validation_error_type,
};
use fastrapi_http::request::create_py_request;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::upload_file::PyUploadFile;
use fastrapi_pyutil::call::CallArgs;
use fastrapi_pyutil::json::{json_value_to_py, write_json_string, write_py_json};
use fastrapi_runtime::server_state::current_server;
use percent_encoding::percent_decode_str;
use pydantic_core_kernel::ErrorDetail;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyBytes, PyDict, PyFloat, PyList, PySet, PyString, PyTuple};
use pyo3::{IntoPyObjectExt, intern};
use simd_json::prelude::ValueAsObject;
use simd_json::{OwnedValue as Value, StaticNode};
use smallvec::SmallVec;
use std::borrow::Cow;

type FormFields = AHashMap<FormText, SmallVec<[BodyField; 2]>>;

fastrapi_pyutil::cached_py_import!(VALIDATION_ERROR, "pydantic_core", "ValidationError");

enum ErrorItem<'py> {
    Missing {
        source: &'static str,
        alias: Option<Bound<'py, PyString>>,
    },
    Python(Bound<'py, PyAny>),
    Kernel(ErrorDetail),
}

pub enum Rejection {
    Python(PyErr),
    Response(Response),
}

impl From<PyErr> for Rejection {
    fn from(err: PyErr) -> Self {
        Self::Python(err)
    }
}

pub struct Errors<'py> {
    py: Python<'py>,
    items: SmallVec<[ErrorItem<'py>; 2]>,
}

impl<'py> Errors<'py> {
    #[must_use]
    pub const fn new(py: Python<'py>) -> Self {
        Self {
            py,
            items: SmallVec::new_const(),
        }
    }

    fn extend(&mut self, errors: &Bound<'py, PyAny>) -> PyResult<()> {
        for error in errors.try_iter()? {
            self.items.push(ErrorItem::Python(error?));
        }
        Ok(())
    }

    fn missing(&mut self, loc: Loc<'_>) {
        self.items.push(match loc {
            Loc::Field(param) => ErrorItem::Missing {
                source: source_name(&param.source),
                alias: Some(param.alias_py.bind(self.py).clone()),
            },
            Loc::Source(source) => ErrorItem::Missing {
                source: source_name(source),
                alias: None,
            },
        });
    }

    fn rejected(&mut self, err: PyErr, loc: Loc<'_>) -> PyResult<()> {
        self.rejected_renamed(err, loc, None)
    }

    fn rejected_renamed(
        &mut self,
        err: PyErr,
        loc: Loc<'_>,
        renames: Option<&Py<PyAny>>,
    ) -> PyResult<()> {
        if !VALIDATION_ERROR.is_instance(self.py, err.value(self.py)) {
            return Err(err);
        }
        let errors = errors_with_loc(self.py)?.call1((
            err.value(self.py),
            loc.tuple(self.py)?,
            renames.map(|renames| renames.bind(self.py)),
        ))?;
        self.extend(&errors)
    }

    fn missing_dict(
        &self,
        source: &str,
        alias: Option<&Bound<'py, PyString>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        let loc = match alias {
            Some(alias) => PyTuple::new(py, [PyString::new(py, source), alias.clone()])?,
            None => PyTuple::new(py, [source])?,
        };
        let error = PyDict::new(py);
        error.set_item(intern!(py, "type"), intern!(py, "missing"))?;
        error.set_item(intern!(py, "loc"), loc)?;
        error.set_item(intern!(py, "msg"), intern!(py, "Field required"))?;
        error.set_item(intern!(py, "input"), py.None())?;
        Ok(error.into_any())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn into_list(self) -> PyResult<Option<Bound<'py, PyList>>> {
        if self.items.is_empty() {
            return Ok(None);
        }
        let list = PyList::empty(self.py);
        for item in &self.items {
            match item {
                ErrorItem::Missing { source, alias } => {
                    list.append(self.missing_dict(source, alias.as_ref())?)?;
                }
                ErrorItem::Python(error) => list.append(error)?,
                ErrorItem::Kernel(detail) => list.append(kernel_error_object(self.py, detail)?)?,
            }
        }
        Ok(Some(list))
    }

    pub fn check(self, body: impl FnOnce() -> PyResult<Option<Bound<'py, PyAny>>>) -> PyResult<()> {
        let py = self.py;
        match self.into_list()? {
            None => Ok(()),
            Some(list) => Err(request_validation_error(py, list.into_any(), body()?)),
        }
    }

    fn json(&self) -> PyResult<Vec<u8>> {
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(b"{\"detail\":[");
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                out.push(b',');
            }
            match item {
                ErrorItem::Missing { source, alias } => {
                    out.extend_from_slice(b"{\"type\":\"missing\",\"loc\":[");
                    write_json_string(&mut out, source)?;
                    if let Some(alias) = alias {
                        out.push(b',');
                        write_json_string(&mut out, alias.to_str()?)?;
                    }
                    out.extend_from_slice(b"],\"msg\":\"Field required\",\"input\":null}");
                }
                ErrorItem::Python(error) => write_py_json(self.py, error, &mut out)?,
                ErrorItem::Kernel(detail) => serde_json::to_writer(&mut out, detail)
                    .map_err(|err| pyo3::exceptions::PyValueError::new_err(err.to_string()))?,
            }
        }
        out.extend_from_slice(b"]}");
        Ok(out)
    }

    pub fn reject(
        self,
        input: &RequestInput<'_>,
        body: impl FnOnce() -> PyResult<Option<Bound<'py, PyAny>>>,
    ) -> Rejection {
        if has_validation_handler(self.py, input) {
            return Rejection::Python(self.check(body).err().unwrap_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("validation errors disappeared")
            }));
        }
        match self.json() {
            Ok(body) => Rejection::Response(
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                    body,
                )
                    .into_response(),
            ),
            Err(err) => Rejection::Python(err),
        }
    }
}

fn has_validation_handler(py: Python<'_>, input: &RequestInput<'_>) -> bool {
    let Some(registry) = input
        .server
        .or_else(current_server)
        .and_then(|server| server.server().exception_handlers())
    else {
        return false;
    };
    request_validation_error_type(py)
        .ok()
        .and_then(|class| registry.bind(py).get_item(class).ok().flatten())
        .is_some_and(|handler| !is_default_handler(&handler))
}

const fn source_name(source: &ParameterSource) -> &'static str {
    match source {
        ParameterSource::Path => "path",
        ParameterSource::Query => "query",
        ParameterSource::Header => "header",
        ParameterSource::Cookie => "cookie",
        ParameterSource::Body
        | ParameterSource::BackgroundTasks
        | ParameterSource::SecurityScopes => "body",
    }
}

#[derive(Clone, Copy)]
enum Loc<'a> {
    Field(&'a ParsedParameter),
    Source(&'a ParameterSource),
}

impl Loc<'_> {
    fn tuple(self, py: Python<'_>) -> PyResult<Bound<'_, PyTuple>> {
        match self {
            Self::Field(param) => PyTuple::new(
                py,
                [
                    PyString::new(py, source_name(&param.source)),
                    param.alias_py.bind(py).clone(),
                ],
            ),
            Self::Source(source) => PyTuple::new(py, [source_name(source)]),
        }
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    const TRUE: [&str; 6] = ["1", "true", "t", "on", "yes", "y"];
    const FALSE: [&str; 6] = ["0", "false", "f", "off", "no", "n"];
    if TRUE.iter().any(|value| raw.eq_ignore_ascii_case(value)) {
        Some(true)
    } else {
        FALSE
            .iter()
            .any(|value| raw.eq_ignore_ascii_case(value))
            .then_some(false)
    }
}

fn fast_scalar<'py>(py: Python<'py>, kind: FastKind, raw: &str) -> Option<Bound<'py, PyAny>> {
    match kind {
        FastKind::Str => Some(PyString::new(py, raw).into_any()),
        FastKind::Int => raw.parse::<i64>().ok()?.into_bound_py_any(py).ok(),
        FastKind::Float => raw
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| PyFloat::new(py, value).into_any()),
        FastKind::Bool => parse_bool(raw).map(|value| PyBool::new(py, value).to_owned().into_any()),
    }
}

const EXACT_FLOAT: u64 = 1 << 53;

fn within_bounds(param: &ParsedParameter, value: f64) -> bool {
    let bounds = &param.constraints;
    bounds.gt.is_none_or(|limit| value > limit)
        && bounds.ge.is_none_or(|limit| value >= limit)
        && bounds.lt.is_none_or(|limit| value < limit)
        && bounds.le.is_none_or(|limit| value <= limit)
}

fn within_text(param: &ParsedParameter, value: &str) -> bool {
    let bounds = &param.constraints;
    let length =
        (bounds.min_length.is_some() || bounds.max_length.is_some()).then(|| value.chars().count());
    bounds.min_length.is_none_or(|min| length >= Some(min))
        && bounds.max_length.is_none_or(|max| length <= Some(max))
        && param
            .fast_pattern
            .as_ref()
            .is_none_or(|pattern| pattern.is_match(value))
}

fn fast_int<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    value: i64,
) -> Option<Bound<'py, PyAny>> {
    let bounded = param.constraints.gt.is_some()
        || param.constraints.ge.is_some()
        || param.constraints.lt.is_some()
        || param.constraints.le.is_some();
    #[expect(
        clippy::cast_precision_loss,
        reason = "guarded by the exact float range check"
    )]
    let allowed =
        !bounded || (value.unsigned_abs() < EXACT_FLOAT && within_bounds(param, value as f64));
    allowed.then(|| value.into_bound_py_any(py).ok()).flatten()
}

fn fast_float<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    value: f64,
) -> Option<Bound<'py, PyAny>> {
    (value.is_finite() && within_bounds(param, value)).then(|| PyFloat::new(py, value).into_any())
}

fn fast_text<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    value: &str,
) -> Option<Bound<'py, PyAny>> {
    within_text(param, value).then(|| PyString::new(py, value).into_any())
}

fn fast_value<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    raw: &str,
) -> Option<Bound<'py, PyAny>> {
    let fast = match param.fast {
        Some(FastKind::Int) => raw
            .parse()
            .ok()
            .and_then(|value| fast_int(py, param, value)),
        Some(FastKind::Float) => raw
            .parse()
            .ok()
            .and_then(|value| fast_float(py, param, value)),
        Some(FastKind::Str) => fast_text(py, param, raw),
        Some(FastKind::Bool) => fast_scalar(py, FastKind::Bool, raw),
        None => None,
    };
    fast.or_else(|| param.kernel.as_ref()?.validate_str(py, raw))
}

fn fast_values<'py, S: AsRef<str>>(
    py: Python<'py>,
    param: &ParsedParameter,
    values: &[S],
) -> Option<Bound<'py, PyAny>> {
    if values.is_empty() {
        return None;
    }
    param
        .fast_list
        .and_then(|kind| fast_list(py, kind, values))
        .or_else(|| param.kernel.as_ref()?.validate_strs(py, values))
}

fn fast_json<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    value: &Value,
) -> Option<Bound<'py, PyAny>> {
    fast_json_scalar(py, param, value).or_else(|| param.kernel.as_ref()?.validate_json(py, value))
}

fn fast_json_scalar<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    value: &Value,
) -> Option<Bound<'py, PyAny>> {
    match (param.fast?, value) {
        (FastKind::Str, Value::String(text)) => fast_text(py, param, text),
        (FastKind::Int, Value::Static(StaticNode::I64(number))) => fast_int(py, param, *number),
        (FastKind::Int, Value::Static(StaticNode::U64(number))) => {
            fast_int(py, param, i64::try_from(*number).ok()?)
        }
        (FastKind::Float, Value::Static(StaticNode::F64(number))) => fast_float(py, param, *number),
        (FastKind::Bool, Value::Static(StaticNode::Bool(flag))) => {
            Some(PyBool::new(py, *flag).to_owned().into_any())
        }
        _ => None,
    }
}

fn fast_list<'py, S: AsRef<str>>(
    py: Python<'py>,
    kind: FastKind,
    values: &[S],
) -> Option<Bound<'py, PyAny>> {
    let items = values
        .iter()
        .map(|value| fast_scalar(py, kind, value.as_ref()))
        .collect::<Option<SmallVec<[Bound<'py, PyAny>; 4]>>>()?;
    PyList::new(py, items).ok().map(Bound::into_any)
}

fn default_value<'py>(py: Python<'py>, param: &ParsedParameter) -> PyResult<Bound<'py, PyAny>> {
    match param.empty_default {
        Some(EmptyDefault::List) => return Ok(PyList::empty(py).into_any()),
        Some(EmptyDefault::Dict) => return Ok(PyDict::new(py).into_any()),
        Some(EmptyDefault::Set) => return Ok(PySet::empty(py)?.into_any()),
        None => {}
    }
    match (&param.make_default, &param.default_value) {
        (Some(make), _) => make.bind(py).call0(),
        (None, Some(value)) => Ok(value.bind(py).clone()),
        (None, None) => Ok(py.None().into_bound(py)),
    }
}

fn resolve<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    raw: Option<Bound<'py, PyAny>>,
    loc: Loc<'_>,
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(raw) = raw else {
        if param.required {
            errors.missing(loc);
            return Ok(None);
        }
        return match default_value(py, param) {
            Ok(value) => Ok(Some(value)),
            Err(err) => errors.rejected(err, loc).map(|()| None),
        };
    };
    match param.validate.bind(py).call1((raw,)) {
        Ok(value) => Ok(Some(value)),
        Err(err) => errors.rejected(err, loc).map(|()| None),
    }
}

fn unquote_cookie(value: &str) -> Cow<'_, str> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Cow::Borrowed(value);
    };
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }
    let bytes = inner.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let Some(&next) = bytes.get(index + 1).filter(|_| byte == b'\\') else {
            decoded.push(byte);
            index += 1;
            continue;
        };
        let octal = bytes
            .get(index + 1..index + 4)
            .filter(|digits| digits.iter().all(|digit| (b'0'..=b'7').contains(digit)));
        if let Some(digits) = octal {
            decoded.push(digits.iter().fold(0u8, |acc, digit| {
                acc.wrapping_mul(8).wrapping_add(digit - b'0')
            }));
            index += 4;
        } else {
            decoded.push(next);
            index += 2;
        }
    }
    Cow::Owned(String::from_utf8_lossy(&decoded).into_owned())
}

fn source_value<'a>(
    source: &ParameterSource,
    key: &str,
    input: &'a RequestInput<'_>,
) -> Option<Cow<'a, str>> {
    match source {
        ParameterSource::Path => input
            .get_path_param(key)
            .map(|value| percent_decode_str(value).decode_utf8_lossy()),
        ParameterSource::Query => input.get_query_param(key).map(Cow::Borrowed),
        ParameterSource::Header => input.get_header(key).map(Cow::Borrowed),
        ParameterSource::Cookie => input.get_cookie(key).map(unquote_cookie),
        ParameterSource::Body
        | ParameterSource::BackgroundTasks
        | ParameterSource::SecurityScopes => None,
    }
}

fn source_values<'a>(
    source: &ParameterSource,
    key: &str,
    input: &'a RequestInput<'_>,
) -> SmallVec<[Cow<'a, str>; 4]> {
    match source {
        ParameterSource::Query => input
            .get_all_query_params()
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| Cow::Borrowed(&**value))
            .collect(),
        ParameterSource::Header => input.get_header_values(key),
        _ => source_value(source, key, input).into_iter().collect(),
    }
}

fn param_value<'a>(param: &ParsedParameter, input: &'a RequestInput<'_>) -> Option<Cow<'a, str>> {
    match &param.header_name {
        Some(name) => input.get_header_by_name(name).map(Cow::Borrowed),
        None => source_value(&param.source, &param.external_name, input),
    }
}

fn string_list<S: AsRef<str>>(
    py: Python<'_>,
    values: impl ExactSizeIterator<Item = S>,
) -> PyResult<Option<Bound<'_, PyAny>>> {
    if values.len() == 0 {
        return Ok(None);
    }
    PyList::new(py, values.map(|value| PyString::new(py, value.as_ref())))
        .map(|list| Some(list.into_any()))
}

fn add_received(dict: &Bound<'_, PyDict>, key: &str, value: &str, multi: bool) -> PyResult<()> {
    let py = dict.py();
    let value = PyString::new(py, value).into_any();
    match dict.get_item(key)? {
        Some(existing) if multi => match existing.cast::<PyList>() {
            Ok(list) => list.append(value),
            Err(_) => dict.set_item(key, PyList::new(py, [existing, value])?),
        },
        _ => dict.set_item(key, value),
    }
}

fn model_dict<'py>(
    py: Python<'py>,
    keys: &[ModelKey],
    dict: &Bound<'py, PyDict>,
    value: impl Fn(&ModelKey) -> PyResult<Option<Bound<'py, PyAny>>>,
) -> PyResult<bool> {
    let mut found_any = false;
    for key in keys {
        if let Some(found) = value(key)? {
            dict.set_item(key.alias.bind(py), found)?;
            found_any = true;
        }
    }
    Ok(found_any)
}

fn is_model_key(py: Python<'_>, keys: &[ModelKey], name: &str) -> bool {
    keys.iter().any(|key| {
        key.lookup == name || key.alias.bind(py).to_str().is_ok_and(|alias| alias == name)
    })
}

fn model_param<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    model: &ParamModel,
    input: &RequestInput<'_>,
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let source = &param.source;
    let keys = &model.keys;
    let dict = PyDict::new(py);
    let found_own = model_dict(py, keys, &dict, |key| {
        if key.sequence {
            string_list(py, source_values(source, &key.lookup, input).iter())
        } else {
            Ok(source_value(source, &key.lookup, input)
                .map(|value| PyString::new(py, &value).into_any()))
        }
    })?;
    match source {
        ParameterSource::Query => {
            for (name, value) in input.get_all_query_params() {
                if !is_model_key(py, keys, name) {
                    add_received(&dict, name, value, true)?;
                }
            }
        }
        ParameterSource::Header => {
            for (name, value) in input.headers {
                if let Ok(value) = value.to_str()
                    && !is_model_key(py, keys, name.as_str())
                {
                    add_received(&dict, name.as_str(), value, true)?;
                }
            }
        }
        ParameterSource::Cookie => {
            for (name, value) in input.get_all_cookies() {
                if !is_model_key(py, keys, name) {
                    add_received(&dict, name, &unquote_cookie(value), false)?;
                }
            }
        }
        _ => {}
    }
    let present = match model.presence {
        None => true,
        Some(ModelPresence::OwnKeys) => found_own,
        Some(ModelPresence::AnyKey) => !dict.is_empty(),
    };
    if !present {
        return resolve(py, param, None, Loc::Source(source), errors);
    }
    match param.validate.bind(py).call1((dict,)) {
        Ok(value) => Ok(Some(value)),
        Err(err) => errors
            .rejected_renamed(err, Loc::Source(source), param.loc_renames.as_ref())
            .map(|()| None),
    }
}

pub fn request_param_value<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    input: &RequestInput<'_>,
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    match param.source {
        ParameterSource::BackgroundTasks => {
            return Ok(Some(input.background_tasks(py)?.into_bound(py).into_any()));
        }
        ParameterSource::SecurityScopes => {
            return Ok(Some(
                Bound::new(py, PySecurityScopes::new(None))?.into_any(),
            ));
        }
        _ => {}
    }
    if let Some(model) = &param.model {
        return model_param(py, param, model, input, errors);
    }
    let raw = if param.is_list {
        let values = source_values(&param.source, &param.external_name, input);
        if let Some(converted) = fast_values(py, param, &values) {
            return Ok(Some(converted));
        }
        string_list(py, values.iter())?
    } else {
        let value = param_value(param, input);
        if let Some(converted) = value
            .as_deref()
            .and_then(|text| fast_value(py, param, text))
        {
            return Ok(Some(converted));
        }
        value.map(|text| PyString::new(py, &text).into_any())
    };
    resolve(py, param, raw, Loc::Field(param), errors)
}

fn form_field_to_py<'py>(
    py: Python<'py>,
    field: &BodyField,
    read_bytes: bool,
) -> PyResult<Bound<'py, PyAny>> {
    match field {
        BodyField::Text(raw) => Ok(PyString::new(py, raw).into_any()),
        BodyField::File(file) if read_bytes => Ok(PyBytes::new(py, &file.content).into_any()),
        BodyField::File(file) => Bound::new(
            py,
            PyUploadFile::from_bytes(
                py,
                file.filename.clone(),
                file.content_type.clone(),
                file.headers.clone(),
                file.content.clone(),
            ),
        )
        .map(Bound::into_any),
    }
}

fn form_values<'py>(
    py: Python<'py>,
    values: &[BodyField],
    sequence: bool,
    read_bytes: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if !sequence {
        return values
            .last()
            .map(|value| form_field_to_py(py, value, read_bytes))
            .transpose();
    }
    if values.is_empty() {
        return Ok(None);
    }
    let items = values
        .iter()
        .map(|value| form_field_to_py(py, value, read_bytes))
        .collect::<PyResult<SmallVec<[Bound<'py, PyAny>; 4]>>>()?;
    PyList::new(py, items).map(|list| Some(list.into_any()))
}

fn form_param_value<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    fields: Option<&FormFields>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(values) = fields.and_then(|fields| fields.get(param.external_name.as_str())) else {
        return Ok(None);
    };
    if !param.is_list
        && param.is_form
        && matches!(values.last(), Some(BodyField::Text(text)) if text.is_empty())
    {
        return Ok(None);
    }
    form_values(py, values, param.is_list, param.read_bytes)
}

fn form_dict<'py>(
    py: Python<'py>,
    fields: Option<&FormFields>,
    skip: impl Fn(&str) -> bool,
    dict: &Bound<'py, PyDict>,
) -> PyResult<()> {
    for (name, values) in fields.into_iter().flatten() {
        if !skip(name) {
            dict.set_item(
                name.as_str(),
                form_values(py, values, values.len() > 1, false)?,
            )?;
        }
    }
    Ok(())
}

fn form_model<'py>(
    py: Python<'py>,
    keys: &[ModelKey],
    fields: Option<&FormFields>,
) -> PyResult<Bound<'py, PyAny>> {
    let dict = PyDict::new(py);
    if let Some(fields) = fields {
        model_dict(py, keys, &dict, |key| {
            match fields.get(key.lookup.as_str()) {
                Some(values) => form_values(py, values, key.sequence, false),
                None => Ok(None),
            }
        })?;
    }
    form_dict(py, fields, |name| is_model_key(py, keys, name), &dict)?;
    Ok(dict.into_any())
}

fn is_json_content_type(headers: &HeaderMap, strict: bool) -> bool {
    let Some(value) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    else {
        return !strict;
    };
    let mime = value.split(';').next().unwrap_or_default().trim();
    let Some((main, sub)) = mime.split_once('/') else {
        return false;
    };
    let sub = sub.trim().as_bytes();
    main.trim().eq_ignore_ascii_case("application")
        && (sub.eq_ignore_ascii_case(b"json")
            || sub
                .len()
                .checked_sub(5)
                .is_some_and(|start| sub[start..].eq_ignore_ascii_case(b"+json")))
}

#[derive(Clone, Copy)]
enum JsonBody<'a> {
    Absent,
    Bytes(&'a [u8]),
    Json(&'a [u8]),
}

fn json_body<'a>(
    handler: &RouteHandler,
    payload: Option<&'a BodyPayload>,
    headers: &HeaderMap,
) -> JsonBody<'a> {
    match payload {
        Some(BodyPayload::Json { raw }) if !raw.is_empty() => {
            if is_json_content_type(headers, handler.payload.strict_content_type.unwrap_or(true)) {
                JsonBody::Json(raw)
            } else {
                JsonBody::Bytes(raw)
            }
        }
        _ => JsonBody::Absent,
    }
}

fn json_document<'py>(
    py: Python<'py>,
    raw: &[u8],
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    json_error(py)?.call1((PyBytes::new(py, raw),))?.extract()
}

fn json_invalid<'py>(py: Python<'py>, raw: &[u8], errors: &mut Errors<'py>) -> PyResult<bool> {
    let (found, _) = json_document(py, raw)?;
    if found.is_truthy()? {
        errors.extend(&found)?;
        return Ok(true);
    }
    Ok(false)
}

fn single_json_body<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    param: &ParsedParameter,
    raw: &[u8],
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let kernel = param
        .validator_index
        .and_then(|index| handler.validation.param_validators.get(index))
        .and_then(|validator| maybe_validate_with_kernel(py, validator, raw));
    match kernel {
        Some(KernelOutcome::Valid(value)) => return Ok(Some(value.into_bound(py))),
        Some(KernelOutcome::Invalid(details))
            if details.iter().all(|detail| detail.loc.len() > 1) =>
        {
            errors
                .items
                .extend(details.into_iter().map(ErrorItem::Kernel));
            return Ok(None);
        }
        _ => {}
    }
    let loc = Loc::Source(&param.source);
    if let Some(kernel) = &param.kernel
        && let Ok(parsed) = pydantic_core_kernel::json::parse_json(raw)
        && let Some(value) = kernel.validate(py, &parsed)
    {
        return Ok(Some(value));
    }
    let Some(validate_json) = &param.validate_json else {
        return resolve(
            py,
            param,
            Some(PyBytes::new(py, raw).into_any()),
            loc,
            errors,
        );
    };
    let err = match validate_json.bind(py).call1((PyBytes::new(py, raw),)) {
        Ok(value) => return Ok(Some(value)),
        Err(err) => err,
    };
    if let Some(parsed) = parse_json(py, raw) {
        let value = json_value_to_py(py, &parsed)?;
        return resolve(py, param, Some(value), loc, errors);
    }
    if !json_invalid(py, raw, errors)? {
        errors.rejected(err, loc)?;
    }
    Ok(None)
}

fn apply_json_body<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    params: &[&ParsedParameter],
    body: JsonBody<'_>,
    kwargs: &mut CallArgs,
    errors: &mut Errors<'py>,
) -> PyResult<()> {
    if !handler.payload.body_embed {
        let param = params[0];
        let value = match body {
            JsonBody::Json(raw) => single_json_body(py, handler, param, raw, errors)?,
            JsonBody::Bytes(raw) => resolve(
                py,
                param,
                Some(PyBytes::new(py, raw).into_any()),
                Loc::Source(&param.source),
                errors,
            )?,
            JsonBody::Absent => resolve(py, param, None, Loc::Source(&param.source), errors)?,
        };
        if let Some(value) = value {
            kwargs.set(param.name_py.bind(py), value)?;
        }
        return Ok(());
    }
    let parsed = match body {
        JsonBody::Json(raw) => {
            let Some(parsed) = parse_json(py, raw) else {
                json_invalid(py, raw, errors)?;
                return Ok(());
            };
            Some(parsed)
        }
        JsonBody::Bytes(_) | JsonBody::Absent => None,
    };
    let object = parsed.as_ref().map(ValueAsObject::as_object);
    if let Some(None) = object {
        for param in params {
            errors.missing(Loc::Field(param));
        }
        return Ok(());
    }
    let object = object.flatten();
    for param in params {
        let found = object.and_then(|object| object.get(param.external_name.as_str()));
        if let Some(converted) = found.and_then(|value| fast_json(py, param, value)) {
            kwargs.set(param.name_py.bind(py), converted)?;
            continue;
        }
        let raw = found.and_then(|value| json_value_to_py(py, value).ok());
        if let Some(value) = resolve(py, param, raw, Loc::Field(param), errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
    }
    Ok(())
}

fn form_fast<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    fields: Option<&FormFields>,
) -> Option<Bound<'py, PyAny>> {
    let values = fields?.get(param.external_name.as_str())?;
    if param.is_list {
        let texts = values
            .iter()
            .map(|value| match value {
                BodyField::Text(text) => Some(text.as_str()),
                BodyField::File(_) => None,
            })
            .collect::<Option<SmallVec<[&str; 4]>>>()?;
        return fast_values(py, param, &texts);
    }
    match values.last()? {
        BodyField::Text(text) if !text.is_empty() => fast_value(py, param, text),
        _ => None,
    }
}

fn apply_form_body<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    params: &[&ParsedParameter],
    fields: Option<&FormFields>,
    kwargs: &mut CallArgs,
    errors: &mut Errors<'py>,
) -> PyResult<()> {
    if !handler.payload.body_embed {
        let param = params[0];
        let raw = match &param.model {
            Some(model) => Some(form_model(py, &model.keys, fields)?),
            None => form_param_value(py, param, fields)?,
        };
        if let Some(value) = resolve(py, param, raw, Loc::Source(&param.source), errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
        return Ok(());
    }
    for param in params {
        if let Some(value) = form_fast(py, param, fields) {
            kwargs.set(param.name_py.bind(py), value)?;
            continue;
        }
        let raw = form_param_value(py, param, fields)?;
        if let Some(value) = resolve(py, param, raw, Loc::Field(param), errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
    }
    Ok(())
}

const fn form_fields(payload: Option<&BodyPayload>) -> Option<&FormFields> {
    match payload {
        Some(BodyPayload::Form { fields, .. }) => Some(fields),
        _ => None,
    }
}

fn received_body<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    payload: Option<&BodyPayload>,
    headers: &HeaderMap,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if handler.payload.body_is_form {
        let dict = PyDict::new(py);
        form_dict(py, form_fields(payload), |_| false, &dict)?;
        return Ok(Some(dict.into_any()));
    }
    Ok(match json_body(handler, payload, headers) {
        JsonBody::Absent => None,
        JsonBody::Bytes(raw) => Some(PyBytes::new(py, raw).into_any()),
        JsonBody::Json(raw) => Some(json_document(py, raw)?.1),
    })
}

fn apply_body<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    payload: Option<&BodyPayload>,
    headers: &HeaderMap,
    kwargs: &mut CallArgs,
    errors: &mut Errors<'py>,
) -> PyResult<()> {
    let params: SmallVec<[&ParsedParameter; 4]> = handler
        .payload
        .body_param_indices
        .iter()
        .map(|&index| &handler.payload.parsed_params[index])
        .collect();
    if params.is_empty() {
        return Ok(());
    }
    if handler.payload.body_is_form {
        return apply_form_body(py, handler, &params, form_fields(payload), kwargs, errors);
    }
    let body = json_body(handler, payload, headers);
    apply_json_body(py, handler, &params, body, kwargs, errors)
}

pub fn apply_request_data(
    py: Python,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
    kwargs: &mut CallArgs,
) -> Result<(), Rejection> {
    if handler.payload.has_multiple_query_params {
        request_input.get_all_query_params();
    }

    if let Some(name) = &handler.payload.request_param {
        let request = create_py_request(py, request_input, payload.map(|p| p.raw().clone()))?;
        kwargs.set(name.bind(py), request)?;
    }

    let mut errors = Errors::new(py);
    for &index in &handler.payload.request_param_indices {
        let param = &handler.payload.parsed_params[index];
        if let Some(value) = request_param_value(py, param, request_input, &mut errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
    }

    apply_body(
        py,
        handler,
        payload,
        request_input.headers,
        kwargs,
        &mut errors,
    )?;
    if errors.is_empty() {
        return Ok(());
    }
    Err(errors.reject(request_input, || {
        received_body(py, handler, payload, request_input.headers)
    }))
}
