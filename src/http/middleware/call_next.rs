use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};

#[pyclass(
    frozen,
    name = "DownstreamResponse",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyDownstreamResponse {
    #[pyo3(get)]
    pub status_code: u16,
    #[pyo3(get)]
    pub headers: Py<PyDict>,
    #[pyo3(get)]
    pub body: Py<PyBytes>,
}

#[pymethods]
impl PyDownstreamResponse {
    fn __repr__(&self) -> String {
        format!("DownstreamResponse(status_code={})", self.status_code)
    }

    fn text(&self, py: Python<'_>) -> String {
        String::from_utf8_lossy(self.body.bind(py).as_bytes()).into_owned()
    }
}

#[pyclass(
    frozen,
    name = "CallNext",
    module = "fastrapi.middleware",
    skip_from_py_object
)]
pub struct PyCallNext {
    future: Py<PyAny>,
}

#[pymethods]
impl PyCallNext {
    fn __call__<'py>(&self, py: Python<'py>, _request: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
        self.future.bind(py).clone()
    }

    fn __await<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.future.bind(py).clone()
    }
}

fn middleware_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Middleware Execution Error",
    )
        .into_response()
}

fn response_to_py(py: Python<'_>, response: Response) -> PyResult<Bound<'_, PyAny>> {
    let status = response.status().as_u16();

    let mut header_pairs: Vec<(String, String)> = Vec::with_capacity(response.headers().len());
    for (key, value) in response.headers().iter() {
        if let (Some(k), Some(v)) = (key.as_str().into(), value.to_str().ok()) {
            header_pairs.push((k.to_owned(), v.to_owned()));
        }
    }
    let body_bytes = crate::globals::PYTHON_RUNTIME.block_on(async move {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_default()
    });

    let headers_dict = PyDict::new(py);
    for (k, v) in header_pairs {
        headers_dict.set_item(k, v)?;
    }

    Bound::new(
        py,
        PyDownstreamResponse {
            status_code: status,
            headers: headers_dict.unbind(),
            body: PyBytes::new(py, &body_bytes).unbind(),
        },
    )
    .map(|bound| bound.into_any())
}

fn py_to_response(py: Python<'_>, value: &Bound<'_, PyAny>) -> Response {
    if let Ok(downstream) = value.cast::<PyDownstreamResponse>() {
        let ds = downstream.borrow();
        let status =
            StatusCode::from_u16(ds.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut builder = Response::builder().status(status);
        if let Ok(dict) = ds.headers.bind(py).cast::<PyDict>() {
            for (k, v) in dict.iter() {
                if let (Ok(k), Ok(v)) = (k.extract::<&str>(), v.extract::<&str>()) {
                    builder = builder.header(k, v);
                }
            }
        }
        return builder
            .body(Body::from(ds.body.bind(py).as_bytes().to_vec()))
            .unwrap_or_else(|_| middleware_error());
    }

    crate::http::responses::convert_auto_response(py, value)
}

enum Outcome {
    Respond(Response),
    Await(Pin<Box<dyn Future<Output = Result<Py<PyAny>, PyErr>> + Send>>),
}

pub(crate) async fn run_call_next_stack(
    middlewares: &[Arc<super::PyMiddleware>],
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> Response {
    drive(middlewares.to_vec(), 0, request, next, async_loop).await
}

#[allow(clippy::too_many_arguments)]
fn drive(
    middlewares: Vec<Arc<super::PyMiddleware>>,
    idx: usize,
    request: Request,
    next: Next,
    async_loop: Arc<Py<PyAny>>,
) -> futures_util::future::BoxFuture<'static, Response> {
    Box::pin(async move {
        let Some(mw) = middlewares.get(idx).cloned() else {
            return next.run(request).await;
        };

        let (parts, body) = request.into_parts();
        let method = parts.method.clone();
        let path = parts.uri.path().to_owned();
        let query = parts.uri.query().unwrap_or("").to_owned();
        let headers = Arc::new(parts.headers.clone());
        let downstream_request = Request::from_parts(parts, body);

        let (tx, rx) = tokio::sync::oneshot::channel::<Response>();
        {
            let inner_mws = middlewares.clone();
            let inner_loop = async_loop.clone();
            tokio::spawn(async move {
                let response =
                    drive(inner_mws, idx + 1, downstream_request, next, inner_loop).await;
                let _ = tx.send(response);
            });
        }

        let outcome =
            crate::runtime::blocking::run_python(move |py| -> Result<Outcome, Response> {
                invoke(
                    py,
                    &mw,
                    method.as_str(),
                    &path,
                    &query,
                    headers,
                    rx,
                    &async_loop,
                )
            })
            .await;

        match outcome {
            Ok(Ok(Outcome::Respond(response))) => response,
            Ok(Ok(Outcome::Await(fut))) => {
                let awaited = fut.await;
                Python::attach(|py| match awaited {
                    Ok(value) => py_to_response(py, value.bind(py)),
                    Err(err) => {
                        err.print(py);
                        middleware_error()
                    }
                })
            }
            _ => Python::attach(|py| {
                tracing::error!("call_next middleware returned no response");
                let _ = py;
                middleware_error()
            }),
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn invoke(
    py: Python<'_>,
    mw: &super::PyMiddleware,
    method: &str,
    path: &str,
    query: &str,
    headers: Arc<axum::http::HeaderMap>,
    downstream: tokio::sync::oneshot::Receiver<Response>,
    async_loop: &Arc<Py<PyAny>>,
) -> Result<Outcome, Response> {
    let fail = |err: PyErr| -> Response {
        err.print(py);
        middleware_error()
    };

    let scope = PyDict::new(py);
    let _ = scope.set_item(intern!(py, "type"), intern!(py, "http"));
    let _ = scope.set_item(intern!(py, "method"), method);
    let _ = scope.set_item(intern!(py, "path"), path);
    let _ = scope.set_item(intern!(py, "query_string"), query);

    let req_obj = crate::http::request::PyRequest::create_bound(
        py,
        scope.into_any().unbind(),
        Some(headers),
        None,
    )
    .map_err(fail)?
    .into_any()
    .unbind();

    let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
    let bridged = async move {
        let response = downstream.await.map_err(|_| {
            pyo3::exceptions::PyRuntimeError::new_err("call_next pipeline terminated")
        })?;
        Python::attach(|py| Ok(response_to_py(py, response)?.unbind()))
    };
    let future = rsloop::rust_async::future_into_py_with_locals(py, locals.clone(), bridged)
        .map_err(fail)?
        .unbind();

    let call_next = Bound::new(py, PyCallNext { future })
        .map_err(fail)?
        .into_any()
        .unbind();

    let result = mw.func.bind(py).call1((req_obj, call_next)).map_err(fail)?;

    if result.is_none() {
        tracing::error!("call_next middleware returned None without awaiting call_next");
        return Err(middleware_error());
    }
    if result.hasattr("__await__").unwrap_or(false) {
        let fut = rsloop::rust_async::into_future_with_locals(&locals, result).map_err(fail)?;
        return Ok(Outcome::Await(Box::pin(fut)));
    }

    Ok(Outcome::Respond(py_to_response(py, &result)))
}
