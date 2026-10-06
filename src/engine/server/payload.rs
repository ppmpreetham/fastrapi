use super::serve::AppState;

use crate::routing::types::{BodyField, BodyPayload, ParameterSource, RouteHandler, UploadedFile};
use ahash::AHashMap;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use smallvec::SmallVec;
use crate::ffi::pydantic::ValidationErrorDetails;
use multer::Constraints;
use multer::Error;
use multer::Multipart;
use multer::SizeLimit;
use std::str::from_utf8;

pub(crate) async fn extract_payload(
    headers: &HeaderMap,
    body: Body,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<Option<BodyPayload>, Response> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("multipart/form-data") {
        let raw = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
            .await
            .map_err(|_| {
                (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response()
            })?;
        return parse_multipart_form(Body::from(raw.clone()), content_type, handler, state)
            .await
            .map(|fields| Some(BodyPayload::Form { fields, raw }));
    }

    let body = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response())?;
    if body.is_empty() {
        return Ok(None);
    }

    if content_type.starts_with("application/x-www-form-urlencoded") {
        return parse_urlencoded_form(&body, state.max_field_size)
            .map(|fields| Some(BodyPayload::Form { fields, raw: body }));
    }

    if handler.validation.defer_json_parse {
        return Ok(Some(BodyPayload::Json {
            raw: body,
            value: None,
        }));
    }

    let mut json_buf = body.to_vec();
    let value = simd_json::to_owned_value(&mut json_buf).map_err(|err| {
        let detail = simd_json::json!({
            "detail": [{
                "type": "json_invalid",
                "loc": ["body", err.to_string()],
                "msg": "JSON decode error",
                "input": null,
            }]
        });
        let mut response =
            (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(detail.clone())).into_response();
        response
            .extensions_mut()
            .insert(ValidationErrorDetails(detail));
        response
    })?;
    Ok(Some(BodyPayload::Json {
        raw: body,
        value: Some(value),
    }))
}

pub(crate) fn parse_urlencoded_form(
    body: &[u8],
    max_field_size: Option<usize>,
) -> Result<AHashMap<String, SmallVec<[BodyField; 2]>>, Response> {
    let raw = from_utf8(body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid form body").into_response())?;
    let mut form: AHashMap<String, SmallVec<[BodyField; 2]>> = AHashMap::new();

    form_urlencoded::parse(raw.as_bytes()).try_for_each(
        |(key, value)| -> Result<(), Response> {
            if let Some(limit) = max_field_size
                && value.len() > limit
            {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Form field too large").into_response());
            }
            form.entry(key.into_owned())
                .or_default()
                .push(BodyField::Text(value.into_owned()));
            Ok(())
        },
    )?;

    Ok(form)
}

pub(crate) async fn parse_multipart_form(
    body: Body,
    content_type: &str,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<AHashMap<String, SmallVec<[BodyField; 2]>>, Response> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Missing multipart boundary").into_response())?;
    let stream = body
        .into_data_stream()
        .map(|res| res.map_err(|e| e.to_string()));
    let constraints = multipart_constraints(handler, state);
    let mut multipart = Multipart::with_constraints(stream, boundary, constraints);
    let mut form: AHashMap<String, SmallVec<[BodyField; 2]>> = AHashMap::new();

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

        let entry = match filename {
            Some(filename) => BodyField::File(UploadedFile {
                filename: Some(filename),
                content_type,
                content: bytes,
            }),
            None => BodyField::Text(String::from_utf8_lossy(&bytes).into_owned()),
        };
        form.entry(name).or_default().push(entry);
    }

    Ok(form)
}

pub(crate) fn multipart_error_response(err: multer::Error) -> Response {
    match err {
        Error::FieldSizeExceeded { .. } | Error::StreamSizeExceeded { .. } => {
            (StatusCode::PAYLOAD_TOO_LARGE, err.to_string()).into_response()
        }
        Error::UnknownField { .. } => {
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, "Invalid multipart body").into_response(),
    }
}

pub(crate) fn multipart_constraints(
    handler: &RouteHandler,
    state: &AppState,
) -> multer::Constraints {
    let mut size_limit = SizeLimit::new();

    if let Some(limit) = state.max_body_size {
        size_limit = size_limit.whole_stream(limit as u64);
    }

    if let Some(limit) = state
        .max_field_size
        .into_iter()
        .chain(state.max_file_size)
        .max()
    {
        size_limit = size_limit.per_field(limit as u64);
    }

    let mut allowed: Vec<String> = Vec::new();
    for param in handler
        .payload
        .parsed_params
        .iter()
        .filter(|p| p.source == ParameterSource::Body)
    {
        let limit = if param.is_file {
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

        if state.reject_unknown_multipart_fields {
            allowed.push(param.external_name.clone());
            if param.external_name != param.name {
                allowed.push(param.name.clone());
            }
        }
    }

    let mut constraints = Constraints::new().size_limit(size_limit);
    if state.reject_unknown_multipart_fields && !allowed.is_empty() {
        constraints = constraints.allowed_fields(allowed);
    }
    constraints
}
