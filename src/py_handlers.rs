// ── py_handlers.rs ────────────────────────────────────────────────────────────

use crate::dependencies::{self, DependencyExecutionError};
use crate::exceptions::PyHTTPException;
use crate::request::PyRequest;
use crate::responses::convert_response_by_type;
use crate::types::route::{RequestInput, RouteHandler};
use crate::utils::local_guard;
use crate::ROUTES;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;

type DependencyResults = std::collections::HashMap<String, Arc<Py<PyAny>>>;

// ── pure-Rust handler lookup, no GIL needed ───────────────────────────────────

fn load_handler(route_key: &Arc<str>) -> Option<Arc<RouteHandler>> {
    let guard = local_guard(&*ROUTES);
    ROUTES.get(route_key.as_ref(), &guard).cloned()
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn python_error_to_response(py: Python<'_>, err: PyErr) -> Response {
    if let Ok(http_error) = err.value(py).extract::<PyRef<'_, PyHTTPException>>() {
        return http_error.to_response(py);
    }
    err.print(py);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn create_request_object(py: Python<'_>, request_input: &RequestInput) -> PyResult<Py<PyAny>> {
    let scope = PyDict::new(py);
    scope.set_item("type", "http")?;
    scope.set_item("method", request_input.method.as_str())?;
    scope.set_item("path", request_input.path.as_str())?;
    scope.set_item("query_string", request_input.query_string.as_str())?;

    let path_params = PyDict::new(py);
    for (key, value) in &request_input.path_params {
        path_params.set_item(key, value)?;
    }
    scope.set_item("path_params", path_params)?;

    let query_params = PyDict::new(py);
    for (key, value) in &request_input.query_params {
        query_params.set_item(key, value)?;
    }
    scope.set_item("query_params", query_params)?;

    let headers = PyDict::new(py);
    for (key, value) in &request_input.headers {
        headers.set_item(key, value)?;
    }
    scope.set_item("headers", headers)?;

    let cookies = PyDict::new(py);
    for (key, value) in &request_input.cookies {
        cookies.set_item(key, value)?;
    }
    scope.set_item("cookies", cookies)?;

    let py_request = PyRequest::from_scope(py, scope.into_any().unbind());
    Ok(Py::new(py, py_request)?.into_any())
}

fn merge_dependency_results(
    kwargs: &Bound<'_, PyDict>,
    dependency_results: DependencyResults,
) -> Result<(), Response> {
    let py = kwargs.py();
    for (name, value) in dependency_results {
        if kwargs.set_item(&name, value.as_ref().bind(py)).is_err() {
            return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    Ok(())
}

async fn prepare_request_context(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    request_input: RequestInput,
    payload: Option<serde_json::Value>,
) -> Result<(RequestInput, Option<Py<PyAny>>, Py<PyDict>), Response> {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(
                |py| -> Result<(RequestInput, Option<Py<PyAny>>, Py<PyDict>), Response> {
                    let request_object = if handler.dependency_needs_request {
                        match create_request_object(py, &request_input) {
                            Ok(req) => Some(req),
                            Err(err) => return Err(python_error_to_response(py, err)),
                        }
                    } else {
                        None
                    };

                    let kwargs = PyDict::new(py);
                    crate::pydantic::apply_request_data(
                        py,
                        &handler,
                        &request_input,
                        payload.as_ref(),
                        &kwargs,
                    )?;

                    Ok((request_input, request_object, kwargs.unbind()))
                },
            )
        })
        .await
    {
        Ok(result) => result,
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

// ── handler dispatch ──────────────────────────────────────────────────────────

async fn call_sync_handler(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    kwargs: Option<Py<PyDict>>,
    dependency_results: Option<DependencyResults>,
) -> Response {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let mut kwargs = kwargs;
                if kwargs.is_none() && dependency_results.is_some() {
                    kwargs = Some(PyDict::new(py).unbind());
                }

                if let Some(results) = dependency_results {
                    if let Some(kwargs_ref) = kwargs.as_ref() {
                        if let Err(response) =
                            merge_dependency_results(kwargs_ref.bind(py), results)
                        {
                            return response;
                        }
                    }
                }

                let py_func = handler.func.bind(py);
                let result = match &kwargs {
                    Some(kw) => py_func.call((), Some(&kw.bind(py))),
                    None => py_func.call0(),
                };
                match result {
                    Ok(result) => convert_response_by_type(py, &result, handler.response_type),
                    Err(err) => python_error_to_response(py, err),
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn call_async_handler(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    kwargs: Option<Py<PyDict>>,
    dependency_results: Option<DependencyResults>,
) -> Response {
    let response_type = handler.response_type;
    let future = match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| -> Result<_, Response> {
                let mut kwargs = kwargs;
                if kwargs.is_none() && dependency_results.is_some() {
                    kwargs = Some(PyDict::new(py).unbind());
                }

                if let Some(results) = dependency_results {
                    if let Some(kwargs_ref) = kwargs.as_ref() {
                        merge_dependency_results(kwargs_ref.bind(py), results)?;
                    }
                }

                let py_func = handler.func.bind(py);
                let coroutine = match &kwargs {
                    Some(kw) => py_func.call((), Some(&kw.bind(py))),
                    None => py_func.call0(),
                }
                .map_err(|err| python_error_to_response(py, err))?;

                pyo3_async_runtimes::tokio::into_future(coroutine)
                    .map_err(|err| python_error_to_response(py, err))
            })
        })
        .await
    {
        Ok(Ok(future)) => future,
        Ok(Err(response)) => return response,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    match future.await {
        Ok(result) => {
            Python::attach(|py| convert_response_by_type(py, &result.bind(py), response_type))
        }
        Err(err) => Python::attach(|py| python_error_to_response(py, err)),
    }
}

// ── public entry points ───────────────────────────────────────────────────────

pub async fn run_py_handler_with_request(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    request_input: RequestInput,
    payload: Option<serde_json::Value>,
) -> Response {
    let handler = match load_handler(&route_key) {
        Some(h) => h,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    // ── fast path: sync handler + no dependencies ──────────────────────────────
    // Single GIL acquisition inside spawn_blocking — never touches the tokio thread.
    if !handler.is_async && handler.dependencies.is_empty() {
        return rt_handle
            .spawn_blocking(move || {
                Python::attach(|py| {
                    let kwargs = PyDict::new(py);
                    if let Err(resp) = crate::pydantic::apply_request_data(
                        py,
                        &handler,
                        &request_input,
                        payload.as_ref(),
                        &kwargs,
                    ) {
                        return resp;
                    }
                    let py_func = handler.func.bind(py);
                    match py_func.call((), Some(&kwargs)) {
                        Ok(result) => convert_response_by_type(py, &result, handler.response_type),
                        Err(err) => python_error_to_response(py, err),
                    }
                })
            })
            .await
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let (request_input, request_object, kwargs) =
        match prepare_request_context(rt_handle.clone(), handler.clone(), request_input, payload)
            .await
        {
            Ok(res) => res,
            Err(response) => return response,
        };

    let dependency_results = if handler.dependencies.is_empty() {
        None
    } else {
        match dependencies::execute_dependencies(
            &handler.dependencies,
            &request_input,
            request_object,
        )
        .await
        {
            Ok(results) => Some(results),
            Err(DependencyExecutionError::Response(response)) => return response,
            Err(DependencyExecutionError::Python(err)) => {
                return Python::attach(|py| python_error_to_response(py, err));
            }
        }
    };

    if handler.is_async {
        call_async_handler(rt_handle, handler, Some(kwargs), dependency_results).await
    } else {
        call_sync_handler(rt_handle, handler, Some(kwargs), dependency_results).await
    }
}

pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
) -> Response {
    let handler = match load_handler(&route_key) {
        Some(handler) => handler,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    if handler.is_async {
        call_async_handler(rt_handle, handler, None, None).await
    } else {
        call_sync_handler(rt_handle, handler, None, None).await
    }
}
