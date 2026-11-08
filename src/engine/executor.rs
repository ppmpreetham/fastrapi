//! The executor that runs Python route handlers for HTTP requests.
//! It prepares the arguments, resolves the dependencies and calls sync or async handlers.
//! It then converts the handler result into an HTTP response.

use super::handler_errors::{
    error_response, python_error_to_response, python_error_to_response_in,
};
use super::managed::run_managed_handler;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use fastrapi_core::body::BodyPayload;
use fastrapi_core::params::PathParamRange;
use fastrapi_http::request_input::RequestInput;
use fastrapi_http::response_type::ResponseType;
use fastrapi_routing::dependencies::{
    self, TeardownTasks, execute_teardowns_async, execute_teardowns_sync,
};
use fastrapi_routing::request_data::Rejection;
use fastrapi_routing::route::{ExecutionMode, RouteHandler};
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::{blocking, py_bridge};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PySet, PyString};
use smallvec::SmallVec;
use std::sync::{Arc, OnceLock};

use fastrapi_http::background::OwnBackground;
use fastrapi_http::request::create_py_request;
use fastrapi_http::response_convert::convert_response_by_type;
use fastrapi_pyutil::call::CallArgs;
use fastrapi_pyutil::json::py_json_response_with_dump_options;
use fastrapi_runtime::thread_state;
use fastrapi_runtime::tokio_runtime::spawn;

#[inline(always)]
fn specialized_response_conversion(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'_>>,
    server: EventLoop,
) -> Response {
    let request_headers = request_input.map(|input| input.headers);
    match handler.response.response_type {
        ResponseType::PlainText => {
            if let Ok(s) = result.cast::<PyString>() {
                return match fastrapi_pyutil::body::str_body(s) {
                    Ok(body) => (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
                        )],
                        body,
                    )
                        .into_response(),
                    Err(_) => s.to_string_lossy().into_owned().into_response(),
                };
            }

            match convert_response_by_type(py, result, &handler.response, request_headers) {
                Ok(resp) => resp,
                Err(err) => error_response(py, &err, request_input, server),
            }
        }

        _ => match convert_response_by_type(py, result, &handler.response, request_headers) {
            Ok(resp) => resp,
            Err(err) => error_response(py, &err, request_input, server),
        },
    }
}

pub(crate) fn build_request_input_from_parts<'a>(
    parts: &'a Parts,
    param_ranges: &[PathParamRange],
    names: &'a [Arc<str>],
    server: EventLoop,
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
        path_params,
        ..RequestInput::new(&parts.method, &parts.uri, &parts.headers, Some(server))
    }
}

#[inline(always)]
pub(crate) fn spawn_background_tasks(
    event_loop: EventLoop,
    request_input: &RequestInput<'_>,
    response: &Response,
) {
    if let Some(tasks) = request_input.background.get()
        && response.extensions().get::<OwnBackground>().is_none()
    {
        let tasks = Python::attach(|py| tasks.clone_ref(py));
        spawn(async move {
            let pending = Python::attach(|py| match tasks.try_borrow(py) {
                Ok(bg) => Some(bg.execute_all(event_loop)),
                Err(e) => {
                    tracing::error!("Failed to borrow BackgroundTasks: {}", e);
                    None
                }
            });
            if let Some(pending) = pending {
                pending.run().await;
            }
        });
    }
}

#[inline(always)]
pub(crate) fn prepare_kwargs_and_payload(
    py: Python<'_>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
) -> Result<CallArgs, Response> {
    let mut kwargs = CallArgs::new();
    fastrapi_routing::request_data::apply_request_data(
        py,
        handler,
        request_input,
        payload,
        &mut kwargs,
    )
    .map_err(|rejection| match rejection {
        Rejection::Python(err) => python_error_to_response_in(py, &err, Some(request_input)),
        Rejection::Response(response) => response,
    })?;

    Ok(kwargs)
}

pub(crate) fn request_object(
    py: Python<'_>,
    handler: &RouteHandler,
    kwargs: &CallArgs,
    request_input: &RequestInput<'_>,
    payload: Option<&BodyPayload>,
) -> PyResult<Py<PyAny>> {
    if let Some(name) = &handler.payload.request_param
        && let Some(existing) = kwargs.get(name.bind(py))
    {
        return Ok(existing.unbind());
    }
    create_py_request(py, request_input, payload_raw_bytes(payload))
}

