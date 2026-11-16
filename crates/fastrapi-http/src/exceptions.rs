//! This module contains the Python exception classes of fastrapi.
//! The classes are Rust pyclasses, and `HTTPException` subclasses the starlette class with a Rust `__init__`.

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use fastrapi_pyutil::json::py_json_response_with_status;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::server_state::current_server;
use pyo3::PyClassInitializer;
use pyo3::exceptions::{PyException, PyRuntimeError, PyTypeError, PyUserWarning};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyCFunction, PyDict, PyModule, PyString, PyTuple, PyType};
use std::sync::Arc;

const HANDLERS_SOURCE: &str = include_str!("py/exception_handlers.py");

static EXCEPTIONS: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
static HANDLERS: PyOnceLock<Py<PyModule>> = PyOnceLock::new();
static HTTP_EXCEPTION: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static WEBSOCKET_EXCEPTION: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static REQUEST_VALIDATION_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static WEBSOCKET_VALIDATION_ERROR: PyOnceLock<Py<PyType>> = PyOnceLock::new();
static DEPRECATION_WARNING: PyOnceLock<Py<PyType>> = PyOnceLock::new();

fastrapi_pyutil::cached_py_import!(
    STARLETTE_HTTP_EXCEPTION,
    "starlette.exceptions",
    "HTTPException"
);
fastrapi_pyutil::cached_py_import!(
    STARLETTE_WEBSOCKET_EXCEPTION,
    "starlette.exceptions",
    "WebSocketException"
);
fastrapi_pyutil::cached_py_import!(METHOD_TYPE, "types", "MethodType");
fastrapi_pyutil::cached_py_import!(STATUS_PHRASES, "http.client", "responses");

#[pyclass(
    extends = PyRuntimeError,
    subclass,
    name = "FastAPIError",
    module = "fastrapi.exceptions"
)]
pub struct PyFastAPIError;

#[pymethods]
impl PyFastAPIError {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    const fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self
    }
}

#[pyclass(
    extends = PyFastAPIError,
    subclass,
    name = "DependencyScopeError",
    module = "fastrapi.exceptions"
)]
pub struct PyDependencyScopeError;

#[pymethods]
impl PyDependencyScopeError {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, PyTuple>,
        _kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
    }
}

#[pyclass(
    extends = PyFastAPIError,
    subclass,
    name = "PydanticV1NotSupportedError",
    module = "fastrapi.exceptions"
)]
pub struct PyPydanticV1NotSupportedError;

#[pymethods]
impl PyPydanticV1NotSupportedError {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, PyTuple>,
        _kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyFastAPIError).add_subclass(Self)
    }
}

#[pyclass(
    extends = PyUserWarning,
    subclass,
    name = "FastAPIDeprecationWarning",
    module = "fastrapi.exceptions"
)]
pub struct PyFastAPIDeprecationWarning;

#[pymethods]
impl PyFastAPIDeprecationWarning {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    const fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self
    }
}

#[pyclass(
    extends = PyException,
    subclass,
    name = "ValidationException",
    module = "fastrapi.exceptions"
)]
pub struct PyValidationException {
    #[pyo3(get, name = "_errors")]
    pub errors: Py<PyAny>,
    #[pyo3(get)]
    pub endpoint_ctx: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub endpoint_function: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub endpoint_path: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub endpoint_file: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub endpoint_line: Option<Py<PyAny>>,
}

impl PyValidationException {
    fn create(errors: Bound<'_, PyAny>, endpoint_ctx: Option<Bound<'_, PyAny>>) -> Self {
        let py = errors.py();
        let endpoint_ctx = endpoint_ctx.filter(|ctx| !ctx.is_none());
        let field = |key: &str| {
            endpoint_ctx
                .as_ref()
                .and_then(|ctx| ctx.call_method1(intern!(py, "get"), (key,)).ok())
                .filter(|value| !value.is_none())
                .map(Bound::unbind)
        };
        Self {
            endpoint_function: field("function"),
            endpoint_path: field("path"),
            endpoint_file: field("file"),
            endpoint_line: field("line"),
            endpoint_ctx: endpoint_ctx.map(Bound::unbind),
            errors: errors.unbind(),
        }
    }

