use crate::engine::server::payload::*;
use crate::engine::server::rate_limit::*;
use crate::engine::server::serve::*;

use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use smallvec::SmallVec;
use std::sync::Arc;

use crate::{
    ffi::py_handlers::{ExecutionMode, run_py_handler, run_py_handler_no_request},
    routing::{
        router::{FrozenRouter, RouteMatch},
        types::{HttpMethod, PathParamRange},
    },
};

pub(crate) async fn dispatch(router: Arc<FrozenRouter>, state: AppState, req: Request) -> Response {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    };

    let Some(path_str) = dispatch_path(&state, req.uri().path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let route_match = match router.resolve(method, path_str) {
        Some(v) => v,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let (handler, params_iter) = match route_match {
        RouteMatch::Static(handler) => (handler, None),
        RouteMatch::Params(handler, params) => (handler, Some(params)),
    };

    if let Some(limit) = handler.rate_limit_per_second
        && is_rate_limited(&req, Arc::as_ptr(&handler) as usize, limit)
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    if matches!(
        handler.execution_mode,
        ExecutionMode::SyncNoArgs | ExecutionMode::AsyncNoArgs
    ) {
        return run_py_handler_no_request(
            state.rt_handle,
            state.async_loop,
            state.sync_to_threadpool,
            handler,
        )
        .await;
    }

    let path_base = path_str.as_ptr() as usize;
    let param_ranges: SmallVec<[PathParamRange; 4]> = if let Some(params) = params_iter {
        params
            .iter()
            .map(|(k, v)| {
                let start = v.as_ptr() as usize - path_base;
                debug_assert!(
                    start <= path_str.len(),
                    "matchit returned a string outside the input path"
                );
                PathParamRange {
                    key: k.to_string(),
                    start,
                    end: start + v.len(),
                }
            })
            .collect()
    } else {
        SmallVec::new()
    };

    let (request_parts, body) = req.into_parts();
    let has_body_requirements = !handler.body_param_indices.is_empty();

    let payload = if has_body_requirements {
        match extract_payload(&request_parts.headers, body, &handler, &state).await {
            Ok(p) => p,
            Err(resp) => return resp,
        }
    } else {
        None
    };

    run_py_handler(
        state.rt_handle,
        state.async_loop,
        state.sync_to_threadpool,
        handler,
        request_parts,
        param_ranges,
        payload,
    )
    .await
}

pub(crate) fn dispatch_path<'a>(state: &AppState, original_path: &'a str) -> Option<&'a str> {
    let root = state.root_path.trim_end_matches('/');
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
pub(crate) fn request_matches_router(
    router: &FrozenRouter,
    state: &AppState,
    req: &Request,
) -> bool {
    let Ok(method) = HttpMethod::try_from(req.method()) else {
        return true;
    };

    let Some(path) = dispatch_path(state, req.uri().path()) else {
        return false;
    };

    router.resolve(method, path).is_some()
}
