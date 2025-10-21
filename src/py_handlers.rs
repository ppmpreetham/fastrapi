use crate::pydantic::validate_with_pydantic;
use crate::{
    utils::{json_to_py_object, py_to_response},
    ROUTES,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pyo3::{
    prelude::*,
    types::{PyAny, PyDict, PyType},
};
use std::sync::Arc;

/// For routes WITH payload (POST, PUT, PATCH, DELETE)
pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    payload: serde_json::Value,
) -> Response {
    // Use spawn_blocking to prevent blocking Tokio's async executor
    // This is crucial because Python handlers may do I/O
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                // Detach during long operations to allow GC and other Python events
                if let Some(entry) = ROUTES.get(route_key.as_ref()) {
                    let py_func = entry.value().bind(py);

                    // Optimized type annotation checking
                    let py_payload = match py_func.getattr("__annotations__") {
                        Ok(annotations) => {
                            if let Ok(ann_dict) = annotations.cast::<PyDict>() {
                                if let Some(item) = ann_dict.items().into_iter().next() {
                                    if let Ok((_, type_hint)) =
                                        item.extract::<(Py<PyAny>, Py<PyAny>)>()
                                    {
                                        let type_hint_bound = type_hint.into_bound(py);

                                        if type_hint_bound.is_instance_of::<PyType>() {
                                            match validate_with_pydantic(
                                                py,
                                                &type_hint_bound,
                                                &payload,
                                            ) {
                                                Ok(validated) => validated,
                                                Err(err_resp) => return err_resp,
                                            }
                                        } else {
                                            json_to_py_object(py, &payload)
                                        }
                                    } else {
                                        json_to_py_object(py, &payload)
                                    }
                                } else {
                                    json_to_py_object(py, &payload)
                                }
                            } else {
                                json_to_py_object(py, &payload)
                            }
                        }
                        Err(_) => json_to_py_object(py, &payload),
                    };

                    // Vectorcall optimization via tuple
                    match py_func.call1((py_payload,)) {
                        Ok(result) => py_to_response(py, &result),
                        Err(err) => {
                            err.print(py);
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Error in route handler: {}", err),
                            )
                                .into_response()
                        }
                    }
                } else {
                    (StatusCode::NOT_FOUND, "Route handler not found").into_response()
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// For routes WITHOUT payload (GET, HEAD, OPTIONS)
pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
) -> Response {
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                if let Some(py_func) = ROUTES.get(route_key.as_ref()) {
                    match py_func.call0(py) {
                        Ok(result) => {
                            let result_bound = result.into_bound(py);
                            py_to_response(py, &result_bound)
                        }
                        Err(err) => {
                            err.print(py);
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Error in route handler: {}", err),
                            )
                                .into_response()
                        }
                    }
                } else {
                    (StatusCode::NOT_FOUND, "Route handler not found").into_response()
                }
            })
        })
        .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
