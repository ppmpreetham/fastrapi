//! The executor for route handlers that need a managed Python context.
//! It injects the dependencies, applies the response metadata and closes the dependency exit stacks.

use super::deferred::{Cleanup, finish_request, with_cleanup};
use super::executor::{build_request_input_from_parts, kwargs_or_pending, request_object};
use super::pending_validation::dependency_failure;
use super::handler_errors::python_error_to_response_in;
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use fastrapi_core::body::BodyPayload;
use fastrapi_core::params::PathParamRange;
use fastrapi_http::request_input::RequestInput;
use fastrapi_pyutil::call::CallArgs;
use fastrapi_pyutil::errors::exc_info;
use fastrapi_routing::dependencies::{self, ExitStacks, RequestPlan};
use fastrapi_routing::route::RouteHandler;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::{blocking, py_bridge};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyList;
use smallvec::SmallVec;

use fastrapi_http::response_convert::{convert_response_by_type, is_plain_value};
use fastrapi_http::responses::PyResponse;
use pyo3::exceptions::{PyRuntimeError, PyValueError};

pub(crate) async fn run_managed_handler(
    event_loop: EventLoop,
    handler: &'static RouteHandler,
    parts: Parts,
    ranges: SmallVec<[PathParamRange; 4]>,
    payload: Option<BodyPayload>,
    sync_to_threadpool: bool,
) -> Response {
    let input = build_request_input_from_parts(
        &parts,
        &ranges,
        &handler.payload.path_param_names,
        event_loop,
    );
    let setup = Python::attach(|py| -> Result<_, Response> {
        let (mut kwargs, pending) = kwargs_or_pending(py, handler, &input, payload.as_ref())?;
        let objects = (|| -> PyResult<_> {
            let plan = dependencies::request_plan(
                py,
                &handler.payload.dependencies,
                handler.payload.dependency_overrides.as_ref(),
                &handler.payload.path_param_names,
                &handler.payload.override_cache,
            )?;
            let (needs_request, needs_response) = match &plan {
                RequestPlan::Static(_) => (
                    handler.payload.dependency_needs_request,
                    handler.payload.dependency_needs_response,
                ),
                RequestPlan::Overridden(plan) => (plan.needs_request, plan.needs_response),
            };
            let response = if handler.payload.response_param.is_some() || needs_response {
                let response = py.get_type::<PyResponse>().call0()?;
                response.setattr(intern!(py, "status_code"), 0u16)?;
                response.setattr(intern!(py, "raw_headers"), PyList::empty(py))?;
                if let Some(name) = &handler.payload.response_param {
                    kwargs.set(name.bind(py), &response)?;
                }
                Some(response.unbind())
            } else {
                None
            };
            let request = if needs_request {
                Some(request_object(
                    py,
                    handler,
                    &kwargs,
                    &input,
                    payload.as_ref(),
                )?)
            } else {
                None
            };
            Ok((kwargs, request, response, plan, pending))
        })();
        objects.map_err(|err| python_error_to_response_in(py, &err, Some(&input)))
    });
    let (mut kwargs, request, metadata, plan, pending) = match setup {
        Ok(value) => value,
        Err(response) => return response,
    };

    let mut stacks = ExitStacks::default();
    let outcome = async {
        let values = dependencies::execute_request_dependencies(
            event_loop,
            plan.nodes(),
            &input,
            request.as_ref(),
            metadata.as_ref(),
            &mut stacks,
            sync_to_threadpool,
        )
        .await;
        let values = match (values, pending) {
            (Ok(_), Some(pending)) => return Err(pending.into_error()),
            (Ok(values), None) => values,
            (Err(err), pending) => return Err(Python::attach(|py| dependency_failure(py, pending, err))),
        };
        if !handler.execution.is_async && sync_to_threadpool {
            return blocking::run_python(move |py| {
                inject_dependencies(py, &mut kwargs, values)?;
                kwargs
                    .call(
                        handler.execution.func.bind(py),
                        handler.execution.call_layout.as_deref(),
                    )
                    .map(Bound::unbind)
            })
            .await
            .map_err(|err| PyRuntimeError::new_err(err.to_string()))?;
        }
        let called = Python::attach(|py| -> PyResult<_> {
            inject_dependencies(py, &mut kwargs, values)?;
            let func = handler.execution.func.bind(py);
            let layout = handler.execution.call_layout.as_deref();
            if handler.execution.is_async {
                let coroutine = kwargs.call(func, layout)?;
                return py_bridge::schedule_task(py, event_loop, coroutine).map(Err);
            }
            let _entered = py_bridge::enter_request_copy(py)?;
            Ok(Ok(kwargs.call(func, layout)?.unbind()))
        })?;
        match called {
            Ok(value) => Ok(value),
            Err(future) => Ok(future.await?),
        }
    }
    .await;
    let outcome = match stacks.function.take() {
        Some(function) => {
            let exit = Python::attach(|py| {
                schedule_stack_exit(py, event_loop, &function, outcome.as_ref().err())
            });
            match exit {
                Ok(exit) => match exit.await {
                    Ok(_) => outcome,
                    Err(err) => Err(err),
                },
                Err(err) => Err(err),
            }
        }
        None => outcome,
    };
    let stack = stacks.request;

    let converted = Python::attach(|py| {
        let converted = outcome.and_then(|value| {
            let value = value.bind(py);
            let explicit = !value.is_none()
                && !is_plain_value(value)
                && value.hasattr(intern!(py, "status_code"))?
                && value.hasattr(intern!(py, "raw_headers"))?;
            let response =
                convert_response_by_type(py, value, &handler.response, Some(input.headers))?;
            Ok((response, explicit))
        });
        match stack {
            Some(stack) if converted.is_ok() => Ok(managed_response(
                py,
                event_loop,
                &input,
                converted,
                metadata.as_ref(),
                Some(Cleanup::Stack(stack, event_loop)),
            )),
            Some(stack) => {
                let exit = schedule_stack_exit(py, event_loop, &stack, converted.as_ref().err());
                Err((converted, exit))
            }
            None => Ok(finish_managed_response(
                py,
                event_loop,
                &input,
                converted,
                None,
                metadata.as_ref(),
            )),
        }
    });
    match converted {
        Ok(response) => response,
        Err((converted, exit)) => {
            let exit = match exit {
                Ok(future) => future.await,
                Err(err) => Err(err),
            };
            Python::attach(|py| {
                finish_managed_response(
                    py,
                    event_loop,
                    &input,
                    converted,
                    Some(exit),
                    metadata.as_ref(),
                )
            })
        }
    }
}

