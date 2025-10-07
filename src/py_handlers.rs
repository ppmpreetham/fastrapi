use pyo3::{exceptions::PyTypeError, prelude::*, types::{PyDict, PyType}};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use crate::{utils::{json_to_py_object, py_to_response}, ROUTES};
use crate::pydantic::validate_with_pydantic;

/// for routes WITH payload (POST, PUT, PATCH, DELETE)
pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: String,
    payload: serde_json::Value,
) -> Response {
    match rt_handle.spawn_blocking(move || {
        Python::attach(|py| {
            if let Some(entry) = ROUTES.get(&route_key) {
                let py_func = entry.value().bind(py);

                let py_payload_result = match py_func.getattr("__annotations__") {
                    Ok(annotations) => {
                        if let Ok(annot_dict) = annotations.downcast::<PyDict>() {
                            if let Some(item) = annot_dict.items().into_iter().next() {
                                let (_, type_hint) = item.extract::<(Py<PyAny>, Py<PyAny>)>().unwrap();
                                let type_hint_bound = type_hint.into_bound(py);
                                if type_hint_bound.is_instance_of::<PyType>() {
                                    validate_with_pydantic(py, &type_hint_bound, &payload)
                                } else {
                                    Ok(json_to_py_object(py, &payload))
                                }
                            } else {
                                Ok(json_to_py_object(py, &payload))
                            }
                        } else {
                            Ok(json_to_py_object(py, &payload))
                        }
                    }
                    Err(_) => Ok(json_to_py_object(py, &payload)),
                };

                match py_payload_result {
                    Ok(py_payload) => match py_func.call1((py_payload,)) {
                        Ok(result) => Ok(py_to_response(py, &result)),
                        Err(err) => {
                            let err_str = Python::attach(|py| {
                                if err.is_instance_of::<PyTypeError>(py) {
                                    format!("TypeError in route handler: {}", err)
                                } else {
                                    format!("Error in route handler: {}", err)
                                }
                            });
                            
                            err.print(py);
                            Ok((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                err_str,
                            ).into_response())
                        }
                    },
                    Err(response) => Ok(response),
                }
            } else {
                eprintln!("Route handler not found for {}", route_key);
                Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "Route handler not found"
                ))
            }
        })
    }).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            Python::attach(|py| {
                err.print(py);
                if err.is_instance_of::<PyTypeError>(py) {
                    format!("TypeError in handler execution: {}", err)
                } else {
                    format!("Error in handler execution: {}", err)
                };
                (StatusCode::INTERNAL_SERVER_ERROR).into_response()
            })
        },
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: String,
) -> Response {
    match rt_handle.spawn_blocking(move || {
        Python::attach(|py| {
            if let Some(py_func) = ROUTES.get(&route_key) {
                match py_func.call0(py) {
                    Ok(result) => Ok(py_to_response(py, &result.into_bound(py))),
                    Err(err) => {
                        err.print(py);
                        Err(err)
                    }
                }
            } else {
                eprintln!("Route handler not found for {}", route_key);
                Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "Route handler not found",
                ))
            }
        })
    }).await
    {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            Python::attach(|py| {
                err.print(py);
                if err.is_instance_of::<PyTypeError>(py) {
                    format!("TypeError in handler execution: {}", err)
                } else {
                    format!("Error in handler execution: {}", err)
                };
                (StatusCode::INTERNAL_SERVER_ERROR).into_response()
            })
        },
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}