use super::payload::extract_payload;
use super::rate_limit::is_rate_limited;
use super::serve::AppState;

use axum::{
    extract::Request,
    http::{HeaderValue, StatusCode, header::ALLOW},
    response::{IntoResponse, Response},
};
use pyo3::prelude::Python;
use smallvec::SmallVec;
use std::sync::Arc;

use crate::{
    ffi::py_handlers::{ExecutionMode, run_py_handler, run_py_handler_no_request},
    routing::{
        router::{FrozenRouter, RouteMatch, RoutePattern, RouteTarget},
        types::{HttpMethod, PathParamRange},
    },
    runtime::py_bridge,
};
use axum::http::header::HOST;
use crate::engine::errors::dispatch_validation_handler;

/// dispatches requests to the route handler
pub(crate) async fn dispatch_or_not_found(
    router: Arc<FrozenRouter>,
    state: AppState,
    req: Request,
) -> Result<Response, Request> {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
    };

    let Some(path_str) = dispatch_path(&state, req.uri().path()) else {
        return Err(req);
    };

    let request_host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h));
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
    let RouteTarget {
        handler, pattern, ..
    } = target;

    let metrics_enabled = state.metrics_enabled;
    let tag = move |resp: Response| {
        if metrics_enabled {
            tag_with_pattern(resp, &pattern)
        } else {
            resp
        }
    };

    if let Some(limit) = handler.execution.rate_limit_per_second
        && is_rate_limited(&req, Arc::as_ptr(&handler) as usize, limit)
    {
        return Ok(tag(StatusCode::TOO_MANY_REQUESTS.into_response()));
    }

    let async_loop = state.pick_loop();

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

    py_bridge::scoped_request_loop(async_loop.clone(), async move {
        let (request_parts, body) = req.into_parts();
        if matches!(
            handler.execution.execution_mode,
            ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
        ) {
            return Ok(tag(run_py_handler_no_request(
                async_loop,
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
            match extract_payload(&request_parts.headers, body, &handler, &state).await {
                Ok(p) => p.map(Arc::new),
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
            async_loop,
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

#[inline]
fn tag_with_pattern(mut resp: Response, pattern: &Arc<str>) -> Response {
    resp.extensions_mut().insert(RoutePattern(pattern.clone()));
    resp
}

pub(crate) fn dispatch_path<'a>(state: &AppState, original_path: &'a str) -> Option<&'a str> {
    dispatch_path_with_root(state.root_path.as_ref(), original_path)
}

pub(crate) fn dispatch_path_with_root<'a>(root: &str, original_path: &'a str) -> Option<&'a str> {
    if root.is_empty() {
        Some(original_path)
    } else if original_path == root {
        Some("/")
    } else if let Some(stripped) = original_path.strip_prefix(root) {
        stripped.starts_with('/').then_some(stripped)
    } else {
        None
    }
}
