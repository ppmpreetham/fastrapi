//! This module contains helper functions for the Python response classes.
//! The functions render the content, make the header list, and set cookies.

use crate::middleware::asgi::send_response;
use crate::response_convert::convert_auto_response;
use crate::responses::PyStreamingResponse;
use crate::sse::PyEventSourceResponse;
use cookie::time::Duration;
use cookie::{Cookie, SameSite};
use fastrapi_pyutil::imports::SIMPLE_COOKIE;
use fastrapi_runtime::blocking;
use fastrapi_runtime::py_bridge;
use pyo3::exceptions::{
    PyAssertionError, PyRuntimeError, PyStopAsyncIteration, PyStopIteration, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::PyType;
use pyo3::types::{PyBytes, PyDict, PyList, PyString};
use pyo3::{Borrowed, PyTypeInfo, intern};
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;

pub struct HeadersArg(pub Py<PyDict>);

impl FromPyObject<'_, '_> for HeadersArg {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, '_, PyAny>) -> PyResult<Self> {
        if let Ok(dict) = obj.cast::<PyDict>() {
            return Ok(Self(dict.to_owned().unbind()));
        }
        let py = obj.py();
        let dict = PyDict::new(py);
        for item in obj.call_method0(intern!(py, "items"))?.try_iter()? {
            let (key, value): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item?.extract()?;
            dict.set_item(key, value)?;
        }
        Ok(Self(dict.unbind()))
    }
}

fastrapi_pyutil::cached_py_import!(pub(crate) JSON_DUMPS, "json", "dumps");
fastrapi_pyutil::cached_py_import!(METHOD_DESCRIPTOR, "types", "MethodDescriptorType");
fastrapi_pyutil::cached_py_import!(GETSET_DESCRIPTOR, "types", "GetSetDescriptorType");
fastrapi_pyutil::cached_py_import!(WRAPPER_DESCRIPTOR, "types", "WrapperDescriptorType");
fastrapi_pyutil::cached_py_import!(
    MUTABLE_HEADERS,
    "starlette.datastructures",
    "MutableHeaders"
);

pub struct ResponseParts {
    pub content: Py<PyAny>,
    pub status_code: u16,
    pub headers: Option<Py<PyDict>>,
    pub media_type: Option<String>,
    pub background: Option<Py<PyAny>>,
    pub raw_headers: Py<PyList>,
    pub body: Py<PyAny>,
    pub charset: String,
}

/// What a Python subclass of a native response class overrides about rendering,
/// recorded by `__init_subclass__` when the class is defined.
#[derive(Clone, Default)]
pub struct RenderOverrides {
    /// The class defines its own `render`, which its `__init__` calls.
    render: bool,
    media_type: MediaTypeOverride,
    charset: Option<String>,
}

/// The `media_type` of a response class.
#[derive(Clone, Default)]
enum MediaTypeOverride {
    /// The native class's default.
    #[default]
    Inherited,
    /// A value the class sets itself, which may be `None`.
    Own(Option<String>),
}

static RENDER_OVERRIDES: LazyLock<papaya::HashMap<usize, (Py<PyType>, RenderOverrides)>> =
    LazyLock::new(papaya::HashMap::new);

impl RenderOverrides {
    /// The overrides of `cls`, a class built on the native response class `T`.
    /// The native classes themselves have none, so their construction never looks them up.
    pub fn of<T: PyTypeInfo>(cls: &Bound<'_, PyType>) -> Self {
        if cls.is(T::type_object(cls.py())) {
            return Self::default();
        }
        let key = cls.as_ptr() as usize;
        RENDER_OVERRIDES
            .pin()
            .get(&key)
            .map(|(_, overrides)| overrides.clone())
            .unwrap_or_default()
    }

