use super::payload::*;
use super::rate_limit::*;
use super::serve::*;

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use smallvec::SmallVec;
use std::sync::Arc;

use crate::{
    ffi::py_handlers::{ExecutionMode, run_py_handler, run_py_handler_no_request},
    runtime::py_bridge,
    routing::{
        router::{FrozenRouter, RouteMatch, RoutePattern, RouteTarget},
        types::{HttpMethod, PathParamRange},
    },
};

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

    let Some(route_match) = router.resolve(method, path_str) else {
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

    py_bridge::scoped_request_loop(
        async_loop.clone(),
        async move {
            if matches!(
                handler.execution.execution_mode,
                ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
            ) {
                return Ok(tag(
                    run_py_handler_no_request(async_loop, state.sync_to_threadpool, handler).await,
                ));
            }

            let (request_parts, body) = req.into_parts();

            let needs_body = !handler.payload.body_param_indices.is_empty()
                || handler.payload.request_param.is_some();

            let payload = if !needs_body {
                None
            } else {
                match extract_payload(&request_parts.headers, body, &handler, &state).await {
                    Ok(p) => p.map(Arc::new),
                    Err(resp) => return Ok(resp),
                }
            };

            Ok(tag(
                run_py_handler(
                    async_loop,
                    state.sync_to_threadpool,
                    handler,
                    request_parts,
                    param_ranges,
                    payload,
                )
                .await,
            ))
        },
    )
    .await
}

#[inline]
fn tag_with_pattern(mut resp: Response, pattern: &Arc<str>) -> Response {
    resp.extensions_mut().insert(RoutePattern(pattern.clone()));
    resp
}

pub(crate) fn dispatch_path<'a>(state: &AppState, original_path: &'a str) -> Option<&'a str> {
    let root = state.root_path.as_ref();
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