type ManagedOutcome = PyResult<(Response, bool)>;

fn inject_dependencies(
    py: Python<'_>,
    kwargs: &mut CallArgs,
    values: dependencies::ResolvedDependencies,
) -> PyResult<()> {
    values
        .into_iter()
        .try_for_each(|(name, value)| kwargs.set(name.bind(py), value.bind(py)))
}

fn schedule_stack_exit(
    py: Python<'_>,
    event_loop: EventLoop,
    stack: &Py<PyAny>,
    error: Option<&PyErr>,
) -> PyResult<py_bridge::PyTaskFuture> {
    let stack = stack.bind(py);
    let coroutine = match error {
        Some(err) => stack.call_method1(intern!(py, "__aexit__"), exc_info(py, err))?,
        None => stack.call_method1(intern!(py, "__aexit__"), (py.None(), py.None(), py.None()))?,
    };
    py_bridge::schedule_task(py, event_loop, coroutine)
}

fn finish_managed_response(
    py: Python<'_>,
    event_loop: EventLoop,
    input: &RequestInput<'_>,
    outcome: ManagedOutcome,
    exit: Option<PyResult<Py<PyAny>>>,
    metadata: Option<&Py<PyAny>>,
) -> Response {
    match exit {
        Some(Err(err)) => return python_error_to_response_in(py, &err, Some(input)),
        Some(Ok(suppressed))
            if outcome.is_err() && suppressed.bind(py).is_truthy().unwrap_or(false) =>
        {
            return python_error_to_response_in(
                py,
                &PyRuntimeError::new_err(
                    "A dependency suppressed an exception before a response was produced",
                ),
                Some(input),
            );
        }
        _ => {}
    }
    managed_response(py, event_loop, input, outcome, metadata, None)
}

fn managed_response(
    py: Python<'_>,
    event_loop: EventLoop,
    input: &RequestInput<'_>,
    outcome: ManagedOutcome,
    metadata: Option<&Py<PyAny>>,
    cleanup: Option<Cleanup>,
) -> Response {
    let (mut response, explicit) = match outcome {
        Err(err) => {
            return with_cleanup(python_error_to_response_in(py, &err, Some(input)), cleanup);
        }
        Ok(converted) => converted,
    };
    if !explicit
        && let Some(metadata) = metadata
        && let Err(err) = apply_response_metadata(py, metadata, &mut response)
    {
        return with_cleanup(python_error_to_response_in(py, &err, Some(input)), cleanup);
    }
    finish_request(event_loop, input, response, cleanup)
}

fn apply_response_metadata(
    py: Python<'_>,
    metadata: &Py<PyAny>,
    response: &mut Response,
) -> PyResult<()> {
    let metadata = metadata.bind(py);
    if let Ok(status) = metadata
        .getattr(intern!(py, "status_code"))?
        .extract::<u16>()
        && status != 0
    {
        *response.status_mut() =
            StatusCode::from_u16(status).map_err(|err| PyValueError::new_err(err.to_string()))?;
    }
    for item in metadata.getattr(intern!(py, "raw_headers"))?.try_iter()? {
        let (name, value): (Vec<u8>, Vec<u8>) = item?.extract()?;
        let name =
            HeaderName::from_bytes(&name).map_err(|err| PyValueError::new_err(err.to_string()))?;
        let value = HeaderValue::from_bytes(&value)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        response.headers_mut().append(name, value);
    }
    Ok(())
}