    /// Records what `cls` overrides. A class that overrides `render` gets an `__init__`
    /// that calls it, as Starlette's `Response.__init__` does, unless it defines its own.
    pub fn register(cls: &Bound<'_, PyType>) -> PyResult<()> {
        let py = cls.py();
        let overrides = Self {
            render: own_attribute(cls, intern!(py, "render"))?.is_some(),
            media_type: match own_attribute(cls, intern!(py, "media_type"))? {
                Some(media_type) => MediaTypeOverride::Own(media_type.extract()?),
                None => MediaTypeOverride::Inherited,
            },
            charset: own_attribute(cls, intern!(py, "charset"))?
                .map(|charset| charset.extract())
                .transpose()?,
        };
        if overrides.render
            && own_attribute(cls, intern!(py, "__init__"))?.is_none()
            && let Some(init) = cls.getattr_opt(intern!(py, "_render_with_override"))?
        {
            cls.setattr(intern!(py, "__init__"), init)?;
        }
        RENDER_OVERRIDES
            .pin()
            .insert(cls.as_ptr() as usize, (cls.clone().unbind(), overrides));
        Ok(())
    }
}

/// The value of `name` on `cls` when a Python class sets it in place of the
/// native class's descriptor.
fn own_attribute<'py>(
    cls: &Bound<'py, PyType>,
    name: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = cls.py();
    let value = cls.getattr(name)?;
    let native = value.is_instance(METHOD_DESCRIPTOR.get(py)?)?
        || value.is_instance(GETSET_DESCRIPTOR.get(py)?)?
        || value.is_instance(WRAPPER_DESCRIPTOR.get(py)?)?;
    Ok((!native).then_some(value))
}

pub fn response_parts(
    py: Python<'_>,
    class_name: &str,
    overrides: RenderOverrides,
    content: Option<Py<PyAny>>,
    status_code: u16,
    headers: Option<Py<PyDict>>,
    media_type: Option<String>,
    background: Option<Py<PyAny>>,
) -> PyResult<ResponseParts> {
    let content = content.unwrap_or_else(|| py.None());
    let media_type = media_type.or_else(|| match overrides.media_type {
        MediaTypeOverride::Own(media_type) => media_type,
        MediaTypeOverride::Inherited => match class_name {
            "HTMLResponse" => Some("text/html"),
            "PlainTextResponse" => Some("text/plain"),
            "JSONResponse" | "ORJSONResponse" | "UJSONResponse" => Some("application/json"),
            _ => None,
        }
        .map(str::to_owned),
    });
    let charset = overrides.charset.unwrap_or_else(|| "utf-8".to_owned());
    // A class's own `render` runs in its `__init__`, once the instance exists.
    let rendered = class_name != "StreamingResponse" && !overrides.render;
    let body = rendered
        .then(|| render(py, content.bind(py), class_name, &charset))
        .transpose()?;
    let raw_headers = init_headers(
        py,
        headers.as_ref().map(|headers| headers.bind(py).as_any()),
        body.as_ref(),
        status_code,
        media_type.as_deref(),
        &charset,
    )?;
    let body = body.map_or_else(|| py.None(), Bound::unbind);
    Ok(ResponseParts {
        content,
        status_code,
        headers,
        media_type,
        background,
        raw_headers,
        body,
        charset,
    })
}

pub fn render<'py>(
    py: Python<'py>,
    content: &Bound<'py, PyAny>,
    kind: &str,
    charset: &str,
) -> PyResult<Bound<'py, PyAny>> {
    if matches!(kind, "JSONResponse" | "ORJSONResponse" | "UJSONResponse") {
        if charset.eq_ignore_ascii_case("utf-8")
            && let Some(rendered) = fastrapi_pyutil::json::plain_json_bytes(content)?
        {
            return Ok(rendered.into_any());
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item(pyo3::intern!(py, "ensure_ascii"), false)?;
        kwargs.set_item(pyo3::intern!(py, "allow_nan"), false)?;
        kwargs.set_item(pyo3::intern!(py, "separators"), (",", ":"))?;
        return JSON_DUMPS
            .get(py)?
            .call((content,), Some(&kwargs))?
            .call_method1(pyo3::intern!(py, "encode"), (charset,));
    }
    if content.is_none() {
        return Ok(PyBytes::new(py, b"").into_any());
    }
    if content.is_instance_of::<PyBytes>() || content.get_type().name()?.to_str()? == "memoryview" {
        return Ok(content.clone());
    }
    content.call_method1(pyo3::intern!(py, "encode"), (charset,))
}

