use crate::http::responses::convert_auto_response;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use std::sync::Arc;
use tracing::error;

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
}

#[derive(Clone)]
pub struct PyMiddleware {
    pub func: Py<PyAny>,
    pub is_async: bool,
    pub kind: PyMiddlewareKind,
}

impl PyMiddleware {
    pub fn new(py: Python<'_>, func: Py<PyAny>) -> Self {
        let bound = func.bind(py);
        let code = bound.getattr(intern!(py, "__code__")).or_else(|_| {
            bound
                .getattr("__call__")
                .and_then(|c| c.getattr("__code__"))
        });
        let is_async = code
            .and_then(|c| c.getattr("co_flags"))
            .ok()
            .and_then(|flags| flags.extract::<u32>().ok())
            .is_some_and(|f| (f & 0x80) != 0);

        Self {
            func,
            is_async,
            kind: PyMiddlewareKind::Context,
        }
    }

    pub fn from_custom(
        py: Python<'_>,
        obj: &Bound<'_, PyAny>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
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
            .and_then(|sig| sig.getattr("parameters").ok())
            .and_then(|params| params.call_method0("values").ok())
            .and_then(|values| values.len().ok())
            .unwrap_or(1);

        let kind = if param_count >= 2 {
            PyMiddlewareKind::CallNext
        } else {
            PyMiddlewareKind::Context
        };

        let is_async = instance
            .get_type()
            .getattr("__call__")
            .ok()
            .and_then(|call| call.getattr("__code__").ok())
            .and_then(|code| code.getattr("co_flags").ok())
            .and_then(|flags| flags.extract::<u32>().ok())
            .is_some_and(|f| (f & 0x80) != 0);

        Ok(Self {
            func: instance.unbind(),
            is_async,
            kind,
        })
    }
}

fn build_request_context(py: Python<'_>, request: &Request) -> PyResult<Py<PyDict>> {
    let ctx = PyDict::new(py);

    let scope_dict = PyDict::new(py);
    scope_dict.set_item("type", "http")?;
    scope_dict.set_item("method", request.method().as_str())?;
    scope_dict.set_item("path", request.uri().path())?;
    scope_dict.set_item(
        "query_string",
        PyBytes::new(py, request.uri().query().unwrap_or("").as_bytes()),
    )?;

    let headers = collect_headers(py, request.headers())?;
    scope_dict.set_item("headers", &headers)?;
    ctx.set_item("scope", scope_dict)?;
    ctx.set_item("headers", &headers)?;
    ctx.set_item("method", request.method().as_str())?;
    ctx.set_item("path", request.uri().path())?;

    Ok(ctx.unbind())
}

#[inline]
fn collect_headers(py: Python<'_>, headers: &HeaderMap) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in headers.iter() {
        dict.set_item(
            PyBytes::new(py, key.as_str().as_bytes()),
            PyBytes::new(py, value.as_bytes()),
        )?;
    }
    Ok(dict.unbind())
}

enum Step {
    Continue,
    Respond(Response),
    Await(Pin<Box<dyn Future<Output = Result<Py<PyAny>, PyErr>> + Send>>),
}

use std::future::Future;
use std::pin::Pin;

pub async fn execute_py_middleware(
    middleware: Arc<PyMiddleware>,
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> Response {
    let middlewares = Arc::new(vec![middleware]);
    execute_py_middlewares(middlewares, request, next, async_loop).await
}

pub async fn execute_py_middlewares(
    middlewares: Arc<Vec<Arc<PyMiddleware>>>,
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> Response {
    let ctx = Python::attach(|py| build_request_context(py, &request));

    let mut ctx = match ctx {
        Ok(ctx) => Some(ctx),
        Err(err) => {
            error!("Failed to build middleware context: {}", err);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let (ctx_mws, cn_mws): (Vec<Arc<PyMiddleware>>, Vec<Arc<PyMiddleware>>) = middlewares
        .iter()
        .cloned()
        .partition(|mw| mw.kind == PyMiddlewareKind::Context);
    let ctx_mws = Arc::new(ctx_mws);
    let has_call_next = !cn_mws.is_empty();

    // synchronous middlewares batched in a single hop.
    let sync_prefix = ctx_mws
        .iter()
        .position(|mw| mw.is_async)
        .unwrap_or(ctx_mws.len());

    if sync_prefix > 0 {
        let prefix = ctx_mws.clone();
        let chain_ctx = ctx.take();

        if let Some(chain_ctx) = chain_ctx {
            let outcome =
                crate::runtime::blocking::run_python(move |py| -> Result<Py<PyDict>, Response> {
                    run_sync_chain(py, &prefix[..sync_prefix], chain_ctx)
                })
                .await;

            match outcome {
                Ok(Ok(next_ctx)) => ctx = Some(next_ctx),
                Ok(Err(resp)) => return resp,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    }

    // (async) context middlewares
    let mut remaining: &[Arc<PyMiddleware>] = &ctx_mws[sync_prefix..];
    while let Some((mw, rest)) = remaining.split_first() {
        remaining = rest;

        let Some(current_ctx) = ctx.take() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        let step_result = crate::runtime::blocking::run_python({
            let mw = mw.clone();
            let async_loop = async_loop.clone();
            move |py| -> Result<(Step, Py<PyDict>), Response> {
                run_single_step(py, &mw, current_ctx, &async_loop)
            }
        })
        .await;

        let (step, ctx_back) = match step_result {
            Ok(Ok(pair)) => pair,
            Ok(Err(resp)) => return resp,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        ctx = Some(ctx_back);

        match step {
            Step::Continue => {}
            Step::Respond(resp) => return resp,
            Step::Await(fut) => {
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
                        Some(
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Middleware Execution Error",
                            )
                                .into_response(),
                        )
                    }
                });
                if let Some(resp) = responded {
                    return resp;
                }
            }
        }
    }

    // Starlette `(request, call_next)` middlewares
    if has_call_next {
        return call_next::run_call_next_stack(&cn_mws, request, next, async_loop).await;
    }

    next.run(request).await
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
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Middleware Execution Error",
                )
                    .into_response());
            }
        }
    }
    Ok(ctx)
}

fn run_single_step(
    py: Python<'_>,
    middleware: &PyMiddleware,
    ctx: Py<PyDict>,
    async_loop: &Arc<Py<PyAny>>,
) -> Result<(Step, Py<PyDict>), Response> {
    let middleware_func = middleware.func.bind(py);
    match middleware_func.call1((ctx.bind(py),)) {
        Ok(result) => {
            if result.is_none() {
                return Ok((Step::Continue, ctx));
            }

            if result.hasattr("__await__").unwrap_or(false) {
                let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
                match rsloop::rust_async::into_future_with_locals(&locals, result) {
                    Ok(fut) => Ok((Step::Await(Box::pin(fut)), ctx)),
                    Err(err) => {
                        err.print(py);
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Middleware Execution Error",
                        )
                            .into_response())
                    }
                }
            } else {
                Ok((Step::Respond(convert_auto_response(py, &result)), ctx))
            }
        }
        Err(err) => {
            err.print(py);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Middleware Execution Error",
            )
                .into_response())
        }
    }
}