    fn endpoint_context(&self, py: Python<'_>) -> PyResult<String> {
        let text = |value: &Option<Py<PyAny>>| -> PyResult<Option<String>> {
            value
                .as_ref()
                .map(|value| Ok(value.bind(py).str()?.to_string_lossy().into_owned()))
                .transpose()
        };
        let path = text(&self.endpoint_path)?;
        let (Some(file), Some(line), Some(function)) = (
            text(&self.endpoint_file)?,
            text(&self.endpoint_line)?,
            text(&self.endpoint_function)?,
        ) else {
            return Ok(path.map_or_else(String::new, |path| format!("\n  Endpoint: {path}")));
        };
        let mut context = format!("\n  File \"{file}\", line {line}, in {function}");
        if let Some(path) = path {
            context.push_str("\n    ");
            context.push_str(&path);
        }
        Ok(context)
    }
}

#[pymethods]
impl PyValidationException {
    #[new]
    #[pyo3(signature = (errors, *, endpoint_ctx=None))]
    fn new(errors: Bound<'_, PyAny>, endpoint_ctx: Option<Bound<'_, PyAny>>) -> Self {
        Self::create(errors, endpoint_ctx)
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    const fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}

    fn errors(&self, py: Python<'_>) -> Py<PyAny> {
        self.errors.clone_ref(py)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        let errors = self.errors.bind(py);
        let count = errors.len()?;
        let mut message = format!(
            "{count} validation error{}:\n",
            if count == 1 { "" } else { "s" }
        );
        for error in errors.try_iter()? {
            message.push_str("  ");
            message.push_str(&error?.str()?.to_string_lossy());
            message.push('\n');
        }
        message.push_str(&self.endpoint_context(py)?);
        Ok(message.trim_end().to_owned())
    }
}

#[pyclass(
    extends = PyValidationException,
    subclass,
    name = "RequestValidationError",
    module = "fastrapi.exceptions"
)]
pub struct PyRequestValidationError {
    #[pyo3(get, set)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyRequestValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None, endpoint_ctx=None))]
    fn new(
        errors: Bound<'_, PyAny>,
        body: Option<Bound<'_, PyAny>>,
        endpoint_ctx: Option<Bound<'_, PyAny>>,
    ) -> PyClassInitializer<Self> {
        let body = body.map_or_else(|| errors.py().None(), Bound::unbind);
        PyClassInitializer::from(PyValidationException::create(errors, endpoint_ctx))
            .add_subclass(Self { body })
    }
}

#[pyclass(
    extends = PyValidationException,
    subclass,
    name = "WebSocketRequestValidationError",
    module = "fastrapi.exceptions"
)]
pub struct PyWebSocketRequestValidationError;

#[pymethods]
impl PyWebSocketRequestValidationError {
    #[new]
    #[pyo3(signature = (errors, *, endpoint_ctx=None))]
    fn new(
        errors: Bound<'_, PyAny>,
        endpoint_ctx: Option<Bound<'_, PyAny>>,
    ) -> PyClassInitializer<Self> {
        PyClassInitializer::from(PyValidationException::create(errors, endpoint_ctx))
            .add_subclass(Self)
    }
}

#[pyclass(
    extends = PyValidationException,
    subclass,
    name = "ResponseValidationError",
    module = "fastrapi.exceptions"
)]
pub struct PyResponseValidationError {
    #[pyo3(get, set)]
    pub body: Py<PyAny>,
}

#[pymethods]
impl PyResponseValidationError {
    #[new]
    #[pyo3(signature = (errors, *, body=None, endpoint_ctx=None))]
    fn new(
        errors: Bound<'_, PyAny>,
        body: Option<Bound<'_, PyAny>>,
        endpoint_ctx: Option<Bound<'_, PyAny>>,
    ) -> PyClassInitializer<Self> {
        let body = body.map_or_else(|| errors.py().None(), Bound::unbind);
        PyClassInitializer::from(PyValidationException::create(errors, endpoint_ctx))
            .add_subclass(Self { body })
    }
}

#[pyclass(frozen, name = "rust_method", module = "fastrapi.exceptions")]
struct RustMethod {
    function: Py<PyAny>,
}

#[pymethods]
impl RustMethod {
    fn __get__<'py>(
        slf: &Bound<'py, Self>,
        instance: Option<&Bound<'py, PyAny>>,
        _owner: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        match instance.filter(|instance| !instance.is_none()) {
            Some(instance) => METHOD_TYPE
                .get(py)?
                .call1((slf.get().function.bind(py), instance)),
            None => Ok(slf.clone().into_any()),
        }
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.function.bind(args.py()).call(args, kwargs)
    }
}