pub fn headers<'py>(py: Python<'py>, raw: &Py<PyList>) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(pyo3::intern!(py, "raw"), raw.bind(py))?;
    MUTABLE_HEADERS.get(py)?.call((), Some(&kwargs))
}

pub fn init_headers(
    py: Python<'_>,
    supplied: Option<&Bound<'_, PyAny>>,
    body: Option<&Bound<'_, PyAny>>,
    status: u16,
    media_type: Option<&str>,
    charset: &str,
) -> PyResult<Py<PyList>> {
    let raw = PyList::empty(py);
    if let Some(supplied) = supplied {
        for item in supplied
            .call_method0(pyo3::intern!(py, "items"))?
            .try_iter()?
        {
            let item = item?;
            let name = item
                .get_item(0)?
                .call_method0(pyo3::intern!(py, "lower"))?
                .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
            let value = item
                .get_item(1)?
                .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
            raw.append((name, value))?;
        }
    }
    let raw = raw.unbind();
    let headers = headers(py, &raw)?;
    if status >= 200
        && status != 204
        && status != 304
        && !headers.contains("content-length")?
        && let Some(body) = body
    {
        headers.set_item("content-length", body.len()?.to_string())?;
    }
    if let Some(media_type) = media_type
        && !headers.contains("content-type")?
    {
        headers.set_item("content-type", content_type(media_type, charset).as_ref())?;
    }
    Ok(raw)
}

#[must_use]
pub fn content_type<'a>(media_type: &'a str, charset: &str) -> Cow<'a, str> {
    let has_charset = media_type
        .as_bytes()
        .windows(8)
        .any(|window| window.eq_ignore_ascii_case(b"charset="));
    if !media_type.starts_with("text/") || has_charset {
        return Cow::Borrowed(media_type);
    }
    if charset == "utf-8" {
        match media_type {
            "text/plain" => return Cow::Borrowed("text/plain; charset=utf-8"),
            "text/html" => return Cow::Borrowed("text/html; charset=utf-8"),
            "text/event-stream" => return Cow::Borrowed("text/event-stream; charset=utf-8"),
            _ => {}
        }
    }
    Cow::Owned(format!("{media_type}; charset={charset}"))
}

pub fn set_cookie(
    py: Python<'_>,
    raw: &Py<PyList>,
    key: String,
    value: String,
    max_age: Option<i64>,
    expires: Option<&Bound<'_, PyAny>>,
    path: Option<String>,
    domain: Option<String>,
    secure: bool,
    httponly: bool,
    samesite: Option<String>,
    partitioned: bool,
) -> PyResult<()> {
    let same_site = match samesite.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("lax") => Some(SameSite::Lax),
        Some("strict") => Some(SameSite::Strict),
        Some("none") => Some(SameSite::None),
        None => None,
        _ => {
            return Err(PyAssertionError::new_err(
                "samesite must be either 'strict', 'lax' or 'none'",
            ));
        }
    };
    if partitioned && py.version_info() < (3, 14) {
        return Err(PyValueError::new_err(
            "Partitioned cookies are only supported in Python 3.14 and above.",
        ));
    }
    let mut cookie = Cookie::new(key, value);
    cookie.set_secure(secure);
    cookie.set_http_only(httponly);
    if let Some(value) = path {
        cookie.set_path(value);
    }
    if let Some(value) = domain {
        cookie.set_domain(value);
    }
    if let Some(value) = same_site {
        cookie.set_same_site(value);
    }
    if let Some(value) = max_age {
        cookie.set_max_age(Duration::seconds(value));
    }
    let jar = SIMPLE_COOKIE.get(py)?.call0()?;
    jar.set_item(cookie.name(), cookie.value())?;
    let morsel = jar.get_item(cookie.name())?;
    if let Some(value) = cookie.path() {
        morsel.set_item(pyo3::intern!(py, "path"), value)?;
    }
    if let Some(value) = cookie.domain() {
        morsel.set_item(pyo3::intern!(py, "domain"), value)?;
    }
    if let Some(value) = cookie.max_age() {
        morsel.set_item("max-age", value.whole_seconds())?;
    }
    if secure {
        morsel.set_item(pyo3::intern!(py, "secure"), true)?;
    }
    if httponly {
        morsel.set_item(pyo3::intern!(py, "httponly"), true)?;
    }
    if let Some(value) = samesite {
        morsel.set_item(pyo3::intern!(py, "samesite"), value)?;
    }
    if partitioned {
        morsel.set_item(pyo3::intern!(py, "partitioned"), true)?;
    }
    if let Some(value) = expires {
        if value.is_instance(
            &py.import("datetime")?
                .getattr(pyo3::intern!(py, "datetime"))?,
        )? {
            let kwargs = PyDict::new(py);
            kwargs.set_item(pyo3::intern!(py, "usegmt"), true)?;
            morsel.set_item(
                "expires",
                py.import("email.utils")?
                    .getattr(pyo3::intern!(py, "format_datetime"))?
                    .call((value,), Some(&kwargs))?,
            )?;
        } else {
            morsel.set_item(pyo3::intern!(py, "expires"), value)?;
        }
    }
    let encoded = morsel
        .call_method0(pyo3::intern!(py, "OutputString"))?
        .call_method1(pyo3::intern!(py, "encode"), ("latin-1",))?;
    raw.bind(py)
        .append((PyBytes::new(py, b"set-cookie"), encoded))
}

