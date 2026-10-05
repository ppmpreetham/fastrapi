use crate::ffi::exceptions::PyHTTPException;
use crate::ffi::pydantic;
use crate::routing::dependencies::{self, DependencyExecutionError};
use crate::routing::types::{BodyPayload, PathParamRange, RequestInput, RouteHandler};
use crate::runtime::py_bridge;
use crate::types::response::ResponseType;
use axum::{
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use smallvec::SmallVec;
use std::sync::{Arc, OnceLock};
use tracing::error;

use super::blocking;
use crate::http::request::create_py_request;
use axum::http::HeaderMap;
use axum::http::HeaderName;
use axum::http::HeaderValue;
use crate::engine::background::PyBackgroundTasks;
use crate::engine::errors::dispatch_exception_handler;
use crate::globals::spawn;
use crate::http::responses::PyResponse;
use crate::http::responses::convert_response_by_type;
use crate::routing::dependencies::TeardownTasks;
use crate::utils::py_json_response_with_dump_options;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::exceptions::PyStopIteration;
use pyo3::exceptions::PyValueError;
use pyo3::types::PyBool;
use pyo3::types::PyFloat;
use pyo3::types::PyInt;
use pyo3::types::PyList;
use pyo3::types::PySet;
use std::mem::take;

crate::cached_py_import!(TRACEBACK_MODULE, "traceback");
crate::cached_py_import!(ASYNC_EXIT_STACK, "contextlib", "AsyncExitStack");

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

// TODO
// pub struct ExecutionStruct{
//   sync: bool,
//   args: bool,
//   deps: bool,
//   requirements: bool,
// }

#[inline(always)]
fn specialized_response_conversion(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    handler: &RouteHandler,
    request_headers: Option<&HeaderMap>,
) -> Response {
    match handler.response.response_type {
        ResponseType::PlainText => {
            if let Ok(s) = result.cast::<PyString>() {
                return s.to_string_lossy().into_owned().into_response();
            }

            match convert_response_by_type(
                py,
                result,
                handler,
                request_headers,
            ) {
                Ok(resp) => resp,
                Err(err) => python_error_to_response(py, err),
            }
        }

        _ => match convert_response_by_type(
            py,
            result,
            handler,
            request_headers,
        ) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        },
    }
}

fn python_error_to_response(py: Python<'_>, err: PyErr) -> Response {
    python_error_to_response_in(py, err, None)
}