fn status_phrase<'py>(status: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = status.py();
    Ok(STATUS_PHRASES
        .get(py)?
        .get_item(status)
        .unwrap_or_else(|_| PyString::new(py, "").into_any()))
}

fn http_exception_init(
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let argument = |index: usize, name: &str| {
        args.get_item(index)
            .ok()
            .or_else(|| kwargs.and_then(|kwargs| kwargs.get_item(name).ok().flatten()))
    };
    let py = args.py();
    let this = args.get_item(0)?;
    let status_code = argument(1, "status_code").ok_or_else(|| {
        PyTypeError::new_err(
            "HTTPException.__init__() missing 1 required positional argument: 'status_code'",
        )
    })?;
    let detail = match argument(2, "detail").filter(|detail| !detail.is_none()) {
        Some(detail) => detail,
        None => status_phrase(&status_code)?,
    };
    let headers = argument(3, "headers").unwrap_or_else(|| py.None().into_bound(py));
    this.setattr(intern!(py, "status_code"), status_code)?;
    this.setattr(intern!(py, "detail"), detail)?;
    this.setattr(intern!(py, "headers"), headers)?;
    Ok(py.None())
}

fn build_http_exception(py: Python<'_>) -> PyResult<Py<PyType>> {
    let init = PyCFunction::new_closure(py, Some(c"__init__"), None, http_exception_init)?;
    let namespace = PyDict::new(py);
    namespace.set_item(
        "__init__",
        Bound::new(
            py,
            RustMethod {
                function: init.into_any().unbind(),
            },
        )?,
    )?;
    starlette_subclass(
        "HTTPException",
        STARLETTE_HTTP_EXCEPTION.get(py)?,
        &namespace,
    )
}

fn starlette_subclass(
    name: &str,
    base: &Bound<'_, PyAny>,
    namespace: &Bound<'_, PyDict>,
) -> PyResult<Py<PyType>> {
    let py = base.py();
    namespace.set_item("__module__", "fastrapi.exceptions")?;
    namespace.set_item("__qualname__", name)?;
    py.get_type::<PyType>()
        .call1((name, (base,), namespace))?
        .cast_into::<PyType>()
        .map(Bound::unbind)
        .map_err(PyErr::from)
}

pub fn websocket_exception_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    WEBSOCKET_EXCEPTION
        .get_or_try_init(py, || {
            starlette_subclass(
                "WebSocketException",
                STARLETTE_WEBSOCKET_EXCEPTION.get(py)?,
                &PyDict::new(py),
            )
        })
        .map(|class| class.bind(py))
}

