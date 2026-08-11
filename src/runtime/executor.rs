use crate::ffi::exceptions::PyHTTPException;
use crate::ffi::pydantic;
use crate::http::request::PyRequest;
use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::{BodyPayload, PathParamRange, RequestInput, RouteHandler};
use crate::types::response::ResponseType;
use axum::{
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString};
use smallvec::SmallVec;
use std::future::Future;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::error;

static SYNC_HANDLER_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> =
    std::sync::OnceLock::new();

#[inline(always)]
fn get_sync_semaphore() -> &'static tokio::sync::Semaphore {
    SYNC_HANDLER_SEMAPHORE.get_or_init(|| {
        let permits = crate::globals::config().sync_threads;
        tokio::sync::Semaphore::new(permits)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    SyncNoArgs,
    AsyncNoArgs,
    SyncNoDeps,
    SyncDepsNoReq,
    SyncDepsReq,
    AsyncNoDeps,
    AsyncSyncDepsNoReq,
    AsyncSyncDepsReq,
    AsyncAsyncDepsNoReq,
    AsyncAsyncDepsReq,
}

#[inline(always)]
fn specialized_response_conversion(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    handler: &RouteHandler,
) -> Response {
    match handler.response.response_type {
        ResponseType::PlainText => {
            if let Ok(s) = result.cast::<PyString>() {
                return s.to_string_lossy().into_owned().into_response();
            }

            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }

        _ => match crate::http::responses::convert_response_by_type(py, result, handler) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        },
    }
}

fn python_error_to_response(py: Python<'_>, err: PyErr) -> Response {
    if let Ok(http_error) = err.value(py).extract::<PyRef<'_, PyHTTPException>>() {
        return http_error.to_response(py);
    }
    log_python_error(py, &err);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn log_python_error(py: Python<'_>, err: &PyErr) {
    let rendered = (|| -> PyResult<String> {
        let traceback_bound = py.import(intern!(py, "traceback"))?;

        let traceback_obj = err
            .traceback(py)
            .map(|tb| tb.into_any().unbind())
            .unwrap_or_else(|| py.None());
        let lines = traceback_bound.call_method1(
            intern!(py, "format_exception"),
            (err.get_type(py), err.value(py), traceback_obj.bind(py)),
        )?;
        Ok(lines.extract::<Vec<String>>()?.concat())
    })();

    match rendered {
        Ok(traceback) => error!(target: "fastrapi::python", "Python handler error:\n{}", traceback),
        Err(format_err) => error!(
            target: "fastrapi::python",
            "Python handler error: {}; traceback formatting failed: {}", err, format_err
        ),
    }
}

fn build_request_input_from_parts<'a>(
    parts: &'a Parts,
    param_ranges: &'a [PathParamRange],
) -> RequestInput<'a> {
    let path_str = parts.uri.path();
    let path_params = OnceLock::new();

    if !param_ranges.is_empty() {
        let path_params_vec: SmallVec<[(Arc<str>, &'a str); 8]> = param_ranges
            .iter()
            .map(|r| (r.key.clone(), &path_str[r.start..r.end]))
            .collect();
        let _ = path_params.set(path_params_vec);
    }

    RequestInput {
        method: parts.method.as_str(),
        path: path_str,
        query_string: parts.uri.query().unwrap_or(""),
        headers: &parts.headers,
        path_params,
        query_params: OnceLock::new(),
        cookies: OnceLock::new(),
    }
}

fn create_request_object(py: Python<'_>, request_input: &RequestInput<'_>) -> PyResult<Py<PyAny>> {
    let scope = PyDict::new(py);
    scope.set_item(intern!(py, "type"), intern!(py, "http"))?;
    scope.set_item(intern!(py, "method"), request_input.method)?;
    scope.set_item(intern!(py, "path"), request_input.path)?;
    scope.set_item(intern!(py, "query_string"), request_input.query_string)?;

    let path_params = PyDict::new(py);
    if let Some(params) = request_input.path_params.get() {
        params
            .iter()
            .try_for_each(|(k, v)| path_params.set_item(k.as_ref(), v))?;
    }
    scope.set_item(intern!(py, "path_params"), path_params)?;

    let query_params = PyDict::new(py);
    request_input
        .get_all_query_params()
        .into_iter()
        .try_for_each(|(k, v)| query_params.set_item(k.as_ref(), v.as_ref()))?;
    scope.set_item(intern!(py, "query_params"), query_params)?;

    scope.set_item(
        intern!(py, "headers"),
        PyList::new(
            py,
            request_input.headers.iter().map(|(key, value)| {
                (
                    PyBytes::new(py, key.as_str().as_bytes()),
                    PyBytes::new(py, value.as_bytes()),
                )
            }),
        )?,
    )?;

    let cookies = PyDict::new(py);
    request_input
        .get_all_cookies()
        .into_iter()
        .try_for_each(|(k, v)| cookies.set_item(k, v))?;
    scope.set_item(intern!(py, "cookies"), cookies)?;

    let bound_req = PyRequest::create_bound(py, scope.into_any().unbind())?;
    Ok(bound_req.into_any().unbind())
}

#[inline(always)]
fn spawn_background_tasks(
    rt_handle: &tokio::runtime::Handle,
    bg_tasks: Option<Py<crate::engine::background::PyBackgroundTasks>>,
) {
    if let Some(tasks) = bg_tasks {
        rt_handle.spawn(async move {
            let handles = Python::attach(|py| match tasks.try_borrow(py) {
                Ok(bg) => bg.execute_all(),
                Err(e) => {
                    tracing::error!("Failed to borrow BackgroundTasks: {}", e);
                    Vec::new()
                }
            });
            for handle in handles {
                let _ = handle.await;
            }
        });
    }
}

#[inline(always)]
fn prepare_kwargs_and_payload<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
) -> Result<
    (
        Bound<'py, PyDict>,
        Option<Py<crate::engine::background::PyBackgroundTasks>>,
    ),
    Response,