#[inline(always)]
fn resolve_sync_deps(
    py: Python<'_>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    request_object: Option<Py<PyAny>>,
    kwargs: &mut CallArgs,
) -> Result<TeardownTasks, Response> {
    let (dep_results, teardowns) = dependencies::execute_dependencies_sync(
        py,
        &handler.payload.dependencies,
        request_input,
        request_object,
    )
    .map_err(|err| python_error_to_response_in(py, &err, Some(request_input)))?;

    dep_results.into_iter().try_for_each(|(name, value)| {
        kwargs
            .set(name.bind(py), value.bind(py))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    })?;
    Ok(teardowns)
}

#[inline(always)]
fn call_handler_and_convert(
    py: Python<'_>,
    handler: &RouteHandler,
    kwargs: Option<&CallArgs>,
    request_input: Option<&RequestInput<'_>>,
    server: EventLoop,
) -> Result<Response, PyErr> {
    let py_func = handler.execution.func.bind(py);
    let result = kwargs.map_or_else(
        || py_func.call0(),
        |kw| kw.call(py_func, handler.execution.call_layout.as_deref()),
    );
    match result {
        Ok(res) => Ok(specialized_response_conversion(
            py,
            &res,
            handler,
            request_input,
            server,
        )),
        Err(err) => Err(err),
    }
}

pub(crate) fn render_no_request_response(
    py: Python<'_>,
    handler: &RouteHandler,
    server: EventLoop,
) -> Response {
    match call_handler_and_convert(py, handler, None, None, server) {
        Ok(res) => res,
        Err(err) => python_error_to_response(py, err, server),
    }
}

pub(crate) fn render_no_request_json_response(
    py: Python<'_>,
    handler: &RouteHandler,
    request_input: &RequestInput<'_>,
    server: EventLoop,
) -> Response {
    let fail = |err: PyErr| error_response(py, &err, Some(request_input), server);
    let result = match handler.execution.func.bind(py).call0() {
        Ok(result) => result,
        Err(err) => return fail(err),
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
        return match convert_response_by_type(
            py,
            &result,
            &handler.response,
            Some(request_input.headers),
        ) {
            Ok(resp) => resp,
            Err(err) => fail(err),
        };
    }

    py_json_response_with_dump_options(
        py,
        handler.response.default_status.unwrap_or(StatusCode::OK),
        &result,
        handler.response.serialization_hint,
        handler.response.dump_options.as_ref().map(|d| d.bind(py)),
    )
    .unwrap_or_else(fail)
}

pub(crate) type PyTaskFuture = py_bridge::PyTaskFuture;

pub(crate) enum HandlerRun {
    Done(Result<Response, Response>),
    Pending(PyTaskFuture),
}

enum Step<'py> {
    Return(Bound<'py, PyAny>),
    Raise(PyErr),
    Yield,
}

fn step_coroutine<'py>(py: Python<'py>, coroutine: &Bound<'py, PyAny>) -> Step<'py> {
    use pyo3::ffi;
    unsafe {
        let ctx = ffi::PyContext_New();
        if ctx.is_null() {
            return Step::Raise(PyErr::fetch(py));
        }
        let ctx = Bound::from_owned_ptr(py, ctx);
        if ffi::PyContext_Enter(ctx.as_ptr()) != 0 {
            return Step::Raise(PyErr::fetch(py));
        }
        let mut out: *mut ffi::PyObject = std::ptr::null_mut();
        let status = ffi::PyIter_Send(coroutine.as_ptr(), ffi::Py_None(), &raw mut out);
        let step = match status {
            ffi::PySendResult::PYGEN_RETURN => Step::Return(Bound::from_owned_ptr(py, out)),
            ffi::PySendResult::PYGEN_ERROR => Step::Raise(PyErr::fetch(py)),
            ffi::PySendResult::PYGEN_NEXT => {
                ffi::Py_DECREF(out);
                Step::Yield
            }
        };
        ffi::PyContext_Exit(ctx.as_ptr());
        step
    }
}

fn start_coroutine(
    py: Python<'_>,
    event_loop: EventLoop,
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'_>>,
    coroutine: Bound<'_, PyAny>,
) -> Result<HandlerRun, Response> {
    if !handler.execution.inline_async {
        return into_asyncio_future(py, event_loop, coroutine).map(HandlerRun::Pending);
    }
    Ok(HandlerRun::Done(match step_coroutine(py, &coroutine) {
        Step::Return(result) => Ok(specialized_response_conversion(
            py,
            &result,
            handler,
            request_input,
            event_loop,
        )),
        Step::Raise(err) => Err(python_error_to_response_in(py, &err, request_input)),
        Step::Yield => {
            _ = coroutine.call_method0(pyo3::intern!(py, "close"));
            let err = pyo3::exceptions::PyRuntimeError::new_err(
                "inline handler suspended unexpectedly",
            );
            Err(python_error_to_response_in(py, &err, request_input))
        }
    }))
}