fn endpoint_context_type(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let builtins = py.import("builtins")?;
    let fields = PyDict::new(py);
    for (name, kind) in [
        ("function", "str"),
        ("path", "str"),
        ("file", "str"),
        ("line", "int"),
    ] {
        fields.set_item(name, builtins.getattr(kind)?)?;
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("total", false)?;
    py.import("typing")?
        .getattr("TypedDict")?
        .call(("EndpointContext", fields), Some(&kwargs))
}

pub fn exceptions_module(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    EXCEPTIONS
        .get_or_try_init(py, || {
            let module = PyModule::new(py, "fastrapi.exceptions")?;
            module.add("HTTPException", http_exception_type(py)?)?;
            module.add("FastAPIError", py.get_type::<PyFastAPIError>())?;
            module.add(
                "DependencyScopeError",
                py.get_type::<PyDependencyScopeError>(),
            )?;
            module.add(
                "PydanticV1NotSupportedError",
                py.get_type::<PyPydanticV1NotSupportedError>(),
            )?;
            module.add(
                "FastAPIDeprecationWarning",
                py.get_type::<PyFastAPIDeprecationWarning>(),
            )?;
            module.add(
                "ValidationException",
                py.get_type::<PyValidationException>(),
            )?;
            module.add(
                "RequestValidationError",
                py.get_type::<PyRequestValidationError>(),
            )?;
            module.add(
                "WebSocketRequestValidationError",
                py.get_type::<PyWebSocketRequestValidationError>(),
            )?;
            module.add(
                "ResponseValidationError",
                py.get_type::<PyResponseValidationError>(),
            )?;
            module.add("WebSocketException", websocket_exception_type(py)?)?;
            module.add(
                "WebSocketDisconnect",
                py.get_type::<PyWebSocketDisconnect>(),
            )?;
            module.add("EndpointContext", endpoint_context_type(py)?)?;
            let create_model = py.import("pydantic")?.getattr("create_model")?;
            module.add("RequestErrorModel", create_model.call1(("Request",))?)?;
            module.add("WebSocketErrorModel", create_model.call1(("WebSocket",))?)?;
            fastrapi_pyutil::imports::register_module(py, "fastrapi.exceptions", &module)?;
            Ok::<_, PyErr>(module.unbind())
        })
        .map(|module| module.bind(py))
}

pub fn exception_handlers_module(py: Python<'_>) -> PyResult<&Bound<'_, PyModule>> {
    HANDLERS
        .get_or_try_init(py, || {
            let module = fastrapi_pyutil::imports::python_source_module(
                py,
                HANDLERS_SOURCE,
                c"fastrapi/exception_handlers.py",
                c"fastrapi.exception_handlers",
            )?;
            fastrapi_pyutil::imports::register_module(py, "fastrapi.exception_handlers", &module)?;
            Ok::<_, PyErr>(module.unbind())
        })
        .map(|module| module.bind(py))
}

fn default_handlers(py: Python<'_>) -> PyResult<[(Bound<'_, PyAny>, Bound<'_, PyAny>); 3]> {
    let module = exception_handlers_module(py)?;
    Ok([
        (
            STARLETTE_HTTP_EXCEPTION.get(py)?.clone(),
            module.getattr(intern!(py, "http_exception_handler"))?,
        ),
        (
            request_validation_error_type(py)?.clone().into_any(),
            module.getattr(intern!(py, "request_validation_exception_handler"))?,
        ),
        (
            websocket_validation_error_type(py)?.clone().into_any(),
            module.getattr(intern!(
                py,
                "websocket_request_validation_exception_handler"
            ))?,
        ),
    ])
}

pub fn register_default_handlers(registry: &Bound<'_, PyDict>) -> PyResult<()> {
    for (class, handler) in default_handlers(registry.py())? {
        registry.set_default(class, handler)?;
    }
    Ok(())
}

#[must_use]
pub fn is_default_handler(handler: &Bound<'_, PyAny>) -> bool {
    default_handlers(handler.py())
        .is_ok_and(|defaults| defaults.iter().any(|(_, default)| default.is(handler)))
}

fn rust_type<'py, T: pyo3::PyTypeInfo>(
    py: Python<'py>,
    cell: &'static PyOnceLock<Py<PyType>>,
) -> &'py Bound<'py, PyType> {
    cell.get_or_init(py, || py.get_type::<T>().unbind())
        .bind(py)
}

pub fn http_exception_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    HTTP_EXCEPTION
        .get_or_try_init(py, || build_http_exception(py))
        .map(|class| class.bind(py))
}

pub fn request_validation_error_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    Ok(rust_type::<PyRequestValidationError>(
        py,
        &REQUEST_VALIDATION_ERROR,
    ))
}

pub fn websocket_validation_error_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    Ok(rust_type::<PyWebSocketRequestValidationError>(
        py,
        &WEBSOCKET_VALIDATION_ERROR,
    ))
}

pub fn deprecation_warning_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    Ok(rust_type::<PyFastAPIDeprecationWarning>(
        py,
        &DEPRECATION_WARNING,
    ))
}

fn raised(result: PyResult<Bound<'_, PyAny>>) -> PyErr {
    match result {
        Ok(value) => PyErr::from_value(value),
        Err(err) => err,
    }
}

#[must_use]
pub fn http_exception(
    py: Python<'_>,
    status_code: u16,
    detail: Option<&str>,
    headers: Option<Bound<'_, PyDict>>,
) -> PyErr {
    raised(http_exception_type(py).and_then(|class| class.call1((status_code, detail, headers))))
}

#[must_use]
pub fn request_validation_error(
    py: Python<'_>,
    errors: Bound<'_, PyAny>,
    body: Option<Bound<'_, PyAny>>,
) -> PyErr {
    raised(Bound::new(py, PyRequestValidationError::new(errors, body, None)).map(Bound::into_any))
}

#[must_use]
pub fn websocket_validation_error(py: Python<'_>, errors: Bound<'_, PyAny>) -> PyErr {
    raised(
        Bound::new(py, PyWebSocketRequestValidationError::new(errors, None)).map(Bound::into_any),
    )
}

