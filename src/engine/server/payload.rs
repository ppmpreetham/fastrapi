use super::serve::*;

use crate::routing::types::{BodyField, BodyPayload, RouteHandler, UploadedFile};
use ahash::AHashMap;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;

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
        return parse_multipart_form(body, content_type, handler, state)
            .await
            .map(|form| Some(BodyPayload::Form(form)));
    }

    let body = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response())?;
    if body.is_empty() {
        return Ok(None);
    }

    if content_type.starts_with("application/x-www-form-urlencoded") {
        return parse_urlencoded_form(&body, state.max_field_size)
            .map(|form| Some(BodyPayload::Form(form)));
    }

    if handler.validation.defer_json_parse {
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
) -> Result<AHashMap<String, BodyField>, Response> {
    let raw = std::str::from_utf8(body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid form body").into_response())?;
    let mut form = AHashMap::new();

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
    body: Body,
    content_type: &str,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<AHashMap<String, BodyField>, Response> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Missing multipart boundary").into_response())?;
    let stream = body
        .into_data_stream()
        .map(|res| res.map_err(|e| e.to_string()));
    let constraints = multipart_constraints(handler, state);
    let mut multipart = multer::Multipart::with_constraints(stream, boundary, constraints);
    let mut form = AHashMap::new();

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

pub(crate) fn multipart_constraints(
    handler: &RouteHandler,
    state: &AppState,
) -> multer::Constraints {
    let mut size_limit = multer::SizeLimit::new();

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

    let allowed: Vec<String> = handler
        .payload
        .parsed_params
        .iter()
        .filter(|p| matches!(p.source, crate::routing::types::ParameterSource::Body))
        .flat_map(|param| {
            if param.external_name != param.name {
                vec![param.external_name.clone(), param.name.clone()]
            } else {
                vec![param.external_name.clone()]
            }
        })
        .collect();

    for param in handler
        .payload
        .parsed_params
        .iter()
        .filter(|p| matches!(p.source, crate::routing::types::ParameterSource::Body))
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
    }

    let mut constraints = multer::Constraints::new().size_limit(size_limit);
    if state.reject_unknown_multipart_fields && !allowed.is_empty() {
        constraints = constraints.allowed_fields(allowed);
    }
    constraints
}