#[inline(always)]
async fn await_python_future(
    event_loop: EventLoop,
    handler: &RouteHandler,
    request_input: Option<&RequestInput<'_>>,
    future_result: Result<HandlerRun, Response>,
) -> Result<Response, Response> {
    let future = match future_result {
        Ok(HandlerRun::Pending(f)) => f,
        Ok(HandlerRun::Done(r)) => return r,
        Err(r) => return Err(r),
    };

    let result = future.await;
    Python::attach(|py| match result {
        Ok(res) => Ok(specialized_response_conversion(
            py,
            res.bind(py),
            handler,
            request_input,
            event_loop,
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
        .map_err(|err| python_error_to_response(py, err, event_loop))
}

async fn core_sync_no_args(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    request_parts: Option<Parts>,
    sync_to_threadpool: bool,
) -> Response {
    let run = move |py: Python<'_>| {
        let input = request_parts.as_ref().map(|parts| {
            build_request_input_from_parts(
                parts,
                &[],
                &handler.payload.path_param_names,
                event_loop,
            )
        });
        call_handler_and_convert(py, handler, None, input.as_ref(), event_loop)
            .unwrap_or_else(|err| error_response(py, &err, input.as_ref(), event_loop))
    };
    if !sync_to_threadpool {
        return Python::attach(run);
    }

    blocking::run_python(run)
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
            build_request_input_from_parts(
                parts,
                ranges,
                &handler.payload.path_param_names,
                event_loop,
            )
        });
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let coroutine = handler
            .execution
            .func
            .bind(py)
            .call0()
            .map_err(|err| error_response(py, &err, request_input.as_ref(), event_loop))?;
        start_coroutine(py, event_loop, handler, request_input.as_ref(), coroutine)
    });
    match await_python_future(event_loop, handler, request_input.as_ref(), future_res).await {
        Ok(res) => res,
        Err(err) => err,
    }
}

pub(crate) fn payload_raw_bytes(payload: Option<&BodyPayload>) -> Option<bytes::Bytes> {
    payload.map(|payload| payload.raw().clone())
}

#[inline(always)]
fn run_sync_gil_section(
    py: Python<'_>,
    state: &GilSection<'_, '_, '_>,
) -> Result<Response, Response> {
    let GilSection {
        handler,
        request_input,
        payload,
        needs_request_object,
        server,
    } = *state;

    let mut kwargs = prepare_kwargs_and_payload(py, handler, request_input, payload)?;

    let req_obj = if needs_request_object {
        Some(
            request_object(py, handler, &kwargs, request_input, payload)
                .map_err(|e| python_error_to_response_in(py, &e, Some(request_input)))?,
        )
    } else {
        None
    };

    let teardowns = resolve_sync_deps(py, handler, request_input, req_obj, &mut kwargs)?;

    let response =
        call_handler_and_convert(py, handler, Some(&kwargs), Some(request_input), server);
    match response {
        Ok(response) => {
            execute_teardowns_sync(py, teardowns, None);
            Ok(response)
        }
        Err(err) => {
            execute_teardowns_sync(py, teardowns, Some(&err));
            Err(python_error_to_response_in(py, &err, Some(request_input)))
        }
    }
}

