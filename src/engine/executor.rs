use super::handler_errors::{python_error_to_response, python_error_to_response_in};
use super::managed::run_managed_handler;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fastrapi_core::body::BodyPayload;
use fastrapi_core::params::PathParamRange;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_type::ResponseType;
use fastrapi_routing::dependencies::{
    self, DependencyExecutionError, TeardownTasks, execute_teardowns_async, execute_teardowns_sync,
};
use fastrapi_routing::route::{ExecutionMode, RouteHandler};
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::{blocking, py_bridge};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PySet, PyString};
use smallvec::SmallVec;
use std::sync::{Arc, OnceLock};

use fastrapi_http::background::PyBackgroundTasks;
use fastrapi_http::request::create_py_request;
use fastrapi_http::response_convert::convert_response_by_type;
use fastrapi_pyutil::json::py_json_response_with_dump_options;
use fastrapi_runtime::tokio_runtime::spawn;

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

            match convert_response_by_type(py, result, &handler.response, request_headers) {
                Ok(resp) => resp,
                Err(err) => python_error_to_response(py, err),
            }
        }

        _ => match convert_response_by_type(py, result, &handler.response, request_headers) {
            Ok(resp) => resp,
            Err(err) => python_error_to_response(py, err),
        },
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
pub(crate) fn spawn_background_tasks(
    event_loop: EventLoop,
    bg_tasks: Option<Py<PyBackgroundTasks>>,
) {
    if let Some(tasks) = bg_tasks {
        spawn(async move {
            for task in Python::attach(|py| match tasks.try_borrow(py) {
                Ok(bg) => bg.execute_all(event_loop),
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
pub(crate) fn prepare_kwargs_and_payload<'py>(
    py: Python<'py>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
) -> Result<(Bound<'py, PyDict>, Option<Py<PyBackgroundTasks>>), Response> {
    let kwargs = PyDict::new(py);
    let bg_tasks = fastrapi_routing::request_data::apply_request_data(
        py,
        handler,
        request_input,
        payload,
        &kwargs,
    )?;

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
fn call_handler_and_convert(
    py: Python<'_>,
    handler: &RouteHandler,
    kwargs: Option<&Bound<'_, PyDict>>,
    request_headers: Option<&HeaderMap>,
) -> Result<Response, PyErr> {
    let py_func = handler.execution.func.bind(py);
    let result = kwargs.map_or_else(|| py_func.call0(), |kw| py_func.call((), Some(kw)));
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
        return match convert_response_by_type(py, &result, &handler.response, None) {
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

pub(crate) type PyTaskFuture = py_bridge::PyTaskFuture;

#[inline(always)]
async fn await_python_future(
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'_>>,
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
        Err(err) => Err(python_error_to_response_in(py, &err, request_input)),
    })
}

#[inline(always)]
pub(crate) fn schedule_python_coroutine(
    py: Python<'_>,
    event_loop: EventLoop,
    coroutine: Bound<'_, PyAny>,
) -> PyResult<PyTaskFuture> {
    py_bridge::schedule_task(py, event_loop, coroutine)
}
#[inline(always)]
fn into_asyncio_future(
    py: Python<'_>,
    event_loop: EventLoop,
    coroutine: Bound<'_, PyAny>,
) -> Result<PyTaskFuture, Response> {
    schedule_python_coroutine(py, event_loop, coroutine)
        .map_err(|err| python_error_to_response(py, err))
}

async fn core_sync_no_args(
    handler: &'static RouteHandler,
    request_parts: Option<Parts>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        return Python::attach(|py| {
            match call_handler_and_convert(
                py,
                handler,
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
            handler,
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
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Option<Parts>,
    param_ranges: Option<SmallVec<[PathParamRange; 4]>>,
) -> Response {
    let request_input = request_parts
        .as_ref()
        .zip(param_ranges.as_ref())
        .map(|(parts, ranges)| {
            build_request_input_from_parts(parts, ranges, &handler.payload.path_param_names)
        });
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let coroutine = handler
            .execution
            .func
            .bind(py)
            .call0()
            .map_err(|err| python_error_to_response(py, err))?;
        into_asyncio_future(py, event_loop, coroutine)
    });
    match await_python_future(handler, request_input.as_ref(), future_res).await {
        Ok(res) => res,
        Err(err) => err,
    }
}

pub(crate) fn payload_raw_bytes(payload: Option<&BodyPayload>) -> Option<&[u8]> {
    match payload {
        Some(BodyPayload::Json { raw, .. } | BodyPayload::Form { raw, .. }) => Some(raw),
        None => None,
    }
}

#[inline(always)]
fn run_sync_gil_section(
    py: Python<'_>,
    state: &GilSection<'_, '_, '_>,
) -> Result<(Response, Option<Py<PyBackgroundTasks>>), Response> {
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
                .map_err(|e| python_error_to_response_in(py, &e, Some(request_input)))?,
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

#[inline]
fn overrides_active(py: Python<'_>, handler: &RouteHandler) -> bool {
    handler
        .payload
        .dependency_overrides
        .as_ref()
        .is_some_and(|overrides| {
            overrides
                .bind(py)
                .cast::<PyDict>()
                .map_or(true, |overrides| !overrides.is_empty())
        })
}

async fn core_sync<const NEEDS_REQ: bool>(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
    sync_to_threadpool: bool,
) -> Response {
    if !sync_to_threadpool {
        let handled = Python::attach(|py| {
            (!overrides_active(py, handler)).then(|| {
                run_sync_route::<NEEDS_REQ>(
                    py,
                    event_loop,
                    handler,
                    &request_parts,
                    &param_ranges,
                    payload.as_ref(),
                )
            })
        });
        return match handled {
            Some(response) => response,
            None => {
                Box::pin(run_managed_handler(
                    event_loop,
                    handler,
                    request_parts,
                    param_ranges,
                    payload,
                    false,
                ))
                .await
            }
        };
    }

    let handled = blocking::run_python(move |py| {
        if overrides_active(py, handler) {
            return Err((request_parts, param_ranges, payload));
        }
        Ok(run_sync_route::<NEEDS_REQ>(
            py,
            event_loop,
            handler,
            &request_parts,
            &param_ranges,
            payload.as_ref(),
        ))
    })
    .await;
    match handled {
        Ok(Ok(response)) => response,
        Ok(Err((request_parts, param_ranges, payload))) => {
            Box::pin(run_managed_handler(
                event_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
                true,
            ))
            .await
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn run_sync_route<const NEEDS_REQ: bool>(
    py: Python<'_>,
    event_loop: EventLoop,
    handler: &RouteHandler,
    request_parts: &Parts,
    param_ranges: &[PathParamRange],
    payload: Option<&BodyPayload>,
) -> Response {
    let request_input = build_request_input_from_parts(
        request_parts,
        param_ranges,
        &handler.payload.path_param_names,
    );
    let section = GilSection {
        handler,
        request_input: &request_input,
        payload,
        needs_request_object: NEEDS_REQ,
    };
    match run_sync_gil_section(py, &section) {
        Ok((response, bg_tasks)) => {
            spawn_background_tasks(event_loop, bg_tasks);
            response
        }
        Err(response) => response,
    }
}

async fn core_async_no_deps(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let (kwargs, bg_tasks) =
            prepare_kwargs_and_payload(py, handler, &request_input, payload.as_ref())?;

        let coroutine = handler
            .execution
            .func
            .bind(py)
            .call((), Some(&kwargs))
            .map_err(|err| python_error_to_response_in(py, &err, Some(&request_input)))?;

        let future = into_asyncio_future(py, event_loop, coroutine)?;
        Ok((future, bg_tasks))
    });

    let (future_res, bg_tasks) = match future_res {
        Ok((f, bg)) => (f, bg),
        Err(resp) => return resp,
    };

    match await_python_future(handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(event_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

async fn core_async_sync_deps<const NEEDS_REQ: bool>(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let setup = Python::attach(
        |py| -> Option<Result<(PyTaskFuture, Option<Py<PyBackgroundTasks>>), Response>> {
            if overrides_active(py, handler) {
                return None;
            }
            Some(start_async_with_sync_deps::<NEEDS_REQ>(
                py,
                event_loop,
                handler,
                &request_input,
                payload.as_ref(),
            ))
        },
    );
    let Some(setup) = setup else {
        drop(request_input);
        return Box::pin(run_managed_handler(
            event_loop,
            handler,
            request_parts,
            param_ranges,
            payload,
            false,
        ))
        .await;
    };

    let (future_res, bg_tasks) = match setup {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };

    match await_python_future(handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(event_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    }
}

fn start_async_with_sync_deps<const NEEDS_REQ: bool>(
    py: Python<'_>,
    event_loop: EventLoop,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
) -> Result<(PyTaskFuture, Option<Py<PyBackgroundTasks>>), Response> {
    let (kwargs, bg_tasks) = prepare_kwargs_and_payload(py, handler, request_input, payload)?;

    let req_obj = if NEEDS_REQ {
        Some(
            create_py_request(py, request_input, payload_raw_bytes(payload))
                .map_err(|e| python_error_to_response_in(py, &e, Some(request_input)))?,
        )
    } else {
        None
    };

    resolve_sync_deps(py, handler, request_input, req_obj, &kwargs)?;

    let coroutine = handler
        .execution
        .func
        .bind(py)
        .call((), Some(&kwargs))
        .map_err(|err| python_error_to_response_in(py, &err, Some(request_input)))?;

    let future = into_asyncio_future(py, event_loop, coroutine)?;
    Ok((future, bg_tasks))
}

async fn core_async_async_deps<const NEEDS_REQ: bool>(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    let request_input = build_request_input_from_parts(
        &request_parts,
        &param_ranges,
        &handler.payload.path_param_names,
    );
    let prep = Python::attach(
        |py| -> Result<(Option<Py<PyAny>>, Py<PyDict>, Option<Py<PyBackgroundTasks>>), Response> {
            let (kwargs, bg_tasks) =
                prepare_kwargs_and_payload(py, handler, &request_input, payload.as_ref())?;

            let req_obj = if NEEDS_REQ {
                Some(
                    create_py_request(py, &request_input, payload_raw_bytes(payload.as_ref()))
                        .map_err(|e| python_error_to_response_in(py, &e, Some(&request_input)))?,
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
        event_loop,
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
            .map_err(|err| python_error_to_response_in(py, &err, Some(&request_input)))?;

        into_asyncio_future(py, event_loop, coroutine)
    });

    let future_res = match setup {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    let response = match await_python_future(handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(event_loop, bg_tasks);
            response
        }
        Err(err_resp) => err_resp,
    };
    execute_teardowns_async(event_loop, teardowns).await;
    response
}

pub async fn run_py_handler(
    event_loop: EventLoop,
    sync_to_threadpool: bool,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::Managed => {
            run_managed_handler(
                event_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(handler, Some(request_parts), sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => {
            core_async_no_args(event_loop, handler, Some(request_parts), Some(param_ranges)).await
        }

        ExecutionMode::SyncNoDeps | ExecutionMode::SyncDepsNoReq => {
            core_sync::<false>(
                event_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::SyncDepsReq => {
            core_sync::<true>(
                event_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
                sync_to_threadpool,
            )
            .await
        }
        ExecutionMode::AsyncNoDeps => {
            core_async_no_deps(event_loop, handler, request_parts, param_ranges, payload).await
        }
        ExecutionMode::AsyncSyncDepsNoReq => {
            core_async_sync_deps::<false>(event_loop, handler, request_parts, param_ranges, payload)
                .await
        }
        ExecutionMode::AsyncSyncDepsReq => {
            core_async_sync_deps::<true>(event_loop, handler, request_parts, param_ranges, payload)
                .await
        }
        ExecutionMode::AsyncAsyncDepsNoReq => {
            core_async_async_deps::<false>(
                event_loop,
                handler,
                request_parts,
                param_ranges,
                payload,
            )
            .await
        }
        ExecutionMode::AsyncAsyncDepsReq => {
            core_async_async_deps::<true>(event_loop, handler, request_parts, param_ranges, payload)
                .await
        }
    }
}

#[inline(always)]
pub async fn run_py_handler_no_request(
    event_loop: EventLoop,
    sync_to_threadpool: bool,
    handler: &'static RouteHandler,
    request_parts: Option<Parts>,
) -> Response {
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(handler, request_parts, sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => {
            core_async_no_args(event_loop, handler, request_parts, None).await
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
