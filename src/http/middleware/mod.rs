use crate::runtime::py_bridge;
use crate::http::responses::convert_auto_response;
use crate::runtime::blocking::run_python;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::exceptions::{PyKeyError, PyTypeError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyIterator, PyList, PyString, PyType};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::str;
use std::sync::Arc;
use tracing::error;

pub mod asgi;
pub mod call_next;
pub mod cors;
pub mod gzip;
pub mod httpsredirect;
mod rate_limit;
pub mod registry;
mod session;
mod trustedhost;

use axum::http::HeaderMap;
pub use call_next::PyDownstreamResponse;
pub use cors::{CORSMiddleware, build_cors_layer, parse_cors_params};
pub use gzip::{GZipMiddleware, parse_gzip_params};
pub use httpsredirect::{HTTPSRedirectMiddleware, parse_https_redirect_params};
pub use rate_limit::rate_limit;
pub use registry::{
    DeclaredLayer, MIDDLEWARE_REGISTRY, MiddlewareBuilder, MiddlewareContainer, MiddlewareRegistry,
};
pub use session::{SessionMiddleware, parse_session_params};
pub use trustedhost::{TrustedHostMiddleware, parse_trusted_host_params};

crate::cached_py_import!(INSPECT_MODULE, "inspect");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyMiddlewareKind {
    /// Legacy single-argument style: `handler(ctx_dict) -> response|None`.
    Context,
    /// Starlette-style: `handler(request, call_next) -> response`.
    CallNext,
    /// raw asgi middleware class: `async def __call__(self, scope, receive, send)`;
    Asgi,
}

#[derive(Clone)]
pub struct PyMiddleware {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub kind: PyMiddlewareKind,
    pub init_kwargs: Option<Py<PyDict>>,
}

impl PyMiddleware {
    pub fn new(py: Python<'_>, func: Py<PyAny>) -> Self {
        let bound = func.bind(py);
        let code = bound.getattr(intern!(py, "__code__")).or_else(|_| {
            bound
                .getattr(intern!(py, "__call__"))
                .and_then(|c| c.getattr(intern!(py, "__code__")))
        });
        let is_async = code
            .and_then(|c| c.getattr(intern!(py, "co_flags")))
            .ok()
            .and_then(|flags| flags.extract::<u32>().ok())
            .is_some_and(|f| (f & 0x80) != 0);

        let kind = match Self::call_param_count(py, bound) {
            Some(count) if count >= 2 => PyMiddlewareKind::CallNext,
            _ => PyMiddlewareKind::Context,
        };

        Self {
            func,
            is_async,
            kind,
            init_kwargs: None,
        }
    }