pub fn asgi_call<'py>(
    response: &Bound<'py, PyAny>,
    send: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = response.py();
    let sse = response.is_instance_of::<PyEventSourceResponse>();
    let body = if response.is_instance_of::<PyStreamingResponse>() && !sse {
        AsgiBody::Stream(StreamSource::of(response)?)
    } else {
        AsgiBody::Converted(convert_auto_response(py, response))
    };
    let background = response.getattr(intern!(py, "background"))?.unbind();
    let awaits_background = !background.is_none(py) && !sse;
    let locals = py_bridge::get_current_locals(py)?;
    let send = send.unbind();
    py_bridge::future_into_py_with_locals(py, locals.clone(), async move {
        match body {
            AsgiBody::Stream(source) => source.send(&locals, &send).await?,
            AsgiBody::Converted(response) => send_response(&locals, &send, response).await?,
        }
        if awaits_background {
            let pending = Python::attach(|py| {
                py_bridge::into_future_with_locals(&locals, background.bind(py).call0()?)
            })?;
            pending.await?;
        }
        Ok(())
    })
}

enum AsgiBody {
    Stream(StreamSource),
    Converted(axum::response::Response),
}

struct StreamSource {
    status: u16,
    headers: Py<PyList>,
    iterator: Py<PyAny>,
    is_async: bool,
    charset: Arc<str>,
}

impl StreamSource {
    fn of(response: &Bound<'_, PyAny>) -> PyResult<Self> {
        let py = response.py();
        let content = response.getattr(intern!(py, "content"))?;
        let is_async = content.hasattr(intern!(py, "__aiter__"))?;
        let iterator = if is_async {
            content.call_method0(intern!(py, "__aiter__"))?
        } else {
            content.try_iter()?.into_any()
        };
        Ok(Self {
            status: response.getattr(intern!(py, "status_code"))?.extract()?,
            headers: response
                .getattr(intern!(py, "raw_headers"))?
                .cast_into()?
                .unbind(),
            iterator: iterator.unbind(),
            is_async,
            charset: response
                .getattr(intern!(py, "charset"))?
                .extract::<String>()
                .map_or_else(|_| Arc::from("utf-8"), Arc::from),
        })
    }

