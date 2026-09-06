use crate::ffi::exceptions::PyHTTPException;
use crate::ffi::pydantic;
use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::{BodyPayload, PathParamRange, RequestInput, RouteHandler};
use crate::types::response::ResponseType;
use axum::{
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use smallvec::SmallVec;
use std::future::Future;
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::error;

use super::blocking;
use crate::http::request::create_py_request;

crate::cached_py_import!(TRACEBACK_MODULE, "traceback");

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

            match crate::http::responses::convert_response_by_type(py, result, handler) {
                Ok(resp) => resp,
                Err(err) => python_error_to_response(py, err),
            }
        }

        _ => match crate::http::responses::convert_response_by_type(py, result, handler) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        },
    }
}

fn python_error_to_response(py: Python<'_>, err: PyErr) -> Response {
    python_error_to_response_in(py, err, None)
}

/// @app.exception_handler registry
pub(crate) fn python_error_to_response_in(
    py: Python<'_>,
    err: PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    if let Some(response) =
        crate::engine::errors::dispatch_exception_handler(py, &err, request_input)
    {
        return response;
    }

    if let Ok(http_error) = err.value(py).cast::<PyHTTPException>() {
        return http_error.borrow().to_response(py);
    }
    log_python_error(py, &err);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn log_python_error(py: Python<'_>, err: &PyErr) {
    let rendered = (|| -> PyResult<String> {
        let traceback_bound = TRACEBACK_MODULE.get(py)?;

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

pub(crate) fn build_request_input_from_parts<'a>(
    parts: &'a Parts,
    param_ranges: &[PathParamRange],
    names: &'a [Arc<str>],
) -> RequestInput<'a> {
    let path_str = parts.uri.path();
    let path_params = OnceLock::new();

    if !param_ranges.is_empty() {
        let path_params_vec: SmallVec<[(&'a str, &'a str); 8]> = param_ranges
            .iter()
            .map(|r| {
                let name = names.get(r.name_index).map(|n| &**n).unwrap_or("");
                (name, &path_str[r.start..r.end])
            })
            .collect();
        _ = path_params.set(path_params_vec);
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

#[inline(always)]
fn spawn_background_tasks(
    async_loop: &Arc<Py<PyAny>>,
    bg_tasks: Option<Py<crate::engine::background::PyBackgroundTasks>>,
) {
    if let Some(tasks) = bg_tasks {
        let async_loop = async_loop.clone();
        crate::globals::spawn(async move {
            for task in Python::attach(|py| match tasks.try_borrow(py) {
                Ok(bg) => bg.execute_all(&async_loop),
                Err(e) => {
                    tracing::error!("Failed to borrow BackgroundTasks: {}", e);
                    Vec::new()
                }
            }) {
                task.await;
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
) -> Result<crate::routing::dependencies::TeardownTasks, Response> {
    let (dep_results, teardowns) = dependencies::execute_dependencies_sync(
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
            .set_item(name, value.bind(py))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    })?;
    Ok(teardowns)
}

#[inline(always)]
fn execute_teardowns_sync(py: Python<'_>, teardowns: crate::routing::dependencies::TeardownTasks) {
    for task in teardowns.into_iter().rev() {
        if task.is_async {
            tracing::error!("Async generator dependency found in sync route!");
        } else {
            match task
                .generator
                .bind(py)
                .call_method0(pyo3::intern!(py, "__next__"))
            {
                Ok(_) => tracing::error!("Sync generator dependency yielded twice!"),
                Err(e) => {
                    if !e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
                        tracing::error!("Error during sync generator dependency teardown: {}", e);
                    }
                }
            }
        }
    }
}

pub(crate) async fn execute_teardowns_async(
    async_loop: &Arc<Py<PyAny>>,
    teardowns: crate::routing::dependencies::TeardownTasks,
) {
    let mut sync_batch: Vec<Py<PyAny>> = Vec::new();

    for task in teardowns.into_iter().rev() {
        if task.is_async {
            flush_sync_teardowns(&mut sync_batch).await;
            let future_anext = Python::attach(|py| -> PyResult<_> {
                let anext_coroutine = task
                    .generator
                    .bind(py)
                    .call_method0(pyo3::intern!(py, "__anext__"))?;
                let locals = rsloop::rust_async::TaskLocals::new(async_loop.bind(py).clone());
                rsloop::rust_async::into_future_with_locals(&locals, anext_coroutine)
            });
            match future_anext {
                Ok(fut) => {
                    _ = fut.await;
                }
                Err(e) => {
                    tracing::error!("Failed to tear down async generator dependency: {}", e);
                }
            }
        } else {
            sync_batch.push(task.generator);
        }
    }
    flush_sync_teardowns(&mut sync_batch).await;
}

async fn flush_sync_teardowns(batch: &mut Vec<Py<PyAny>>) {
    if batch.is_empty() {
        return;
    }
    let generators = std::mem::take(batch);
    _ = blocking::run_python(move |py| {
        for task_gen in generators {
            match task_gen
                .bind(py)
                .call_method0(pyo3::intern!(py, "__next__"))
            {
                Ok(_) => tracing::error!("Sync generator dependency yielded twice!"),
                Err(e) => {
                    if !e.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) {
                        tracing::error!("Error during sync generator dependency teardown: {}", e);
                    }
                }
            }
        }
    })
    .await;
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

    let is_plain_value = result.is_instance_of::<pyo3::types::PyDict>()
        || result.is_instance_of::<pyo3::types::PyList>()
        || result.is_instance_of::<pyo3::types::PySet>()
        || result.is_instance_of::<PyString>()
        || result.is_instance_of::<pyo3::types::PyBool>()
        || result.is_instance_of::<pyo3::types::PyInt>()
        || result.is_instance_of::<pyo3::types::PyFloat>();

    if !is_plain_value {
        return match crate::http::responses::convert_response_by_type(py, &result, handler) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        };
    }

    crate::utils::py_json_response_with_dump_options(
        py,
        handler.response.default_status.unwrap_or(StatusCode::OK),
        &result,
        handler.response.serialization_hint,
        handler.response.dump_options.as_ref().map(|d| d.bind(py)),
    )
    .unwrap_or_else(|err| python_error_to_response(py, err))
}

#[inline(always)]
async fn await_python_future<'a, F>(
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'a>>,
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
        Ok(res) => Ok(specialized_response_conversion(py, res.bind(py), handler)),
        Err(err) => Err(python_error_to_response_in(py, err, request_input)),
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

async fn core_sync_no_args(handler: Arc<RouteHandler>, sync_to_threadpool: bool) -> Response {
    if !sync_to_threadpool {
        return Python::attach(|py| match call_handler_and_convert(py, &handler, None) {
            Ok(res) => res,
            Err(err) => python_error_to_response(py, err),
        });
    }

    blocking::run_python(
        move |py| match call_handler_and_convert(py, &handler, None) {
            Ok(res) => res,
            Err(err) => python_error_to_response(py, err),
        },
    )
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_async_no_args(async_loop: Arc<Py<PyAny>>, handler: Arc<RouteHandler>) -> Response {
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
    match await_python_future(&handler, None, future_res).await {
        Ok(res) => res,
        Err(err) => err,
    }
}

fn payload_raw_bytes(payload: Option<&BodyPayload>) -> Option<&[u8]> {
    match payload {
        Some(BodyPayload::Json { raw, .. }) => Some(raw),
        _ => None,
    }
}

#[inline(always)]
fn run_sync_gil_section(
    py: Python<'_>,
    state: &GilSection<'_, '_, '_>,
) -> Result<
    (
        Response,
        Option<Py<crate::engine::background::PyBackgroundTasks>>,
    ),
    Response,
> {
    let GilSection {
        handler,
        request_input,
        payload,
        needs_request_object,
    } = *state;

    let (kwargs, bg_tasks) = prepare_kwargs_and_payload(py, handler, request_input, payload)?;

    let req_obj = if needs_request_object {
        Some(
            create_py_request(py, request_input, payload_raw_bytes(payload))
                .map_err(|e| python_error_to_response_in(py, e, Some(request_input)))?,
        )
    } else {
        None
    };

    let teardowns = resolve_sync_deps(py, handler, request_input, req_obj, &kwargs)?;

    let response = call_handler_and_convert(py, handler, Some(&kwargs))
        .map_err(|err| python_error_to_response(py, err))?;

    execute_teardowns_sync(py, teardowns);
    Ok((response, bg_tasks))
}

struct GilSection<'a, 'b, 'c> {
    handler: &'a RouteHandler,
    request_input: &'b RequestInput<'c>,
    payload: Option<&'a BodyPayload>,
    needs_request_object: bool,
}

async fn core_sync_no_deps(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        let request_input = build_request_input_from_parts(
            &request_parts,
            &param_ranges,
            &handler.payload.path_param_names,
        );
        return Python::attach(|py| -> Result<Response, Response> {
            let (response, bg_tasks) = run_sync_gil_section(
                py,
                &GilSection {
                    handler: &handler,
                    request_input: &request_input,
                    payload: payload.as_deref(),
                    needs_request_object: false,
                },
            )?;
            spawn_background_tasks(&async_loop, bg_tasks);
            Ok(response)
        })
        .unwrap_or_else(|e| e);
    }

    blocking::run_python(move |py| {
        let request_input = build_request_input_from_parts(
            &request_parts,
            &param_ranges,
            &handler.payload.path_param_names,
        );
        let result = run_sync_gil_section(
            py,
            &GilSection {
                handler: &handler,
                request_input: &request_input,
                payload: payload.as_deref(),
                needs_request_object: false,
            },
        );
        match result {
            Ok((response, bg_tasks)) => {
                spawn_background_tasks(&async_loop, bg_tasks);
                response
            }
            Err(e) => e,
        }
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_sync_deps<const NEEDS_REQ: bool>(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        let request_input = build_request_input_from_parts(
            &request_parts,
            &param_ranges,
            &handler.payload.path_param_names,
        );
        return Python::attach(|py| -> Result<Response, Response> {
            let (response, bg_tasks) = run_sync_gil_section(
                py,
                &GilSection {
                    handler: &handler,
                    request_input: &request_input,
                    payload: payload.as_deref(),
                    needs_request_object: NEEDS_REQ,
                },
            )?;
            spawn_background_tasks(&async_loop, bg_tasks);
            Ok(response)
        })
        .unwrap_or_else(|e| e);
    }

    blocking::run_python(move |py| {
        let request_input = build_request_input_from_parts(
            &request_parts,
            &param_ranges,
            &handler.payload.path_param_names,
        );
        let result = run_sync_gil_section(
            py,
            &GilSection {
                handler: &handler,
                request_input: &request_input,
                payload: payload.as_deref(),
                needs_request_object: NEEDS_REQ,
            },
        );
        match result {
            Ok((response, bg_tasks)) => {
                spawn_background_tasks(&async_loop, bg_tasks);
                response
            }
            Err(e) => e,
        }
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_async_no_deps(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let (kwargs, bg_tasks) =
            prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_deref())?;

        let coroutine = handler
            .execution
            .func
            .bind(py)
            .call((), Some(&kwargs))
            .map_err(|err| python_error_to_response_in(py, err, Some(&request_input)))?;

        let future = into_asyncio_future(py, &async_loop, coroutine)?;
        Ok((future, bg_tasks))
    });

    let (future_res, bg_tasks) = match future_res {
        Ok((f, bg)) => (f, bg),
        Err(resp) => return resp,
    };

    match await_python_future(&handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(&async_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

async fn core_async_sync_deps<const NEEDS_REQ: bool>(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let setup = Python::attach(
        |py| -> Result<
            (
                std::pin::Pin<Box<dyn Future<Output = PyResult<Py<PyAny>>> + Send>>,
                Option<Py<crate::engine::background::PyBackgroundTasks>>,
            ),
            Response,
        > {
            let (kwargs, bg_tasks) =
                prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_deref())?;

            let req_obj = if NEEDS_REQ {
                Some(
                    create_py_request(py, &request_input, payload_raw_bytes(payload.as_deref()))
                        .map_err(|e| python_error_to_response_in(py, e, Some(&request_input)))?,
                )
            } else {
                None
            };

            resolve_sync_deps(py, &handler, &request_input, req_obj, &kwargs)?;

            let coroutine = handler
                .execution
                .func
                .bind(py)
                .call((), Some(&kwargs))
                .map_err(|err| python_error_to_response_in(py, err, Some(&request_input)))?;

            let future = into_asyncio_future(py, &async_loop, coroutine)?;
            Ok((future, bg_tasks))
        },
    );

    let (future_res, bg_tasks) = match setup {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };

    match await_python_future(&handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(&async_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

async fn core_async_async_deps<const NEEDS_REQ: bool>(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let prep = Python::attach(
        |py| -> Result<
            (
                Option<Py<PyAny>>,
                Py<PyDict>,
                Option<Py<crate::engine::background::PyBackgroundTasks>>,
            ),
            Response,
        > {
            let (kwargs, bg_tasks) =
                prepare_kwargs_and_payload(py, &handler, &request_input, payload.as_deref())?;

            let req_obj = if NEEDS_REQ {
                Some(
                    create_py_request(py, &request_input, payload_raw_bytes(payload.as_deref()))
                        .map_err(|e| python_error_to_response_in(py, e, Some(&request_input)))?,
                )
            } else {
                None
            };
            Ok((req_obj, kwargs.unbind(), bg_tasks))
        },
    );

    let (request_object, kwargs_unbind, bg_tasks) = match prep {
        Ok(res) => res,
        Err(r) => return r,
    };

    let (dependency_results, teardowns) = match dependencies::execute_dependencies(
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
            return blocking::run_python(move |py| python_error_to_response(py, err))
                .await
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };

    let setup = Python::attach(|py| -> Result<_, Response> {
        let kwargs = kwargs_unbind.bind(py);

        dependency_results
            .into_iter()
            .try_for_each(|(name, value)| {
                kwargs
                    .set_item(name, value.bind(py))
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
            })?;

        let coroutine = handler
            .execution
            .func
            .bind(py)
            .call((), Some(kwargs))
            .map_err(|err| python_error_to_response_in(py, err, Some(&request_input)))?;

        into_asyncio_future(py, &async_loop, coroutine)
    });

    let future_res = match setup {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    let response = match await_python_future(&handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(&async_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    };
    execute_teardowns_async(&async_loop, teardowns).await;
    response
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
    async_loop: Arc<Py<PyAny>>,
    sync_to_threadpool: bool,
    handler: Arc<RouteHandler>,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => core_sync_no_args(handler, sync_to_threadpool).await,
        ExecutionMode::AsyncNoArgs => core_async_no_args(async_loop, handler).await,

        ExecutionMode::SyncNoDeps => {
            core_sync_no_deps(
                async_loop,
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
                async_loop,
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
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::AsyncNoDeps => {
            core_async_no_deps(async_loop, handler, request_parts, param_ranges, payload).await
        }
        ExecutionMode::AsyncSyncDepsNoReq => {
            core_async_sync_deps::<false>(async_loop, handler, request_parts, param_ranges, payload)
                .await
        }
        ExecutionMode::AsyncSyncDepsReq => {
            core_async_sync_deps::<true>(async_loop, handler, request_parts, param_ranges, payload)
                .await
        }
        ExecutionMode::AsyncAsyncDepsNoReq => {
            core_async_async_deps::<false>(
                async_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncAsyncDepsReq => {
            core_async_async_deps::<true>(async_loop, handler, request_parts, param_ranges, payload)
                .await
        }
    }
}

#[inline(always)]
pub async fn run_py_handler_no_request(
    async_loop: Arc<Py<PyAny>>,
    sync_to_threadpool: bool,
    handler: Arc<RouteHandler>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => core_sync_no_args(handler, sync_to_threadpool).await,
        ExecutionMode::AsyncNoArgs => core_async_no_args(async_loop, handler).await,
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