    fn call_param_count(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Option<usize> {
        let target = if obj.is_instance_of::<PyType>() {
            obj.getattr(intern!(py, "__call__")).ok()?
        } else {
            obj.clone()
        };
        let sig = INSPECT_MODULE
            .get(py)
            .ok()?
            .call_method1(intern!(py, "signature"), (target,))
            .ok()?;
        let parameters = sig.getattr(intern!(py, "parameters")).ok()?;
        let count = parameters.len().ok()?;
        Some(if obj.is_instance_of::<PyType>() {
            count.saturating_sub(1)
        } else {
            count
        })
    }

    pub fn from_custom(
        py: Python<'_>,
        obj: &Bound<'_, PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        if obj.is_instance_of::<pyo3::types::PyType>() && Self::call_param_count(py, obj) == Some(3)
        {
            let is_async = obj
                .getattr(intern!(py, "__call__"))
                .ok()
                .and_then(|call| call.getattr(intern!(py, "__code__")).ok())
                .and_then(|code| code.getattr(intern!(py, "co_flags")).ok())
                .and_then(|flags| flags.extract::<u32>().ok())
                .is_some_and(|f| (f & 0x80) != 0);
            return Ok(Self {
                func: obj.clone().unbind(),
                is_async,
                kind: PyMiddlewareKind::Asgi,
                init_kwargs: kwargs.map(|kw| kw.clone().unbind()),
            });
        }

        let instance: Bound<'_, PyAny> = if obj.is_instance_of::<pyo3::types::PyType>() {
            match obj.call((), kwargs) {
                Ok(instance) => instance,
                Err(_) => obj.call0()?,
            }
        } else if let Some(kwargs) = kwargs
            && !kwargs.is_empty()
        {
            obj.call((), Some(kwargs))?
        } else {
            obj.clone()
        };

        let param_count = INSPECT_MODULE
            .get(py)
            .ok()
            .and_then(|inspect| {
                inspect
                    .call_method1(intern!(py, "signature"), (instance.clone(),))
                    .ok()
            })
            .and_then(|sig| sig.getattr(intern!(py, "parameters")).ok())
            .and_then(|params| params.call_method0(intern!(py, "values")).ok())
            .and_then(|values| values.len().ok())
            .unwrap_or(1);

        let kind = if param_count >= 2 {
            PyMiddlewareKind::CallNext
        } else {
            PyMiddlewareKind::Context
        };

        let is_async = instance
            .get_type()
            .getattr(intern!(py, "__call__"))
            .ok()
            .and_then(|call| call.getattr(intern!(py, "__code__")).ok())
            .and_then(|code| code.getattr(intern!(py, "co_flags")).ok())
            .and_then(|flags| flags.extract::<u32>().ok())
            .is_some_and(|f| (f & 0x80) != 0);

        Ok(Self {
            func: instance.unbind(),
            is_async,
            kind,
            init_kwargs: None,
        })
    }
}

fn build_request_context(
    py: Python<'_>,
    request: &Request,
    headers: Arc<HeaderMap>,
) -> PyResult<Py<PyDict>> {
    let ctx = PyDict::new(py);
    let headers_obj = Bound::new(py, PyLazyHeaders { headers })?.into_any();

    let scope_dict = PyDict::new(py);
    scope_dict.set_item(intern!(py, "type"), intern!(py, "http"))?;
    scope_dict.set_item(intern!(py, "method"), request.method().as_str())?;
    scope_dict.set_item(intern!(py, "path"), request.uri().path())?;
    scope_dict.set_item(
        intern!(py, "query_string"),
        PyBytes::new(py, request.uri().query().unwrap_or("").as_bytes()),
    )?;

    scope_dict.set_item(intern!(py, "headers"), &headers_obj)?;
    ctx.set_item(intern!(py, "scope"), scope_dict)?;
    ctx.set_item(intern!(py, "headers"), &headers_obj)?;
    ctx.set_item(intern!(py, "method"), request.method().as_str())?;
    ctx.set_item(intern!(py, "path"), request.uri().path())?;

    Ok(ctx.unbind())
}

fn header_key_bytes<'a, 'py>(key: &'a Bound<'py, PyAny>) -> PyResult<Cow<'a, [u8]>> {
    if let Ok(s) = key.cast::<PyString>() {
        Ok(Cow::Borrowed(s.to_str()?.as_bytes()))
    } else if let Ok(b) = key.cast::<PyBytes>() {
        Ok(Cow::Borrowed(b.as_bytes()))
    } else {
        Err(PyTypeError::new_err("header keys must be str or bytes"))
    }
}

#[pyclass(
    frozen,
    name = "Headers",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyLazyHeaders {
    pub headers: Arc<HeaderMap>,
}

impl PyLazyHeaders {
    fn unique_entries(&self) -> SmallVec<[(&str, &axum::http::HeaderValue); 16]> {
        let mut out: SmallVec<[(&str, &axum::http::HeaderValue); 16]> = SmallVec::new();
        for (name, value) in self.headers.iter() {
            let name = name.as_str();
            let found = match out.last() {
                Some((last, _)) if *last == name => Some(out.len() - 1),
                _ => out.iter().position(|(n, _)| *n == name),
            };
            match found {
                Some(i) => out[i].1 = value,
                None => out.push((name, value)),
            }
        }
        out
    }

