use crate::responses::convert_response_by_type;
use crate::utils::local_guard;
use crate::ROUTES;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use std::collections::HashMap;
use std::sync::Arc;

pub async fn run_py_handler_with_params(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    path_params: HashMap<String, String>,
    query_params: HashMap<String, String>,
    payload: Option<serde_json::Value>,
) -> Response {
    // TODO: If the route is sync, then use spawn_blocking, if it's async, use spawn and await the result
    match rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let guard = crate::utils::local_guard(&*ROUTES);
                let handler = match ROUTES.get(route_key.as_ref(), &guard) {
                    Some(h) => h,
                    None => return StatusCode::NOT_FOUND.into_response(),
                };

                let response_type = handler.response_type;
                let py_func = handler.func.bind_borrowed(py);

                let kwargs = if handler.needs_kwargs {
                    let dict = PyDict::new(py);

                    // TODO: what if there are overlapping names between path, query and body params? should we prioritize one over the other? should we error out? should we namespace them in the kwargs?
                    // path params
                    for (k, v) in &path_params {
                        // TODO: research if .ok() is enough here? should we log if it fails?
                        let key = PyString::intern(py, k.as_str());
                        dict.set_item(key, v).ok();
                    }

                    // query params
                    for (k, v) in &query_params {
                        let key = PyString::intern(py, k.as_str());
                        dict.set_item(key, v).ok();
                    }

                    // body / validation
                    if let Some(payload_val) = &payload {
                        // TODO: use 422 error if it can't be parsed/validated
                        if let Err(resp) = crate::pydantic::apply_body_and_validation(
                            py,
                            handler,
                            payload_val,
                            &dict,
                        ) {
                            return resp;
                        }
                    }

                    Some(dict)
                } else {
                    None
                };

                // PYTHON CALLING ONLY ONCE
                let result = match kwargs {
                    Some(kw) => py_func.call((), Some(&kw)),
                    None => py_func.call0(),
                };

                match result {
                    Ok(res) => convert_response_by_type(py, &res, response_type),
                    Err(err) => {
                        err.print(py);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            })
        })
        .await
    {
        Ok(resp) => resp,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn run_py_handler_with_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
    payload: serde_json::Value,
) -> Response {
    run_py_handler_with_params(
        rt_handle,
        route_key,
        HashMap::new(),
        HashMap::new(),
        Some(payload),
    )
    .await
}

pub async fn run_py_handler_no_args(
    rt_handle: tokio::runtime::Handle,
    route_key: Arc<str>,
) -> Response {
    rt_handle
        .spawn_blocking(move || {
            Python::attach(|py| {
                let guard = local_guard(&*ROUTES);
                let handler = match ROUTES.get(route_key.as_ref(), &guard) {
                    Some(h) => h,
                    None => return StatusCode::NOT_FOUND.into_response(),
                };
                let response_type = handler.response_type;
                match handler.func.call0(py) {
                    Ok(result) => {
                        convert_response_by_type(py, &result.into_bound(py), response_type)
                    }
                    Err(e) => {
                        e.print(py);
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            })
        })
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
