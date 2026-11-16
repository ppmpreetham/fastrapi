//! The extraction of the request body for route handlers.
//! This module parses JSON, URL-encoded forms and multipart forms.

use super::serve::AppState;

use ahash::AHashMap;
use axum::body::{Body, to_bytes};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use fastrapi_core::body::{BodyField, BodyPayload, FormText, UploadedFile};
use fastrapi_core::params::ParameterSource;
use fastrapi_routing::route::ParsedParameter;
use fastrapi_routing::route::RouteHandler;
use futures_util::StreamExt;
use http_body_util::LengthLimitError;
use multer::{Constraints, Error, Multipart, SizeLimit};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::str::from_utf8;
use std::sync::Arc;

fn part_headers(headers: &HeaderMap) -> Arc<[(String, String)]> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

pub(crate) async fn extract_payload(
    headers: &HeaderMap,
    body: Body,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<Option<BodyPayload>, Response> {
    let raw = to_bytes(body, state.max_body_size.unwrap_or(usize::MAX))
        .await
        .map_err(|err| body_read_error_response(&err))?;
    if !handler.payload.body_is_form {
        return Ok((!raw.is_empty()).then_some(BodyPayload::Json { raw }));
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let fields = if content_type.starts_with("multipart/form-data") {
        parse_multipart_form(Body::from(raw.clone()), content_type, handler, state).await?
    } else if content_type.starts_with("application/x-www-form-urlencoded") {
        parse_urlencoded_form(&raw, state.max_field_size)?
    } else {
        AHashMap::new()
    };
    Ok(Some(BodyPayload::Form { fields, raw }))
}

pub(crate) fn parse_urlencoded_form(
    body: &[u8],
    max_field_size: Option<usize>,
) -> Result<AHashMap<FormText, SmallVec<[BodyField; 2]>>, Response> {
    let raw = from_utf8(body)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "Invalid form body").into_response())?;
    let mut form: AHashMap<FormText, SmallVec<[BodyField; 2]>> = AHashMap::new();

    form_urlencoded::parse(raw.as_bytes()).try_for_each(
        |(key, value)| -> Result<(), Response> {
            if let Some(limit) = max_field_size
                && value.len() > limit
            {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Form field too large").into_response());
            }
            form.entry(form_text(key))
                .or_default()
                .push(BodyField::Text(form_text(value)));
            Ok(())
        },
    )?;

    Ok(form)
}

/// The response for a request body that could not be read. Only the
/// `max_body_size` limit gives a 413, like Starlette's body limit; any other
/// failure (a client disconnect, a malformed chunked body) is a 400, like the
/// `except Exception` around the body read in FastAPI's `get_request_handler`.
pub(crate) fn body_read_error_response(err: &axum::Error) -> Response {
    let mut source: Option<&dyn std::error::Error> = Some(err);
    while let Some(current) = source {
        if current.is::<LengthLimitError>() {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
        }
        source = current.source();
    }
    (
        StatusCode::BAD_REQUEST,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        r#"{"detail":"There was an error parsing the body"}"#,
    )
        .into_response()
}

fn form_text(text: Cow<'_, str>) -> FormText {
    FormText::new(text)
}

pub(crate) async fn parse_multipart_form(
    body: Body,
    content_type: &str,
    handler: &RouteHandler,
    state: &AppState,
) -> Result<AHashMap<FormText, SmallVec<[BodyField; 2]>>, Response> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Missing multipart boundary").into_response())?;
    let stream = body
        .into_data_stream()
        .map(|res| res.map_err(|e| e.to_string()));
    let rules = MultipartRules { handler, state };
    let mut multipart = Multipart::with_constraints(stream, boundary, rules.constraints());
    let mut form: AHashMap<FormText, SmallVec<[BodyField; 2]>> = AHashMap::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| multipart_error_response(&err))?
    {
        let param = field.name().and_then(|name| rules.param(name));
        if rules.rejects_unknown() && param.is_none() {
            let field_name = field.name().map(str::to_owned);
            return Err(multipart_error_response(&Error::UnknownField {
                field_name,
            }));
        }
        let Some(name) = field.name().map(FormText::new) else {
            continue;
        };
        let field_limit = param.and_then(|param| rules.field_limit(param));
        let field_name = field_limit.map(|_| field.name().map(str::to_owned));
        let filename = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(ToString::to_string);
        let headers = part_headers(field.headers());
        let bytes = field
            .bytes()
            .await
            .map_err(|err| multipart_error_response(&err))?;
        if let (Some(limit), Some(field_name)) = (field_limit, field_name)
            && bytes.len() as u64 > limit
        {
            let exceeded = Error::FieldSizeExceeded { limit, field_name };
            return Err(multipart_error_response(&exceeded));
        }

        let entry = match filename {
            Some(filename) => BodyField::File(UploadedFile {
                filename: Some(filename),
                content_type,
                headers,
                content: bytes,
            }),
            None => BodyField::Text(form_text(String::from_utf8_lossy(&bytes))),
        };
        form.entry(name).or_default().push(entry);
    }

    Ok(form)
}

pub(crate) fn multipart_error_response(err: &multer::Error) -> Response {
    match err {
        Error::FieldSizeExceeded { .. } | Error::StreamSizeExceeded { .. } => {
            (StatusCode::PAYLOAD_TOO_LARGE, err.to_string()).into_response()
        }
        Error::UnknownField { .. } => (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
        _ => (StatusCode::BAD_REQUEST, "Invalid multipart body").into_response(),
    }
}

/// The size limits and allowed fields of a multipart body, read from the
/// route and the app settings without building any per-field table: multer
/// enforces the stream limit and the largest field limit while it streams,
/// and the parser checks the limit of each declared field after reading it.
struct MultipartRules<'a> {
    handler: &'a RouteHandler,
    state: &'a AppState,
}

impl MultipartRules<'_> {
    fn constraints(&self) -> Constraints {
        let mut size_limit = SizeLimit::new();
        if let Some(limit) = self.state.max_body_size {
            size_limit = size_limit.whole_stream(limit as u64);
        }
        if let Some(limit) = self
            .state
            .max_field_size
            .into_iter()
            .chain(self.state.max_file_size)
            .max()
        {
            size_limit = size_limit.per_field(limit as u64);
        }
        Constraints::new().size_limit(size_limit)
    }

    fn body_params(&self) -> impl Iterator<Item = &ParsedParameter> {
        self.handler
            .payload
            .parsed_params
            .iter()
            .filter(|param| param.source == ParameterSource::Body)
    }

    fn param(&self, name: &str) -> Option<&ParsedParameter> {
        self.body_params()
            .find(|param| param.external_name == name || param.name == name)
    }

    fn rejects_unknown(&self) -> bool {
        self.state.reject_unknown_multipart_fields && self.body_params().next().is_some()
    }

    fn field_limit(&self, param: &ParsedParameter) -> Option<u64> {
        let limit = if param.is_file {
            self.state.max_file_size
        } else {
            self.state.max_field_size
        };
        limit.map(|limit| limit as u64)
    }
}
