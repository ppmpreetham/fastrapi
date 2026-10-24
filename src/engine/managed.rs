//! The executor for route handlers that need a managed Python context.
//! It injects the dependencies, applies the response metadata and closes the dependency exit stacks.

use super::executor::{
    build_request_input_from_parts, payload_raw_bytes, prepare_kwargs_and_payload,
    spawn_background_tasks,
};
use super::handler_errors::python_error_to_response_in;
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use fastrapi_core::body::BodyPayload;
use fastrapi_core::params::PathParamRange;
use fastrapi_http::request_input::RequestInput;
use fastrapi_routing::dependencies::{self, DependencyExecutionError};
use fastrapi_routing::route::RouteHandler;
use fastrapi_runtime::py_bridge::EventLoop;
use fastrapi_runtime::{blocking, py_bridge};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use smallvec::SmallVec;

use fastrapi_http::background::PyBackgroundTasks;
use fastrapi_http::request::create_py_request;
use fastrapi_http::response_convert::convert_response_by_type;
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
    let input = build_request_input_from_parts(&parts, &ranges, &handler.payload.path_param_names);
    let setup = Python::attach(|py| -> Result<_, Response> {
        let (kwargs, background) =
            prepare_kwargs_and_payload(py, handler, &input, payload.as_ref())?;
        let objects = (|| -> PyResult<_> {
            let response = py.get_type::<PyResponse>().call0()?;
            response.setattr(intern!(py, "status_code"), 0u16)?;
            response.setattr(intern!(py, "raw_headers"), PyList::empty(py))?;
            if let Some(name) = &handler.payload.response_param {
                kwargs.set_item(name.bind(py), &response)?;
            }
            let request = create_py_request(py, &input, payload_raw_bytes(payload.as_ref()))?;
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
                plan,
            ))
        })();
        objects.map_err(|err| python_error_to_response_in(py, &err, Some(&input)))
    });
    let (kwargs, mut background, request, metadata, plan) = match setup {
        Ok(value) => value,
        Err(response) => return response,
    };

    let mut stack: Option<Py<PyAny>> = None;
    let outcome = async {
        let values = dependencies::execute_request_dependencies(
            event_loop,
            &plan,
            &input,
            &request,
            &metadata,
            &mut stack,
            sync_to_threadpool,
        )
        .await?;
        if !handler.execution.is_async && sync_to_threadpool {
            return blocking::run_python(move |py| {
                let kwargs = kwargs.bind(py);
                inject_dependencies(py, kwargs, values)?;
                handler
                    .execution
                    .func
                    .bind(py)
                    .call((), Some(kwargs))
                    .map(Bound::unbind)
            })
            .await
            .map_err(|err| PyRuntimeError::new_err(err.to_string()))?
            .map_err(DependencyExecutionError::from);
        }
        let called = Python::attach(|py| -> PyResult<_> {
            let kwargs = kwargs.bind(py);
            inject_dependencies(py, kwargs, values)?;
            let result = handler.execution.func.bind(py).call((), Some(kwargs))?;
            if handler.execution.is_async {
                py_bridge::schedule_task(py, event_loop, result).map(Err)
            } else {
                Ok(Ok(result.unbind()))
            }
        })?;
        match called {
            Ok(value) => Ok(value),
            Err(future) => Ok(future.await?),
        }
    }
    .await;

    let converted = Python::attach(|py| {
        let converted = outcome.and_then(|value| {
            let value = value.bind(py);
            let explicit = value.hasattr(intern!(py, "status_code"))?
                && value.hasattr(intern!(py, "raw_headers"))?;
            let response =
                convert_response_by_type(py, value, &handler.response, Some(input.headers))?;
            Ok((response, explicit))
        });
        match stack {
            Some(stack) => {
                let exit = schedule_stack_exit(py, event_loop, &stack, &converted);
                Err((converted, exit))
            }
            None => Ok(finish_managed_response(
                py,
                event_loop,
                &input,
                converted,
                None,
                &metadata,
                background.take(),
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
                    &metadata,
                    background,
                )
            })
        }
    }
}

type ManagedOutcome = Result<(Response, bool), DependencyExecutionError>;

fn inject_dependencies(
    py: Python<'_>,
    kwargs: &Bound<'_, PyDict>,
    values: dependencies::ResolvedDependencies,
) -> PyResult<()> {
    values
        .into_iter()
        .try_for_each(|(name, value)| kwargs.set_item(name.bind(py), value.bind(py)))
}

fn schedule_stack_exit(
    py: Python<'_>,
    event_loop: EventLoop,
    stack: &Py<PyAny>,
    outcome: &ManagedOutcome,
) -> PyResult<py_bridge::PyTaskFuture> {
    let exc_info = |err: &PyErr| {
        (
            err.get_type(py).into_any().unbind(),
            err.value(py).clone().into_any().unbind(),
            err.traceback(py)
                .map_or_else(|| py.None(), |tb| tb.into_any().unbind()),
        )
    };
    let args = match outcome {
        Err(DependencyExecutionError::Python(err)) => exc_info(err),
        Err(DependencyExecutionError::Response(_)) => {
            let (kind, value, _) = exc_info(&PyValueError::new_err("Request validation failed"));
            (kind, value, py.None())
        }
        Ok(_) => (py.None(), py.None(), py.None()),
    };
    let coroutine = stack
        .bind(py)
        .call_method1(intern!(py, "__aexit__"), args)?;
    py_bridge::schedule_task(py, event_loop, coroutine)
}

fn finish_managed_response(
    py: Python<'_>,
    event_loop: EventLoop,
    input: &RequestInput<'_>,
    outcome: ManagedOutcome,
    exit: Option<PyResult<Py<PyAny>>>,
    metadata: &Py<PyAny>,
    background: Option<Py<PyBackgroundTasks>>,
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
    match outcome {
        Err(DependencyExecutionError::Python(err)) => {
            python_error_to_response_in(py, &err, Some(input))
        }
        Err(DependencyExecutionError::Response(response)) => *response,
        Ok((mut response, explicit)) => {
            if !explicit && let Err(err) = apply_response_metadata(py, metadata, &mut response) {
                return python_error_to_response_in(py, &err, Some(input));
            }
            spawn_background_tasks(event_loop, background);
            response
        }
    }
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