#[must_use]
pub fn is_http_exception(value: &Bound<'_, PyAny>) -> bool {
    STARLETTE_HTTP_EXCEPTION.is_instance(value.py(), value)
}

#[must_use]
pub fn http_status_code(value: &Bound<'_, PyAny>) -> Option<u16> {
    value
        .getattr(intern!(value.py(), "status_code"))
        .ok()?
        .extract()
        .ok()
}

fn response_headers(headers: &Bound<'_, PyAny>) -> HeaderMap {
    let py = headers.py();
    let mut map = HeaderMap::new();
    let Ok(items) = headers
        .call_method0(intern!(py, "items"))
        .and_then(|items| items.try_iter())
    else {
        return map;
    };
    for item in items.flatten() {
        let Ok((name, value)) = item.extract::<(String, String)>() else {
            continue;
        };
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            map.append(name, value);
        }
    }
    map
}

pub fn http_exception_response(value: &Bound<'_, PyAny>) -> Option<Response> {
    if !is_http_exception(value) {
        return None;
    }
    let py = value.py();
    let status = StatusCode::from_u16(http_status_code(value)?).ok()?;
    let headers = value
        .getattr(intern!(py, "headers"))
        .ok()
        .filter(|headers| !headers.is_none())
        .map(|headers| response_headers(&headers))
        .unwrap_or_default();
    if !crate::status::body_allowed(status.as_u16()) {
        return Some((status, headers).into_response());
    }
    let detail = value.getattr(intern!(py, "detail")).ok()?;
    let content = PyDict::new(py);
    content.set_item(intern!(py, "detail"), detail).ok()?;
    let mut response = py_json_response_with_status(py, status, content.as_any()).ok()?;
    response.headers_mut().extend(headers);
    Some(response)
}

pub fn validation_error_response(value: &Bound<'_, PyAny>) -> Option<Response> {
    let py = value.py();
    if !value
        .is_instance(request_validation_error_type(py).ok()?)
        .ok()?
    {
        return None;
    }
    let content = PyDict::new(py);
    content
        .set_item(
            intern!(py, "detail"),
            value.call_method0(intern!(py, "errors")).ok()?,
        )
        .ok()?;
    py_json_response_with_status(py, StatusCode::UNPROCESSABLE_ENTITY, content.as_any()).ok()
}

/// Common base class for all non-exit exceptions.
#[pyclass(
    extends = PyException,
    name = "WebSocketDisconnect",
    get_all,
    from_py_object
)]
#[derive(Clone)]
pub struct PyWebSocketDisconnect {
    pub code: u16,
    pub reason: Option<String>,
}

#[pymethods]
impl PyWebSocketDisconnect {
    #[new]
    #[pyo3(signature = (code=1000, reason=None))]
    const fn new(code: u16, reason: Option<String>) -> Self {
        Self { code, reason }
    }

    #[pyo3(signature = (*_args, **_kwargs))]
    const fn __init__(&self, _args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) {}
}

#[derive(Clone)]
pub struct UnhandledError(pub Arc<PyErr>);

#[must_use]
pub fn plain_server_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "Internal Server Error",
    )
        .into_response()
}

#[must_use]
pub fn server_error_response(py: Python<'_>, err: &PyErr, server: Option<EventLoop>) -> Response {
    let traceback = is_debug(py, server)
        .then(|| format_traceback(py, err))
        .flatten();
    let mut response = match traceback {
        Some(traceback) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            traceback,
        )
            .into_response(),
        None => plain_server_error(),
    };
    response
        .extensions_mut()
        .insert(UnhandledError(Arc::new(err.clone_ref(py))));
    response
}

fn is_debug(py: Python<'_>, server: Option<EventLoop>) -> bool {
    server
        .or_else(current_server)
        .and_then(|server| server.server().app())
        .and_then(|app| app.bind(py).getattr(intern!(py, "debug")).ok())
        .is_some_and(|debug| debug.is_truthy().unwrap_or(false))
}

fn format_traceback(py: Python<'_>, err: &PyErr) -> Option<String> {
    let lines = py
        .import(intern!(py, "traceback"))
        .ok()?
        .call_method1(intern!(py, "format_exception"), (err.value(py),))
        .ok()?;
    Some(lines.extract::<Vec<String>>().ok()?.concat())
}
