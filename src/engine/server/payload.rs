use crate::engine::server::*;
use crate::engine::types::{FastrAPI, FrontendMount, StaticMount};
use ahash::{AHashMap, AHashSet};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    middleware::{self as axum_middleware, Next},
    response::{Html, IntoResponse, Response},
    routing::{MethodRouter, *},
    serve::ListenerExt,
};
use dashmap::DashMap;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use pyo3::{
    exceptions::{PyRuntimeError, PyTypeError},
    intern,
    prelude::*,
};
use smallvec::SmallVec;
use std::{
    collections::hash_map::Entry,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tower::{ServiceExt, service_fn};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{CompressionLayer, predicate::SizeAbove},
    normalize_path::NormalizePathLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer, cookie::Key};
use tracing::{Level, error, info};

use crate::{
    ffi::py_handlers::{
        ExecutionMode, render_no_request_json_response, render_no_request_response, run_py_handler,
        run_py_handler_no_request,
    },
    globals::{MIDDLEWARES, PYTHON_RUNTIME},
    http::{
        middleware::{
            CORSMiddleware, GZipMiddleware, HTTPSRedirectMiddleware, SessionMiddleware,
            TrustedHostMiddleware, build_cors_layer, parse_cors_params, parse_gzip_params,
            parse_https_redirect_params, parse_session_params, parse_trusted_host_params,
        },
        websocket::ws_handler,
    },
    routing::{
        prometheus::prometheus_handle,
        router::{FrozenRouter, FrozenRouterBuilder, RouteMatch},
        types::{BodyField, BodyPayload, HttpMethod, PathParamRange, RouteHandler, UploadedFile},
    },
    utils::{local_guard, openapi::build_openapi_spec, py_any_to_json},
};

pub(crate) async fn extract_payload(
    headers: &HeaderMap,
    body: Body,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<Option<BodyPayload>, Response> {
    let body = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response())?;
    if body.is_empty() {
        return Ok(None);
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/x-www-form-urlencoded") {
        return parse_urlencoded_form(&body, state.max_field_size)
            .map(|form| Some(BodyPayload::Form(form)));
    }

    if content_type.starts_with("multipart/form-data") {
        return parse_multipart_form(body, content_type, handler, state)
            .await
            .map(|form| Some(BodyPayload::Form(form)));
    }

    let defer_json_parse = handler.body_param_indices.len() == 1
        && handler.parsed_params[handler.body_param_indices[0]].is_pydantic_model;

    if defer_json_parse {
        return Ok(Some(BodyPayload::Json {
            raw: body,
            value: None,
        }));
    }

    let value = sonic_rs::from_slice(&body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid JSON body").into_response())?;
    Ok(Some(BodyPayload::Json {
        raw: body,
        value: Some(value),
    }))
}

pub(crate) fn parse_urlencoded_form(
    body: &[u8],
    max_field_size: Option<usize>,
) -> Result<ahash::AHashMap<String, BodyField>, Response> {
    let raw = std::str::from_utf8(body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid form body").into_response())?;
    let mut form = ahash::AHashMap::new();

    form_urlencoded::parse(raw.as_bytes()).try_for_each(
        |(key, value)| -> Result<(), Response> {
            if let Some(limit) = max_field_size
                && value.len() > limit
            {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Form field too large").into_response());
            }
            form.insert(key.into_owned(), BodyField::Text(value.into_owned()));
            Ok(())
        },
    )?;

    Ok(form)
}
pub(crate) async fn parse_multipart_form(
    body: bytes::Bytes,
    content_type: &str,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<ahash::AHashMap<String, BodyField>, Response> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Missing multipart boundary").into_response())?;
    let stream =
        futures_util::stream::once(
            async move { Ok::<bytes::Bytes, std::convert::Infallible>(body) },
        );
    let constraints = multipart_constraints(handler, state);
    let mut multipart = multer::Multipart::with_constraints(stream, boundary, constraints);
    let mut form = ahash::AHashMap::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(multipart_error_response)?
    {
        let Some(name) = field.name().map(str::to_owned) else {
            continue;
        };
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(ToString::to_string);
        let bytes = field.bytes().await.map_err(multipart_error_response)?;

        if filename.is_some() {
            form.insert(
                name,
                BodyField::File(UploadedFile {
                    filename,
                    content_type,
                    content: bytes.to_vec(),
                }),
            );
        } else {
            form.insert(
                name,
                BodyField::Text(String::from_utf8_lossy(&bytes).into_owned()),
            );
        }
    }

    Ok(form)
}

pub(crate) fn multipart_error_response(err: multer::Error) -> Response {
    match err {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            (StatusCode::PAYLOAD_TOO_LARGE, err.to_string()).into_response()
        }
        multer::Error::UnknownField { .. } => {
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, "Invalid multipart body").into_response(),
    }
}

pub(crate) fn multipart_constraints(handler: &RouteHandler, state: &AppState) -> multer::Constraints {
    let mut size_limit = multer::SizeLimit::new();

    if let Some(limit) = state.max_body_size {
        size_limit = size_limit.whole_stream(limit as u64);
    }

    match (state.max_field_size, state.max_file_size) {
        (Some(field), Some(file)) => {
            size_limit = size_limit.per_field(field.max(file) as u64);
        }
        (Some(field), None) => {
            size_limit = size_limit.per_field(field as u64);
        }
        (None, Some(file)) => {
            size_limit = size_limit.per_field(file as u64);
        }
        (None, None) => {}
    }

    let mut allowed = Vec::new();
    for param in handler
        .parsed_params
        .iter()
        .filter(|p| matches!(p.source, crate::routing::types::ParameterSource::Body))
    {
        allowed.push(param.external_name.clone());
        if param.external_name != param.name {
            allowed.push(param.name.clone());
        }

        let limit = if is_file_param(param) {
            state.max_file_size
        } else {
            state.max_field_size
        };
        if let Some(limit) = limit {
            size_limit = size_limit.for_field(param.external_name.clone(), limit as u64);
            if param.external_name != param.name {
                size_limit = size_limit.for_field(param.name.clone(), limit as u64);
            }
        }
    }

    let mut constraints = multer::Constraints::new().size_limit(size_limit);
    if state.reject_unknown_multipart_fields && !allowed.is_empty() {
        constraints = constraints.allowed_fields(allowed);
    }
    constraints
}

pub(crate) fn is_file_param(param: &crate::routing::types::ParsedParameter) -> bool {
    let default_is_file = param
        .param_object
        .as_ref()
        .and_then(|obj| {
            Python::attach(|py| {
                obj.bind(py)
                    .get_type()
                    .name()
                    .ok()
                    .map(|name| name.to_string_lossy().into_owned())
            })
        })
        .map(|name| name == "File")
        .unwrap_or(false);

    let annotation_is_upload = param
        .annotation
        .as_ref()
        .and_then(|annotation| {
            Python::attach(|py| {
                annotation
                    .bind(py)
                    .getattr(intern!(py, "__name__"))
                    .ok()
                    .and_then(|name| name.extract::<String>().ok())
            })
        })
        .map(|name| name.contains("UploadFile"))
        .unwrap_or(false);

    default_is_file || annotation_is_upload
}