> {
    let kwargs = PyDict::new(py);
    let bg_tasks = pydantic::apply_request_data(py, handler, request_input, payload, &kwargs)?;

    Ok((kwargs, bg_tasks))
}

#[inline(always)]
fn resolve_sync_deps<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    request_object: Option<Py<PyAny>>,
    kwargs: &Bound<'py, PyDict>,
) -> Result<(), Response> {
    let dep_results = dependencies::execute_dependencies_sync(
        py,
        &handler.payload.dependencies,
        request_input,
        request_object,
    )
    .map_err(|e| match e {
        DependencyExecutionError::Response(r) => *r,
        DependencyExecutionError::Python(err) => python_error_to_response(py, err),
    })?;

    dep_results.into_iter().try_for_each(|(name, value)| {
        kwargs
            .set_item(pyo3::types::PyString::intern(py, &name), value.bind(py))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    })
}

#[inline(always)]
fn call_handler_and_convert(
    py: Python<'_>,
    handler: &RouteHandler,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> Result<Response, PyErr> {
    let py_func = handler.execution.func.bind(py);
    let result = match kwargs {
        Some(kw) => py_func.call((), Some(kw)),
        None => py_func.call0(),
    };
    match result {
        Ok(res) => Ok(specialized_response_conversion(py, &res, handler)),
        Err(err) => Err(err),
    }
}

pub(crate) fn render_no_request_response(py: Python<'_>, handler: &RouteHandler) -> Response {
    match call_handler_and_convert(py, handler, None) {
        Ok(res) => res,
        Err(err) => python_error_to_response(py, err),
    }
}

pub(crate) fn render_no_request_json_response(py: Python<'_>, handler: &RouteHandler) -> Response {
    let result = match handler.execution.func.bind(py).call0() {
        Ok(result) => result,
        Err(err) => return python_error_to_response(py, err),
    };

    if result.is_none() {
        return handler
            .response
            .default_status
            .unwrap_or(StatusCode::NO_CONTENT)
            .into_response();
    }

    crate::utils::py_json_response_with_status_hint(
        py,
        handler.response.default_status.unwrap_or(StatusCode::OK),
        &result,
        handler.response.serialization_hint,
    )
    .unwrap_or_else(|err| python_error_to_response(py, err))
}

#[inline(always)]
async fn await_python_future<F>(
    handler: Arc<RouteHandler>,
    future_result: Result<F, Response>,
) -> Result<Response, Response>
where
    F: Future<Output = PyResult<Py<PyAny>>> + Send + 'static,
{
    let future = match future_result {
        Ok(f) => f,
        Err(r) => return Err(r),
    };

    let result = future.await;
    Python::attach(|py| match result {
        Ok(res) => Ok(specialized_response_conversion(py, res.bind(py), &handler)),
        Err(err) => Err(python_error_to_response(py, err)),
    })
}

#[inline(always)]
pub(crate) fn schedule_python_coroutine(
    py: Python<'_>,
    async_loop: &Arc<Py<PyAny>>,
    coroutine: Bound<'_, PyAny>,
) -> PyResult<std::pin::Pin<Box<dyn Future<Output = PyResult<Py<PyAny>>> + Send>>> {
    let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
    let future = rsloop::rust_async::into_future_with_locals(&locals, coroutine)?;
    Ok(Box::pin(future))
}
#[inline(always)]
fn into_asyncio_future(
    py: Python<'_>,
    async_loop: &Arc<Py<PyAny>>,
    coroutine: Bound<'_, PyAny>,
) -> Result<std::pin::Pin<Box<dyn Future<Output = PyResult<Py<PyAny>>> + Send>>, Response> {
    schedule_python_coroutine(py, async_loop, coroutine)
        .map_err(|err| python_error_to_response(py, err))
}

async fn core_sync_no_args(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        return Python::attach(|py| match call_handler_and_convert(py, &handler, None) {
            Ok(res) => res,
            Err(err) => python_error_to_response(py, err),
        });
    }

    let _permit = match get_sync_semaphore().acquire().await {
        Ok(permit) => permit,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| match call_handler_and_convert(py, &handler, None) {
                Ok(res) => res,
                Err(err) => python_error_to_response(py, err),
            })
        })
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_async_no_args(
    _rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
) -> Response {
    let handler_clone = handler.clone();
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let coroutine = handler_clone
            .execution
            .func
            .bind(py)
            .call0()
            .map_err(|err| python_error_to_response(py, err))?;
        into_asyncio_future(py, &async_loop, coroutine)
    });
    match await_python_future(handler, future_res).await {
        Ok(res) => res,
        Err(err) => err,
    }
}

