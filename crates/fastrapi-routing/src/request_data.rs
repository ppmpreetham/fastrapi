//! Extracts and validates the request data for a route.
//! This module converts raw path, query, header, cookie, form, and body values into Python arguments for the endpoint.
//! When a value is not valid, it makes a 422 error response.

use crate::kernel_validation::maybe_validate_with_kernel;
use crate::params::{errors_with_loc, json_error, missing};
use crate::pydantic::parse_json;
use crate::route::{FastKind, ModelKey, ParsedParameter, RouteHandler};
use ahash::AHashMap;
use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use fastrapi_core::body::{BodyField, BodyPayload, FormText};
use fastrapi_core::params::ParameterSource;
use fastrapi_http::background::PyBackgroundTasks;
use fastrapi_http::exceptions::request_validation_error;
use fastrapi_http::request::create_py_request;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::upload_file::PyUploadFile;
use fastrapi_pyutil::call::CallArgs;
use fastrapi_pyutil::json::json_value_to_py;
use percent_encoding::percent_decode_str;
use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyBytes, PyDict, PyFloat, PyList, PyString, PyTuple};
use simd_json::prelude::ValueAsObject;
use smallvec::SmallVec;
use std::borrow::Cow;

type FormFields = AHashMap<FormText, SmallVec<[BodyField; 2]>>;

fastrapi_pyutil::cached_py_import!(VALIDATION_ERROR, "pydantic_core", "ValidationError");

pub struct Errors<'py> {
    py: Python<'py>,
    list: Option<Bound<'py, PyList>>,
}

impl<'py> Errors<'py> {
    #[must_use]
    pub const fn new(py: Python<'py>) -> Self {
        Self { py, list: None }
    }

    fn list(&mut self) -> &Bound<'py, PyList> {
        self.list.get_or_insert_with(|| PyList::empty(self.py))
    }

    fn extend(&mut self, errors: &Bound<'py, PyAny>) -> PyResult<()> {
        let list = self.list().clone();
        errors.try_iter()?.try_for_each(|error| list.append(error?))
    }

    fn missing(&mut self, loc: Loc<'_>) -> PyResult<()> {
        let error = missing(self.py)?.call1((loc.tuple(self.py)?,))?;
        self.list().append(error)
    }

    fn rejected(&mut self, err: PyErr, loc: Loc<'_>) -> PyResult<()> {
        if !VALIDATION_ERROR.is_instance(self.py, err.value(self.py)) {
            return Err(err);
        }
        let errors = errors_with_loc(self.py)?.call1((err.value(self.py), loc.tuple(self.py)?))?;
        self.extend(&errors)
    }

    #[must_use]
    pub fn into_list(self) -> Option<Bound<'py, PyList>> {
        self.list
    }

    pub fn check(self, body: impl FnOnce() -> PyResult<Option<Bound<'py, PyAny>>>) -> PyResult<()> {
        match self.list {
            None => Ok(()),
            Some(list) => Err(request_validation_error(self.py, list.into_any(), body()?)),
        }
    }
}

const fn source_name(source: &ParameterSource) -> &'static str {
    match source {
        ParameterSource::Path => "path",
        ParameterSource::Query => "query",
        ParameterSource::Header => "header",
        ParameterSource::Cookie => "cookie",
        ParameterSource::Body | ParameterSource::BackgroundTasks => "body",
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

fn default_value<'py>(py: Python<'py>, param: &ParsedParameter) -> PyResult<Bound<'py, PyAny>> {
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
            errors.missing(loc)?;
            return Ok(None);
        }
        return default_value(py, param).map(Some);
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
        ParameterSource::Body | ParameterSource::BackgroundTasks => None,
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
) -> PyResult<()> {
    for key in keys {
        match value(key)? {
            Some(found) => dict.set_item(key.alias.bind(py), found)?,
            None if !key.required && !key.default.is_none(py) => {
                dict.set_item(key.alias.bind(py), key.default.bind(py))?;
            }
            None => {}
        }
    }
    Ok(())
}

fn is_model_key(py: Python<'_>, keys: &[ModelKey], name: &str) -> bool {
    keys.iter().any(|key| {
        key.lookup == name || key.alias.bind(py).to_str().is_ok_and(|alias| alias == name)
    })
}