    async fn send(self, locals: &py_bridge::TaskLocals, send: &Py<PyAny>) -> PyResult<()> {
        send_message(locals, send, |py| {
            let message = PyDict::new(py);
            message.set_item(intern!(py, "type"), intern!(py, "http.response.start"))?;
            message.set_item(intern!(py, "status"), self.status)?;
            message.set_item(intern!(py, "headers"), self.headers.bind(py))?;
            Ok(message)
        })
        .await?;
        while let Some(chunk) = self.next_chunk(locals).await? {
            send_message(locals, send, |py| body_message(py, &chunk, true)).await?;
        }
        send_message(locals, send, |py| body_message(py, b"", false)).await
    }

    async fn next_chunk(&self, locals: &py_bridge::TaskLocals) -> PyResult<Option<Vec<u8>>> {
        if self.is_async {
            let pending = Python::attach(|py| {
                let next = self
                    .iterator
                    .bind(py)
                    .call_method0(intern!(py, "__anext__"))?;
                py_bridge::into_future_with_locals(locals, next)
            })?;
            return match pending.await {
                Ok(chunk) => {
                    Python::attach(|py| encode_chunk(chunk.bind(py), &self.charset)).map(Some)
                }
                Err(err) if Python::attach(|py| err.is_instance_of::<PyStopAsyncIteration>(py)) => {
                    Ok(None)
                }
                Err(err) => Err(err),
            };
        }
        let iterator = Python::attach(|py| self.iterator.clone_ref(py));
        let charset = Arc::clone(&self.charset);
        blocking::run_python(move |py| {
            match iterator.bind(py).call_method0(intern!(py, "__next__")) {
                Ok(chunk) => encode_chunk(&chunk, &charset).map(Some),
                Err(err) if err.is_instance_of::<PyStopIteration>(py) => Ok(None),
                Err(err) => Err(err),
            }
        })
        .await
        .map_err(|err| PyRuntimeError::new_err(err.to_string()))?
    }
}

fn encode_chunk(chunk: &Bound<'_, PyAny>, charset: &str) -> PyResult<Vec<u8>> {
    if chunk.is_instance_of::<PyString>() {
        return chunk
            .call_method1(intern!(chunk.py(), "encode"), (charset,))?
            .extract();
    }
    chunk.extract()
}

fn body_message<'py>(
    py: Python<'py>,
    chunk: &[u8],
    more_body: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let message = PyDict::new(py);
    message.set_item(intern!(py, "type"), intern!(py, "http.response.body"))?;
    message.set_item(intern!(py, "body"), PyBytes::new(py, chunk))?;
    message.set_item(intern!(py, "more_body"), more_body)?;
    Ok(message)
}

async fn send_message(
    locals: &py_bridge::TaskLocals,
    send: &Py<PyAny>,
    message: impl for<'py> FnOnce(Python<'py>) -> PyResult<Bound<'py, PyDict>>,
) -> PyResult<()> {
    let pending = Python::attach(|py| {
        py_bridge::into_future_with_locals(locals, send.bind(py).call1((message(py)?,))?)
    })?;
    pending.await.map(drop)
}

#[pymethods]
impl PyStreamingResponse {
    fn stream_response<'py>(
        slf: &Bound<'py, Self>,
        send: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let source = StreamSource::of(slf.as_any())?;
        let locals = py_bridge::get_current_locals(py)?;
        let send = send.unbind();
        py_bridge::future_into_py_with_locals(py, locals.clone(), async move {
            source.send(&locals, &send).await
        })
    }

    fn listen_for_disconnect<'py>(
        &self,
        py: Python<'py>,
        receive: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let locals = py_bridge::get_current_locals(py)?;
        let receive = receive.unbind();
        py_bridge::future_into_py_with_locals(py, locals.clone(), async move {
            loop {
                let pending = Python::attach(|py| {
                    py_bridge::into_future_with_locals(&locals, receive.bind(py).call0()?)
                })?;
                let message = pending.await?;
                let disconnected = Python::attach(|py| {
                    message
                        .bind(py)
                        .get_item(intern!(py, "type"))
                        .and_then(|kind| kind.eq(intern!(py, "http.disconnect")))
                })?;
                if disconnected {
                    return Ok(());
                }
            }
        })
    }
}