/// @`app.exception_handler` registry
pub(crate) fn python_error_to_response_in(
    py: Python<'_>,
    err: PyErr,
    request_input: Option<&RequestInput<'_>>,
) -> Response {
    if let Some(response) =
        dispatch_exception_handler(py, &err, request_input)
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
            .traceback(py).map_or_else(|| py.None(), |tb| tb.into_any().unbind());
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
                let name = names.get(r.name_index).map_or("", |n| &**n);
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
    bg_tasks: Option<Py<PyBackgroundTasks>>,
) {
    if let Some(tasks) = bg_tasks {
        let async_loop = async_loop.clone();
        spawn(async move {
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
        Option<Py<PyBackgroundTasks>>,
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
) -> Result<TeardownTasks, Response> {
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
fn execute_teardowns_sync(
    py: Python<'_>,
    teardowns: TeardownTasks,
    error: Option<&PyErr>,
) {
    for task in teardowns.into_iter().rev() {
        if task.is_async {
            tracing::error!("Async generator dependency found in sync route!");
        } else {
            let result = if let Some(error) = error {
                let traceback = error
                    .traceback(py).map_or_else(|| py.None(), |value| value.into_any().unbind());
                task.generator.bind(py).call_method1(
                    pyo3::intern!(py, "throw"),
                    (error.get_type(py), error.value(py), traceback),
                )
            } else {
                task.generator
                    .bind(py)
                    .call_method0(pyo3::intern!(py, "__next__"))
            };
            match result {
                Ok(_) => tracing::error!("Sync generator dependency yielded twice!"),
                Err(e) => {
                    if !e.is_instance_of::<PyStopIteration>(py) {
                        tracing::error!("Error during sync generator dependency teardown: {}", e);
                    }
                }
            }
        }
    }
}

pub(crate) async fn execute_teardowns_async(
    async_loop: &Arc<Py<PyAny>>,
    teardowns: TeardownTasks,
) {
    let mut sync_batch: Vec<Py<PyAny>> = Vec::new();

    for task in teardowns.into_iter().rev() {
        if task.is_async {
            flush_sync_teardowns(&mut sync_batch).await;
            // Resume past the yield so post-yield cleanup runs; a clean generator
            // ends with StopAsyncIteration (same protocol FastAPI's exit stack uses).
            let resume = Python::attach(|py| -> PyResult<_> {
                let coroutine = task
                    .generator
                    .bind(py)
                    .call_method0(pyo3::intern!(py, "__anext__"))?;
                py_bridge::schedule_task(py, async_loop, coroutine)
            });
            match resume {
                Ok(fut) => match fut.await {
                    Ok(_) => tracing::error!("Async generator dependency yielded twice!"),
                    Err(e) => Python::attach(|py| {
                        if !e.is_instance_of::<PyStopAsyncIteration>(py) {
                            tracing::error!(
                                "Error during async generator dependency teardown: {}",
                                e
                            );
                        }
                    }),
                },
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
    let generators = take(batch);
    _ = blocking::run_python(move |py| {
        for task_gen in generators {
            match task_gen
                .bind(py)
                .call_method0(pyo3::intern!(py, "__next__"))
            {
                Ok(_) => tracing::error!("Sync generator dependency yielded twice!"),
                Err(e) => {
                    if !e.is_instance_of::<PyStopIteration>(py) {
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
    request_headers: Option<&HeaderMap>,
) -> Result<Response, PyErr> {
    let py_func = handler.execution.func.bind(py);
    let result = match kwargs {
        Some(kw) => py_func.call((), Some(kw)),
        None => py_func.call0(),
    };
    match result {
        Ok(res) => Ok(specialized_response_conversion(
            py,
            &res,
            handler,
            request_headers,
        )),
        Err(err) => Err(err),
    }
}

pub(crate) fn render_no_request_response(py: Python<'_>, handler: &RouteHandler) -> Response {
    match call_handler_and_convert(py, handler, None, None) {
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

    let is_plain_value = result.is_instance_of::<PyDict>()
        || result.is_instance_of::<PyList>()
        || result.is_instance_of::<PySet>()
        || result.is_instance_of::<PyString>()
        || result.is_instance_of::<PyBool>()
        || result.is_instance_of::<PyInt>()
        || result.is_instance_of::<PyFloat>();

    if !is_plain_value {
        return match convert_response_by_type(py, &result, handler, None) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        };
    }

    py_json_response_with_dump_options(
        py,
        handler.response.default_status.unwrap_or(StatusCode::OK),
        &result,
        handler.response.serialization_hint,
        handler.response.dump_options.as_ref().map(|d| d.bind(py)),
    )
    .unwrap_or_else(|err| python_error_to_response(py, err))
}

type PyTaskFuture = py_bridge::PyTaskFuture;

#[inline(always)]
async fn await_python_future<'a>(
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'a>>,
    future_result: Result<PyTaskFuture, Response>,
) -> Result<Response, Response> {
    let future = match future_result {
        Ok(f) => f,
        Err(r) => return Err(r),
    };

    let result = future.await;
    Python::attach(|py| match result {
        Ok(res) => Ok(specialized_response_conversion(
            py,
            res.bind(py),
            handler,
            request_input.map(|input| input.headers),
        )),
        Err(err) => Err(python_error_to_response_in(py, err, request_input)),
    })
}

#[inline(always)]
pub(crate) fn schedule_python_coroutine(
    py: Python<'_>,
    async_loop: &Arc<Py<PyAny>>,
    coroutine: Bound<'_, PyAny>,
) -> PyResult<PyTaskFuture> {
    py_bridge::schedule_task(py, async_loop, coroutine)
}
#[inline(always)]
fn into_asyncio_future(
    py: Python<'_>,
    async_loop: &Arc<Py<PyAny>>,
    coroutine: Bound<'_, PyAny>,
) -> Result<PyTaskFuture, Response> {
    schedule_python_coroutine(py, async_loop, coroutine)
        .map_err(|err| python_error_to_response(py, err))
}

async fn core_sync_no_args(
    handler: Arc<RouteHandler>,
    request_parts: Option<Parts>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        return Python::attach(|py| {
            match call_handler_and_convert(
                py,
                &handler,
                None,
                request_parts.as_ref().map(|parts| &parts.headers),
            ) {
                Ok(res) => res,
                Err(err) => python_error_to_response(py, err),
            }
        });
    }

    blocking::run_python(move |py| {
        match call_handler_and_convert(
            py,
            &handler,
            None,
            request_parts.as_ref().map(|parts| &parts.headers),
        ) {
            Ok(res) => res,
            Err(err) => python_error_to_response(py, err),
        }
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn core_async_no_args(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    request_parts: Option<Parts>,
    param_ranges: Option<SmallVec<[PathParamRange; 4]>>,
) -> Response {
    let request_input = request_parts
        .as_ref()
        .zip(param_ranges.as_ref())
        .map(|(parts, ranges)| {
            build_request_input_from_parts(parts, ranges, &handler.payload.path_param_names)
        });
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
    match await_python_future(&handler, request_input.as_ref(), future_res).await {
        Ok(res) => res,
        Err(err) => err,
    }
}

fn payload_raw_bytes(payload: Option<&BodyPayload>) -> Option<&[u8]> {
    match payload {
        Some(BodyPayload::Json { raw, .. }) => Some(raw),
        Some(BodyPayload::Form { raw, .. }) => Some(raw),
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
        Option<Py<PyBackgroundTasks>>,
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

    let response =
        call_handler_and_convert(py, handler, Some(&kwargs), Some(request_input.headers));
    match response {
        Ok(response) => {
            execute_teardowns_sync(py, teardowns, None);
            Ok((response, bg_tasks))
        }
        Err(err) => {
            execute_teardowns_sync(py, teardowns, Some(&err));
            Err(python_error_to_response(py, err))
        }
    }
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
                PyTaskFuture,
                Option<Py<PyBackgroundTasks>>,
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
                Option<Py<PyBackgroundTasks>>,
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
    let needs_kwargs =
        handler.payload.needs_kwargs || handler.payload.response_param.is_some() || !deps_empty;

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
    let has_live_overrides = handler.payload.dependency_overrides.is_some();
    if handler.payload.response_param.is_some()
        || handler.payload.dependency_needs_response
        || has_live_overrides
        || (handler.execution.is_async && handler.payload.has_generators)
        || handler
            .payload
            .dependencies
            .iter()
            .any(|node| node.is_generator || node.is_async_generator || node.is_async)
    {
        return run_managed_handler(
            async_loop,
            handler,
            request_parts,
            param_ranges,
            payload,
            sync_to_threadpool,
        )
        .await;
    }
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(handler, Some(request_parts), sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => {
            core_async_no_args(async_loop, handler, Some(request_parts), Some(param_ranges)).await
        }

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

async fn run_managed_handler(
    async_loop: Arc<Py<PyAny>>,
    handler: Arc<RouteHandler>,
    parts: Parts,
    ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<Arc<BodyPayload>>,
    sync_to_threadpool: bool,
) -> Response {
    let input = build_request_input_from_parts(&parts, &ranges, &handler.payload.path_param_names);
    let setup = Python::attach(|py| -> Result<_, Response> {
        let (kwargs, background) =
            prepare_kwargs_and_payload(py, &handler, &input, payload.as_deref())?;
        let objects = (|| -> PyResult<_> {
            let response = py
                .get_type::<PyResponse>()
                .call0()?;
            response.setattr("status_code", 0u16)?;
            response.setattr("raw_headers", PyList::empty(py))?;
            if let Some(name) = &handler.payload.response_param {
                kwargs.set_item(name.bind(py), &response)?;
            }
            let request = create_py_request(py, &input, payload_raw_bytes(payload.as_deref()))?;
            let stack = ASYNC_EXIT_STACK.get(py)?.call0()?.unbind();
            let plan = dependencies::request_plan(
                py,
                &handler.payload.dependencies,
                handler.payload.dependency_overrides.as_ref(),
                &handler.payload.path_param_names,
            )?;
            Ok((
                kwargs.unbind(),
                background,
                request,
                response.unbind(),
                stack,
                plan,
            ))
        })();
        objects.map_err(|err| python_error_to_response_in(py, err, Some(&input)))
    });
    let (kwargs, background, request, metadata, stack, plan) = match setup {
        Ok(value) => value,
        Err(response) => return response,
    };
    let stack_enter = Python::attach(|py| -> PyResult<_> {
        let coroutine = stack.bind(py).call_method0("__aenter__")?;
        py_bridge::schedule_task(py, &async_loop, coroutine)
    });
    let entered = match stack_enter {
        Ok(future) => future.await,
        Err(err) => Err(err),
    };
    if let Err(err) = entered {
        return Python::attach(|py| python_error_to_response_in(py, err, Some(&input)));
    }
    let outcome = async {
        let values = dependencies::execute_request_dependencies(
            &async_loop,
            &plan,
            &input,
            &request,
            &metadata,
            &stack,
            sync_to_threadpool,
        )
        .await?;
        Python::attach(|py| -> PyResult<()> {
            for (name, value) in values {
                kwargs.bind(py).set_item(name.bind(py), value.bind(py))?;
            }
            Ok(())
        })?;
        let value = if !handler.execution.is_async && sync_to_threadpool {
            let (func, kwargs) =
                Python::attach(|py| (handler.execution.func.clone_ref(py), kwargs.clone_ref(py)));
            blocking::run_python(move |py| {
                func.bind(py)
                    .call((), Some(kwargs.bind(py)))
                    .map(Bound::unbind)
            })
            .await
            .map_err(|err| PyRuntimeError::new_err(err.to_string()))??
        } else {
            let result = Python::attach(|py| -> PyResult<_> {
                let result = handler
                    .execution
                    .func
                    .bind(py)
                    .call((), Some(kwargs.bind(py)))?;
                if handler.execution.is_async {
                    Ok((
                        None,
                        Some(py_bridge::schedule_task(py, &async_loop, result)?),
                    ))
                } else {
                    Ok((Some(result.unbind()), None))
                }
            })?;
            match result {
                (Some(value), _) => value,
                (_, Some(future)) => future.await?,
                _ => unreachable!(),
            }
        };
        Python::attach(|py| -> Result<_, DependencyExecutionError> {
            let value = value.bind(py);
            let explicit = value.hasattr("status_code")? && value.hasattr("raw_headers")?;
            let response = convert_response_by_type(
                py,
                value,
                &handler,
                Some(input.headers),
            )?;
            Ok((response, explicit))
        })
    }
    .await;
    let exit = Python::attach(|py| -> PyResult<_> {
        let args = match &outcome {
            Err(DependencyExecutionError::Python(err)) => (
                err.get_type(py).into_any().unbind(),
                err.value(py).clone().into_any().unbind(),
                err.traceback(py).map_or_else(|| py.None(), |tb| tb.into_any().unbind()),
            ),
            Err(DependencyExecutionError::Response(_)) => {
                let err = PyValueError::new_err("Request validation failed");
                (
                    err.get_type(py).into_any().unbind(),
                    err.value(py).clone().into_any().unbind(),
                    py.None(),
                )
            }
            Ok(_) => (py.None(), py.None(), py.None()),
        };
        let coroutine = stack.bind(py).call_method1("__aexit__", args)?;
        py_bridge::schedule_task(py, &async_loop, coroutine)
    });
    let exit = match exit {
        Ok(future) => future.await,
        Err(err) => Err(err),
    };
    Python::attach(|py| {
        match exit {
            Err(err) => return python_error_to_response_in(py, err, Some(&input)),
            Ok(value) if outcome.is_err() && value.bind(py).is_truthy().unwrap_or(false) => {
                return python_error_to_response_in(
                    py,
                    PyRuntimeError::new_err(
                        "A dependency suppressed an exception before a response was produced",
                    ),
                    Some(&input),
                );
            }
            _ => {}
        }
        match outcome {
            Err(DependencyExecutionError::Python(err)) => {
                python_error_to_response_in(py, err, Some(&input))
            }
            Err(DependencyExecutionError::Response(response)) => *response,
            Ok((mut response, explicit)) => {
                if !explicit {
                    let applied = (|| -> PyResult<()> {
                        let metadata = metadata.bind(py);
                        if let Ok(status) = metadata.getattr("status_code")?.extract::<u16>()
                            && status != 0
                        {
                            *response.status_mut() =
                                StatusCode::from_u16(status).map_err(|err| {
                                    PyValueError::new_err(err.to_string())
                                })?;
                        }
                        for item in metadata.getattr("raw_headers")?.try_iter()? {
                            let (name, value): (Vec<u8>, Vec<u8>) = item?.extract()?;
                            let name =
                                HeaderName::from_bytes(&name).map_err(|err| {
                                    PyValueError::new_err(err.to_string())
                                })?;
                            let value =
                                HeaderValue::from_bytes(&value).map_err(|err| {
                                    PyValueError::new_err(err.to_string())
                                })?;
                            response.headers_mut().append(name, value);
                        }
                        Ok(())
                    })();
                    if let Err(err) = applied {
                        return python_error_to_response_in(py, err, Some(&input));
                    }
                }
                spawn_background_tasks(&async_loop, background);
                response
            }
        }
    })
}

#[inline(always)]
pub async fn run_py_handler_no_request(
    async_loop: Arc<Py<PyAny>>,
    sync_to_threadpool: bool,
    handler: Arc<RouteHandler>,
    request_parts: Option<Parts>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(handler, request_parts, sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => {
            core_async_no_args(async_loop, handler, request_parts, None).await
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