fn model_param<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    keys: &[ModelKey],
    input: &RequestInput<'_>,
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let source = &param.source;
    let dict = PyDict::new(py);
    model_dict(py, keys, &dict, |key| {
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
    resolve(
        py,
        param,
        Some(dict.into_any()),
        Loc::Source(source),
        errors,
    )
}

pub fn request_param_value<'py>(
    py: Python<'py>,
    param: &ParsedParameter,
    input: &RequestInput<'_>,
    errors: &mut Errors<'py>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if let Some(keys) = &param.model_keys {
        return model_param(py, param, keys, input, errors);
    }
    let raw = if param.is_list {
        string_list(
            py,
            source_values(&param.source, &param.external_name, input).iter(),
        )?
    } else {
        let value = param_value(param, input);
        if let (Some(kind), Some(text)) = (param.fast, value.as_deref())
            && let Some(converted) = fast_scalar(py, kind, text)
        {
            return Ok(Some(converted));
        }
        value.map(|text| PyString::new(py, &text).into_any())
    };
    resolve(py, param, raw, Loc::Field(param), errors)
}

fn form_field_to_py<'py>(py: Python<'py>, field: &BodyField) -> PyResult<Bound<'py, PyAny>> {
    match field {
        BodyField::Text(raw) => Ok(PyString::new(py, raw).into_any()),
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
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if !sequence {
        return values
            .last()
            .map(|value| form_field_to_py(py, value))
            .transpose();
    }
    if values.is_empty() {
        return Ok(None);
    }
    let items = values
        .iter()
        .map(|value| form_field_to_py(py, value))
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
    form_values(py, values, param.is_list)
}

fn form_dict<'py>(
    py: Python<'py>,
    fields: Option<&FormFields>,
    skip: impl Fn(&str) -> bool,
    dict: &Bound<'py, PyDict>,
) -> PyResult<()> {
    for (name, values) in fields.into_iter().flatten() {
        if !skip(name) {
            dict.set_item(name.as_str(), form_values(py, values, values.len() > 1)?)?;
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
                Some(values) => form_values(py, values, key.sequence),
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
            if is_json_content_type(headers, handler.payload.strict_content_type) {
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
    if let Some(validator) = param
        .validator_index
        .and_then(|index| handler.validation.param_validators.get(index))
        && let Some(value) = maybe_validate_with_kernel(py, validator, raw)
    {
        return Ok(Some(value.into_bound(py)));
    }
    let loc = Loc::Source(&param.source);
    let Some(validate_json) = &param.validate_json else {
        return resolve(
            py,
            param,
            Some(PyBytes::new(py, raw).into_any()),
            loc,
            errors,
        );
    };
    match validate_json.bind(py).call1((PyBytes::new(py, raw),)) {
        Ok(value) => Ok(Some(value)),
        Err(err) => {
            if !json_invalid(py, raw, errors)? {
                errors.rejected(err, loc)?;
            }
            Ok(None)
        }
    }
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
            errors.missing(Loc::Field(param))?;
        }
        return Ok(());
    }
    let object = object.flatten();
    for param in params {
        let raw = object
            .and_then(|object| object.get(param.external_name.as_str()))
            .and_then(|value| json_value_to_py(py, value).ok());
        if let Some(value) = resolve(py, param, raw, Loc::Field(param), errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
    }
    Ok(())
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
        let raw = match &param.model_keys {
            Some(keys) => Some(form_model(py, keys, fields)?),
            None => form_param_value(py, param, fields)?,
        };
        if let Some(value) = resolve(py, param, raw, Loc::Source(&param.source), errors)? {
            kwargs.set(param.name_py.bind(py), value)?;
        }
        return Ok(());
    }
    for param in params {
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
) -> PyResult<Option<Py<PyBackgroundTasks>>> {
    if handler.payload.has_multiple_query_params {
        request_input.get_all_query_params();
    }

    if let Some(name) = &handler.payload.request_param {
        let request = create_py_request(py, request_input, payload.map(|p| p.raw().clone()))?;
        kwargs.set(name.bind(py), request)?;
    }

    let mut errors = Errors::new(py);
    let mut background: Option<Py<PyBackgroundTasks>> = None;
    for &index in &handler.payload.request_param_indices {
        let param = &handler.payload.parsed_params[index];
        if matches!(param.source, ParameterSource::BackgroundTasks) {
            let tasks = match &background {
                Some(tasks) => tasks.clone_ref(py),
                None => background
                    .insert(Py::new(py, PyBackgroundTasks::new())?)
                    .clone_ref(py),
            };
            kwargs.set(param.name_py.bind(py), tasks)?;
            continue;
        }
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
    errors.check(|| received_body(py, handler, payload, request_input.headers))?;
    Ok(background)
}