    fn last_value(&self, key: &[u8]) -> Option<&[u8]> {
        let key = str::from_utf8(key).ok()?;
        self.headers
            .get_all(key)
            .into_iter()
            .next_back()
            .map(axum::http::HeaderValue::as_bytes)
    }
}

#[pymethods]
impl PyLazyHeaders {
    fn __len__(&self) -> usize {
        self.unique_entries().len()
    }

    fn __contains__(&self, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        let key = header_key_bytes(key)?;
        Ok(match str::from_utf8(&key) {
            Ok(key) => self.headers.get(key).is_some(),
            Err(_) => false,
        })
    }

    fn __getitem__<'py>(
        &self,
        py: Python<'py>,
        key: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let key = header_key_bytes(key)?;
        self.last_value(&key)
            .map(|v| PyBytes::new(py, v))
            .ok_or_else(|| PyKeyError::new_err(String::from_utf8_lossy(&key).into_owned()))
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyIterator>> {
        let names: Vec<Bound<'py, PyBytes>> = self
            .unique_entries()
            .iter()
            .map(|(name, _)| PyBytes::new(py, name.as_bytes()))
            .collect();
        PyList::new(py, names)?.into_any().try_iter()
    }

    fn __repr__(&self) -> String {
        let entries = self.unique_entries();
        let mut out = String::with_capacity(entries.len() * 24);
        out.push_str("Headers({");
        for (i, (name, value)) in entries.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "b'{}': b'{}'",
                name,
                String::from_utf8_lossy(value.as_bytes())
            ));
        }
        out.push_str("})");
        out
    }

    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &Bound<'py, PyAny>,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let key = header_key_bytes(key)?;
        match self.last_value(&key) {
            Some(v) => Ok(PyBytes::new(py, v).into_any().unbind()),
            None => Ok(default.map(|d| d.unbind()).unwrap_or_else(|| py.None())),
        }
    }

    fn keys<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let names: Vec<Bound<'py, PyBytes>> = self
            .unique_entries()
            .iter()
            .map(|(name, _)| PyBytes::new(py, name.as_bytes()))
            .collect();
        PyList::new(py, names)
    }

    fn values<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let values: Vec<Bound<'py, PyBytes>> = self
            .unique_entries()
            .iter()
            .map(|(_, value)| PyBytes::new(py, value.as_bytes()))
            .collect();
        PyList::new(py, values)
    }

    fn items<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let items: Vec<(Bound<'py, PyBytes>, Bound<'py, PyBytes>)> = self
            .unique_entries()
            .iter()
            .map(|(name, value)| {
                (
                    PyBytes::new(py, name.as_bytes()),
                    PyBytes::new(py, value.as_bytes()),
                )
            })
            .collect();
        PyList::new(py, items)
    }
}

enum Step {
    Continue,
    Respond(Response),
    Await(py_bridge::PyTaskFuture),
}

pub struct PreparedMiddlewares {
    context: Arc<Vec<Arc<PyMiddleware>>>,
    call_next: Arc<Vec<Arc<PyMiddleware>>>,
    sync_prefix: usize,
}

impl PreparedMiddlewares {
    pub fn new(middlewares: Arc<Vec<Arc<PyMiddleware>>>) -> Self {
        let (context, call_next): (Vec<Arc<PyMiddleware>>, Vec<Arc<PyMiddleware>>) = middlewares
            .iter()
            .filter(|mw| mw.kind != PyMiddlewareKind::Asgi)
            .cloned()
            .partition(|mw| mw.kind == PyMiddlewareKind::Context);
        let sync_prefix = context
            .iter()
            .position(|mw| mw.is_async)
            .unwrap_or(context.len());

        Self {
            sync_prefix,
            context: Arc::new(context),
            call_next: Arc::new(call_next),
        }
    }