struct GilSection<'a, 'b, 'c> {
    handler: &'a RouteHandler,
    request_input: &'b RequestInput<'c>,
    payload: Option<&'a BodyPayload>,
    needs_request_object: bool,
    server: EventLoop,
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
        event_loop,
    );
    let section = GilSection {
        handler,
        request_input: &request_input,
        payload,
        needs_request_object: NEEDS_REQ,
        server: event_loop,
    };
    match run_sync_gil_section(py, &section) {
        Ok(response) => {
            spawn_background_tasks(event_loop, &request_input, &response);
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
        event_loop,
    );
    let future_res = Python::attach(|py| -> Result<_, Response> {
        let kwargs = prepare_kwargs_and_payload(py, handler, &request_input, payload.as_ref())?;

        let coroutine = kwargs
            .call(
                handler.execution.func.bind(py),
                handler.execution.call_layout.as_deref(),
            )
            .map_err(|err| python_error_to_response_in(py, &err, Some(&request_input)))?;

        start_coroutine(py, event_loop, handler, Some(&request_input), coroutine)
    });

    let future_res = match future_res {
        Ok(future) => future,
        Err(resp) => return resp,
    };

    match await_python_future(event_loop, handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(event_loop, &request_input, &response);
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
        event_loop,
    );
    let setup = Python::attach(|py| -> Option<Result<HandlerRun, Response>> {
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
    });
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

    let future_res = match setup {
        Ok(future) => future,
        Err(resp) => return resp,
    };

    match await_python_future(event_loop, handler, Some(&request_input), Ok(future_res)).await {
        Ok(response) => {
            spawn_background_tasks(event_loop, &request_input, &response);
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
) -> Result<HandlerRun, Response> {
    let mut kwargs = prepare_kwargs_and_payload(py, handler, request_input, payload)?;

    let req_obj = if NEEDS_REQ {
        Some(
            request_object(py, handler, &kwargs, request_input, payload)
                .map_err(|e| python_error_to_response_in(py, &e, Some(request_input)))?,
        )
    } else {
        None
    };

    resolve_sync_deps(py, handler, request_input, req_obj, &mut kwargs)?;

    let coroutine = kwargs
        .call(
            handler.execution.func.bind(py),
            handler.execution.call_layout.as_deref(),
        )
        .map_err(|err| python_error_to_response_in(py, &err, Some(request_input)))?;

    start_coroutine(py, event_loop, handler, Some(request_input), coroutine)
}

type AsyncDepsPrep = (Option<Py<PyAny>>, CallArgs);

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
        event_loop,
    );
    let prep = Python::attach(|py| -> Option<Result<AsyncDepsPrep, Response>> {
        if overrides_active(py, handler) {
            return None;
        }
        Some((|| {
            let kwargs = prepare_kwargs_and_payload(py, handler, &request_input, payload.as_ref())?;
            let req_obj = if NEEDS_REQ {
                Some(
                    request_object(py, handler, &kwargs, &request_input, payload.as_ref())
                        .map_err(|e| python_error_to_response_in(py, &e, Some(&request_input)))?,
                )
            } else {
                None
            };
            Ok((req_obj, kwargs))
        })())
    });

    let (request_object, mut kwargs) = match prep {
        Some(Ok(res)) => res,
        Some(Err(r)) => return r,
        None => {
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
        }
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
        Err(err) => {
            return Python::attach(|py| {
                python_error_to_response_in(py, &err, Some(&request_input))
            });
        }
    };

    let setup = Python::attach(|py| -> Result<_, Response> {
        dependency_results
            .into_iter()
            .try_for_each(|(name, value)| {
                kwargs
                    .set(name.bind(py), value.bind(py))
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
            })?;

        let coroutine = kwargs
            .call(
                handler.execution.func.bind(py),
                handler.execution.call_layout.as_deref(),
            )
            .map_err(|err| python_error_to_response_in(py, &err, Some(&request_input)))?;

        start_coroutine(py, event_loop, handler, Some(&request_input), coroutine)
    });

    let future_res = match setup {
        Ok(f) => f,
        Err(resp) => return resp,
    };

    let response = match await_python_future(
        event_loop,
        handler,
        Some(&request_input),
        Ok(future_res),
    )
    .await
    {
        Ok(response) => {
            spawn_background_tasks(event_loop, &request_input, &response);
            response
        }
        Err(err_resp) => err_resp,
    };
    execute_teardowns_async(event_loop, teardowns).await;
    response
}

#[inline]
fn hold_thread_state(handler: &RouteHandler, sync_to_threadpool: bool) {
    let offloaded = sync_to_threadpool
        && matches!(
            handler.execution.execution_mode,
            ExecutionMode::SyncNoArgs
                | ExecutionMode::SyncNoDeps
                | ExecutionMode::SyncDepsNoReq
                | ExecutionMode::SyncDepsReq
        );
    if !offloaded {
        thread_state::hold();
    }
}

pub async fn run_py_handler(
    event_loop: EventLoop,
    sync_to_threadpool: bool,
    handler: &'static RouteHandler,
    request_parts: Parts,
    param_ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
) -> Response {
    hold_thread_state(handler, sync_to_threadpool);
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
            core_sync_no_args(event_loop, handler, Some(request_parts), sync_to_threadpool).await
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
    hold_thread_state(handler, sync_to_threadpool);
    match handler.execution.execution_mode {
        ExecutionMode::SyncNoArgs => {
            core_sync_no_args(event_loop, handler, request_parts, sync_to_threadpool).await
        }
        ExecutionMode::AsyncNoArgs => {
            core_async_no_args(event_loop, handler, request_parts, None).await
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