async fn core_sync_no_deps(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
        return Python::attach(|py| -> Result<Response, Response> {
            let (kwargs, bg_tasks) =
                prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_ref())?;
            let response = call_handler_and_convert(py, &handler, Some(&kwargs))
                .map_err(|err| python_error_to_response(py, err))?;
            spawn_background_tasks(&rt_handle, bg_tasks);
            Ok(response)
        })
        .unwrap_or_else(|e| e);
    }

    let Ok(_permit) = get_sync_semaphore().acquire().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let rt_handle_clone = rt_handle.clone();
    rt_handle
        .spawn_blocking(move || {
            let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
            Python::attach(|py| -> Result<Response, Response> {
                let (kwargs, bg_tasks) =
                    prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_ref())?;
                let response = call_handler_and_convert(py, &handler, Some(&kwargs))
                    .map_err(|err| python_error_to_response(py, err))?;
                spawn_background_tasks(&rt_handle_clone, bg_tasks);
                Ok(response)
            })
            .unwrap_or_else(|e| e)
        })
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_sync_deps<const NEEDS_REQ: bool>(
    rt_handle: tokio::runtime::Handle,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
        return Python::attach(|py| -> Result<Response, Response> {
            let (kwargs, bg_tasks) =
                prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_ref())?;
            let req_obj = if NEEDS_REQ {
                Some(
                    create_request_object(py, &request_input)
                        .map_err(|e| python_error_to_response(py, e))?,
                )
            } else {
                None
            };
            resolve_sync_deps(py, &handler, &request_input, req_obj, &kwargs)?;
            let response = call_handler_and_convert(py, &handler, Some(&kwargs))
                .map_err(|err| python_error_to_response(py, err))?;
            spawn_background_tasks(&rt_handle, bg_tasks);
            Ok(response)
        })
        .unwrap_or_else(|e| e);
    }

    let Ok(_permit) = get_sync_semaphore().acquire().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let rt_handle_clone = rt_handle.clone();
    rt_handle
        .spawn_blocking(move || {
            let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
            Python::attach(|py| -> Result<Response, Response> {
                let (kwargs, bg_tasks) =
                    prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_ref())?;
                let req_obj = if NEEDS_REQ {
                    Some(
                        create_request_object(py, &request_input)
                            .map_err(|e| python_error_to_response(py, e))?,
                    )
                } else {
                    None
                };
                resolve_sync_deps(py, &handler, &request_input, req_obj, &kwargs)?;
                let response = call_handler_and_convert(py, &handler, Some(&kwargs))
                    .map_err(|err| python_error_to_response(py, err))?;
                spawn_background_tasks(&rt_handle_clone, bg_tasks);
                Ok(response)
            })
            .unwrap_or_else(|e| e)
        })
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_async_no_deps(
    rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let handler_clone = handler.clone();
    let setup_result = rt_handle
        .spawn_blocking(move || {
            let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
            Python::attach(
                |py| -> Result<
                    (
                        Py<PyAny>,
                        Option<Py<crate::engine::background::PyBackgroundTasks>>,
                    ),
                    Response,
                > {
                    let (kwargs, bg_tasks) = prepare_kwargs_and_payload(
                        py,
                        &handler_clone,
                        &request_input,
                        payload.as_ref(),
                    )?;

                    let coroutine = handler_clone
                        .execution
                        .func
                        .bind(py)
                        .call((), Some(&kwargs))
                        .map(Bound::unbind)
                        .map_err(|err| python_error_to_response(py, err))?;

                    Ok((coroutine, bg_tasks))
                },
            )
        })
        .await
        .unwrap_or_else(|_| Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()));

    let (coroutine, bg_tasks) = match setup_result {
        Ok(res) => res,
        Err(resp) => return resp,
    };

    let future_res =
        Python::attach(|py| into_asyncio_future(py, &async_loop, coroutine.into_bound(py)));
    match await_python_future(handler, future_res).await {
        Ok(response) => {
            spawn_background_tasks(&rt_handle, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

async fn core_async_sync_deps<const NEEDS_REQ: bool>(
    rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let handler_clone = handler.clone();
    let setup_result = rt_handle
        .spawn_blocking(move || {
            let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
            Python::attach(
                |py| -> Result<
                    (
                        Py<PyAny>,
                        Option<Py<crate::engine::background::PyBackgroundTasks>>,
                    ),
                    Response,
                > {
                    let (kwargs, bg_tasks) = prepare_kwargs_and_payload(
                        py,
                        &handler_clone,
                        &request_input,
                        payload.as_ref(),
                    )?;

                    let req_obj = if NEEDS_REQ {
                        Some(
                            create_request_object(py, &request_input)
                                .map_err(|e| python_error_to_response(py, e))?,
                        )
                    } else {
                        None
                    };

                    resolve_sync_deps(py, &handler_clone, &request_input, req_obj, &kwargs)?;

                    let coroutine = handler_clone
                        .execution
                        .func
                        .bind(py)
                        .call((), Some(&kwargs))
                        .map(Bound::unbind)
                        .map_err(|err| python_error_to_response(py, err))?;

                    Ok((coroutine, bg_tasks))
                },
            )
        })
        .await
        .unwrap_or_else(|_| Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()));

    let (coroutine, bg_tasks) = match setup_result {
        Ok(res) => res,
        Err(resp) => return resp,
    };

    let future_res =
        Python::attach(|py| into_asyncio_future(py, &async_loop, coroutine.into_bound(py)));

    match await_python_future(handler, future_res).await {
        Ok(response) => {
            spawn_background_tasks(&rt_handle, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

async fn core_async_async_deps<const NEEDS_REQ: bool>(
    rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let handler_clone = handler.clone();
    let prep_parts = request_parts.clone();
    let prep_ranges = param_ranges.clone();
    let prep_payload = payload.clone();

    let prep_res = rt_handle
        .spawn_blocking(move || {
            let request_input = build_request_input_from_parts(&prep_parts, &prep_ranges);
            Python::attach(
                |py| -> Result<
                    (
                        Option<Py<PyAny>>,
                        Py<PyDict>,
                        Option<Py<crate::engine::background::PyBackgroundTasks>>,
                    ),
                    Response,
                > {
                    let (kwargs, bg_tasks) = prepare_kwargs_and_payload(
                        py,
                        &handler_clone,
                        &request_input,
                        prep_payload.as_ref(),
                    )?;

                    let req_obj = if NEEDS_REQ {
                        Some(
                            create_request_object(py, &request_input)
                                .map_err(|e| python_error_to_response(py, e))?,
                        )
                    } else {
                        None
                    };
                    Ok((req_obj, kwargs.unbind(), bg_tasks))
                },
            )
        })
        .await
        .unwrap_or_else(|_| Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()));

    let (request_object, kwargs_unbind, bg_tasks) = match prep_res {
        Ok(res) => res,
        Err(r) => return r,
    };

    let request_input = build_request_input_from_parts(&request_parts, &param_ranges);
    let dependency_results = match dependencies::execute_dependencies(
        rt_handle.clone(),
        &async_loop,
        &handler.payload.dependencies,
        &request_input,
        request_object,
    )
    .await
    {
        Ok(results) => results,
        Err(DependencyExecutionError::Response(r)) => return *r,
        Err(DependencyExecutionError::Python(err)) => {
            return rt_handle
                .spawn_blocking(move || Python::attach(|py| python_error_to_response(py, err)))
                .await
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };

    let handler_ref = handler.clone();
    let setup_result = rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| -> Result<Py<PyAny>, Response> {
                let kwargs = kwargs_unbind.bind(py);

                dependency_results
                    .into_iter()
                    .try_for_each(|(name, value)| {
                        kwargs
                            .set_item(pyo3::types::PyString::intern(py, &name), value.bind(py))
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
                    })?;

                handler_ref
                    .execution
                    .func
                    .bind(py)
                    .call((), Some(kwargs))
                    .map(Bound::unbind)
                    .map_err(|err| python_error_to_response(py, err))
            })
        })
        .await
        .unwrap_or_else(|_| Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()));

    let coroutine = match setup_result {
        Ok(coroutine) => coroutine,
        Err(resp) => return resp,
    };

    let future_res =
        Python::attach(|py| into_asyncio_future(py, &async_loop, coroutine.into_bound(py)));
    match await_python_future(handler, future_res).await {
        Ok(response) => {
            spawn_background_tasks(&rt_handle, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

pub fn assign_execution_mode(handler: &mut RouteHandler) {
    let is_async = handler.execution.is_async;
    let deps_empty = handler.payload.dependencies.is_empty();
    let all_sync = handler.payload.all_deps_sync;
    let needs_req = handler.payload.dependency_needs_request;
    let needs_kwargs = handler.payload.needs_kwargs;

    handler.execution.execution_mode =
        match (needs_kwargs, is_async, deps_empty, all_sync, needs_req) {
            (false, false, _, _, _) => ExecutionMode::SyncNoArgs,
            (false, true, _, _, _) => ExecutionMode::AsyncNoArgs,

            (true, false, true, _, _) => ExecutionMode::SyncNoDeps,
            (true, false, false, _, false) => ExecutionMode::SyncDepsNoReq,
            (true, false, false, _, true) => ExecutionMode::SyncDepsReq,

            (true, true, true, _, _) => ExecutionMode::AsyncNoDeps,
            (true, true, false, true, false) => ExecutionMode::AsyncSyncDepsNoReq,
            (true, true, false, true, true) => ExecutionMode::AsyncSyncDepsReq,
            (true, true, false, false, false) => ExecutionMode::AsyncAsyncDepsNoReq,
            (true, true, false, false, true) => ExecutionMode::AsyncAsyncDepsReq,
        };
}

pub async fn run_py_handler(
    rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    sync_to_threadpool: bool,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(rt_handle, handler, sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => core_async_no_args(rt_handle, async_loop, handler).await,

        ExecutionMode::SyncNoDeps => {
            core_sync_no_deps(
                rt_handle,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::SyncDepsNoReq => {
            core_sync_deps::<false>(
                rt_handle,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::SyncDepsReq => {
            core_sync_deps::<true>(
                rt_handle,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::AsyncNoDeps => {
            core_async_no_deps(
                rt_handle,
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncSyncDepsNoReq => {
            core_async_sync_deps::<false>(
                rt_handle,
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncSyncDepsReq => {
            core_async_sync_deps::<true>(
                rt_handle,
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncAsyncDepsNoReq => {
            core_async_async_deps::<false>(
                rt_handle,
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncAsyncDepsReq => {
            core_async_async_deps::<true>(
                rt_handle,
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
    }
}

#[inline(always)]
pub async fn run_py_handler_no_request(
    rt_handle: tokio::runtime::Handle,
    async_loop: Arc<Py<PyAny>>,
    sync_to_threadpool: bool,
    handler: Arc<RouteHandler>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(rt_handle, handler, sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => core_async_no_args(rt_handle, async_loop, handler).await,
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