    fn sync_chain(&self) -> &[Arc<PyMiddleware>] {
        &self.context[..self.sync_prefix]
    }

    fn async_chain(&self) -> &[Arc<PyMiddleware>] {
        &self.context[self.sync_prefix..]
    }
}

pub async fn execute_py_middleware(
    middleware: Arc<PyMiddleware>,
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> Response {
    let prepared = Arc::new(PreparedMiddlewares::new(Arc::new(vec![middleware])));
    execute_py_middlewares(prepared, request, next, async_loop).await
}

pub async fn execute_py_middlewares(
    middlewares: Arc<PreparedMiddlewares>,
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> Response {
    let headers = Arc::new(request.headers().clone());
    let ctx = Python::attach(|py| build_request_context(py, &request, headers));

    let mut ctx = match ctx {
        Ok(ctx) => Some(ctx),
        Err(err) => {
            error!("Failed to build middleware context: {}", err);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // synchronous middlewares batched in a single hop.
    let sync_prefix = middlewares.sync_prefix;
    if sync_prefix > 0 {
        let chain_ctx = ctx.take();

        if let Some(chain_ctx) = chain_ctx {
            let sync_chain = middlewares.clone();
            let outcome = run_python(move |py| -> Result<Py<PyDict>, Response> {
                run_sync_chain(py, sync_chain.sync_chain(), chain_ctx)
            })
            .await;

            match outcome {
                Ok(Ok(next_ctx)) => ctx = Some(next_ctx),
                Ok(Err(resp)) => return resp,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    }

    let chain = middlewares.async_chain();
    if !chain.is_empty() {
        let Some(current_ctx) = ctx.take() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        let async_loop = async_loop.clone();
        let outcome = async move {
            let ctx = current_ctx;
            for mw in chain {
                let step = Python::attach(|py| -> PyResult<Step> {
                    let result = mw.func.bind(py).call1((ctx.bind(py),))?;
                    if result.is_none() {
                        return Ok(Step::Continue);
                    }
                    if mw.is_async {
                        let fut = py_bridge::schedule_task(py, &async_loop, result)?;
                        return Ok(Step::Await(fut));
                    }
                    Ok(Step::Respond(convert_auto_response(py, &result)))
                });

                match step {
                    Ok(Step::Continue) => {}
                    Ok(Step::Respond(resp)) => return Err(resp),
                    Ok(Step::Await(fut)) => {
                        let result = fut.await;
                        let responded = Python::attach(|py| match result {
                            Ok(value) => {
                                let bound = value.bind(py);
                                if bound.is_none() {
                                    None
                                } else {
                                    Some(convert_auto_response(py, bound))
                                }
                            }
                            Err(err) => {
                                err.print(py);
                                Some(middleware_execution_error())
                            }
                        });
                        if let Some(resp) = responded {
                            return Err(resp);
                        }
                    }
                    Err(err) => {
                        Python::attach(|py| err.print(py));
                        return Err(middleware_execution_error());
                    }
                }
            }
            Ok(())
        }
        .await;

        if let Err(resp) = outcome {
            return resp;
        }
    }

    // Starlette `(request, call_next)` middlewares
    if !middlewares.call_next.is_empty() {
        return call_next::run_call_next_stack(&middlewares.call_next, request, next, async_loop)
            .await;
    }

    next.run(request).await
}

fn middleware_execution_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Middleware Execution Error",
    )
        .into_response()
}

fn run_sync_chain(
    py: Python<'_>,
    chain: &[Arc<PyMiddleware>],
    ctx: Py<PyDict>,
) -> Result<Py<PyDict>, Response> {
    for middleware in chain {
        let middleware_func = middleware.func.bind(py);
        match middleware_func.call1((ctx.bind(py),)) {
            Ok(result) => {
                if !result.is_none() {
                    return Err(convert_auto_response(py, &result));
                }
            }
            Err(err) => {
                err.print(py);
                return Err(middleware_execution_error());
            }
        }
    }
    Ok(ctx)
}
