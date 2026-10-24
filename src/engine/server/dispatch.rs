//! The dispatch of HTTP requests to the route handlers.
//! The dispatch finds the route, applies the rate limit and returns a 404 response if no route matches.

use super::payload::extract_payload;
use super::serve::AppState;
use fastrapi_core::rate_limit::is_rate_limited;

use axum::extract::Request;
use axum::http::header::{ALLOW, HOST};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pyo3::prelude::Python;
use smallvec::SmallVec;

use crate::engine::exception_dispatch::dispatch_validation_handler;
use crate::engine::executor::{run_py_handler, run_py_handler_no_request};
use fastrapi_core::method::HttpMethod;
use fastrapi_core::params::PathParamRange;
use fastrapi_core::path::strip_root_path;
use fastrapi_core::router::{FrozenRouter, RouteMatch, RoutePattern};
use fastrapi_routing::route::{ExecutionMode, RouteHandler};
use fastrapi_runtime::py_bridge;
use std::ptr;

/// Sends a request to its route handler.
pub(crate) async fn dispatch_or_not_found(
    router: &'static FrozenRouter<RouteHandler>,
    state: &'static AppState,
    req: Request,
) -> Result<Response, Request> {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
    };

    let Some(path_str) = dispatch_path(state, req.uri().path()) else {
        return Err(req);
    };

    let request_host = if router.has_host_routes() {
        req.headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.split(':').next().unwrap_or(h))
    } else {
        None
    };
    let Some(route_match) = router.resolve(method, path_str, request_host) else {
        let allowed = router.allowed_methods(path_str, request_host);
        if !allowed.is_empty() {
            let value = allowed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
            if let Ok(value) = HeaderValue::from_str(&value) {
                response.headers_mut().insert(ALLOW, value);
            }
            return Ok(response);
        }
        return Err(req);
    };

    let (target, params_iter) = match route_match {
        RouteMatch::Static(target) => (target, None),
        RouteMatch::Params(target, params) => (target, Some(params)),
    };
    let handler: &'static RouteHandler = &target.handler;
    let pattern: &'static str = &target.pattern;

    let metrics_enabled = state.metrics_enabled;
    let tag = move |mut resp: Response| {
        if metrics_enabled {
            resp.extensions_mut().insert(RoutePattern(pattern));
        }
        resp
    };

    if let Some(limit) = handler.execution.rate_limit_per_second
        && is_rate_limited(&req, ptr::from_ref(handler) as usize, limit)
    {
        return Ok(tag(StatusCode::TOO_MANY_REQUESTS.into_response()));
    }

    let event_loop = state.event_loop;

    let path_base = path_str.as_ptr() as usize;
    let param_ranges: SmallVec<[PathParamRange; 4]> = params_iter
        .map(|params| {
            params
                .iter()
                .enumerate()
                .map(|(i, (_k, v))| {
                    let start = v.as_ptr() as usize - path_base;
                    debug_assert!(
                        start <= path_str.len(),
                        "matchit returned a string outside the input path"
                    );
                    PathParamRange {
                        name_index: i,
                        start,
                        end: start + v.len(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    py_bridge::scoped_request_loop(event_loop, async move {
        let (request_parts, body) = req.into_parts();
        if matches!(
            handler.execution.execution_mode,
            ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
        ) {
            return Ok(tag(run_py_handler_no_request(
                event_loop,
                state.sync_to_threadpool,
                handler,
                Some(request_parts),
            )
            .await));
        }

        let needs_body = !handler.payload.body_param_indices.is_empty()
            || handler.payload.request_param.is_some()
            || handler.payload.dependency_needs_request;

        let payload = if needs_body {
            match extract_payload(&request_parts.headers, body, handler, state).await {
                Ok(p) => p,
                Err(resp) => {
                    return Ok(Python::attach(|py| {
                        dispatch_validation_handler(py, resp, None)
                    }));
                }
            }
        } else {
            None
        };

        let response = run_py_handler(
            event_loop,
            state.sync_to_threadpool,
            handler,
            request_parts,
            param_ranges,
            payload,
        )
        .await;
        Ok(tag(Python::attach(|py| {
            dispatch_validation_handler(py, response, None)
        })))
    })
    .await
}

pub(crate) fn dispatch_path<'a>(state: &AppState, original_path: &'a str) -> Option<&'a str> {
    strip_root_path(&state.root_path, original_path)
}
