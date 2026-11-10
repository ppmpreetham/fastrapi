//! This module contains the `GZipMiddleware` class.
//! It reads the gzip compression configuration from Python keyword arguments and compresses responses like Starlette.

use super::add_vary;
use crate::exceptions::plain_server_error;
use axum::BoxError;
use axum::body::{Body, Bytes, HttpBody as _, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use flate2::Compression;
use flate2::write::GzEncoder;
use futures_util::{Stream, StreamExt};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use smart_default::SmartDefault;
use std::io::{self, Write};
use std::sync::Arc;

const DEFAULT_EXCLUDED_CONTENT_TYPES: [&str; 14] = [
    "application/grpc",
    "application/gzip",
    "application/x-gzip",
    "application/zip",
    "audio/*",
    "font/woff",
    "font/woff2",
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/event-stream",
    "video/*",
];

fn default_excluded() -> Vec<String> {
    DEFAULT_EXCLUDED_CONTENT_TYPES.map(str::to_owned).to_vec()
}

#[pyclass(frozen, name = "GZipMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct GZipMiddleware {
    #[default(500)]
    pub minimum_size: usize,
    #[default(9)]
    pub compresslevel: u32,
    #[default(131_072)]
    pub thread_minimum_size: usize,
    #[default(_code = "default_excluded()")]
    pub exclude_content_types: Vec<String>,
}

#[pymethods]
impl GZipMiddleware {
    #[new]
    #[pyo3(
        signature = (_app=None, minimum_size=500, compresslevel=9, thread_minimum_size=131_072, *, exclude_content_types=default_excluded()),
        text_signature = "(app=None, minimum_size=500, compresslevel=9, thread_minimum_size=131072, *, exclude_content_types=...)"
    )]
    fn new(
        _app: Option<&Bound<'_, PyAny>>,
        minimum_size: usize,
        compresslevel: u32,
        thread_minimum_size: usize,
        exclude_content_types: Vec<String>,
    ) -> Self {
        Self {
            minimum_size,
            compresslevel,
            thread_minimum_size,
            exclude_content_types: exclude_content_types
                .into_iter()
                .map(|content_type| media_type(&content_type).to_ascii_lowercase())
                .collect(),
        }
    }
}

impl GZipMiddleware {
    fn excludes(&self, headers: &HeaderMap) -> bool {
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map_or("", media_type);
        let major = content_type.split('/').next().unwrap_or_default();
        let grpc = content_type
            .get(..17)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("application/grpc+"));
        self.exclude_content_types.iter().any(|excluded| {
            excluded.eq_ignore_ascii_case(content_type)
                || excluded
                    .strip_suffix("/*")
                    .is_some_and(|excluded| excluded.eq_ignore_ascii_case(major))
                || grpc && excluded == "application/grpc"
        })
    }

    fn level(&self) -> Compression {
        Compression::new(self.compresslevel.min(9))
    }

    async fn buffered(&self, response: Response, accepts_gzip: bool) -> Response {
        let (mut parts, body) = response.into_parts();
        add_vary(&mut parts.headers, "Accept-Encoding");
        if !accepts_gzip {
            return Response::from_parts(parts, body);
        }
        let Ok(bytes) = to_bytes(body, usize::MAX).await else {
            return plain_server_error();
        };
        let level = self.level();
        let compressed = if bytes.len() >= self.thread_minimum_size {
            tokio::task::spawn_blocking(move || gzip(&bytes, level))
                .await
                .unwrap_or_else(|err| Err(io::Error::other(err)))
        } else {
            gzip(&bytes, level)
        };
        let Ok(compressed) = compressed else {
            return plain_server_error();
        };
        parts
            .headers
            .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        parts
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from(compressed.len()));
        Response::from_parts(parts, Body::from(compressed))
    }

    fn streamed(&self, response: Response, accepts_gzip: bool) -> Response {
        let (mut parts, body) = response.into_parts();
        add_vary(&mut parts.headers, "Accept-Encoding");
        if !accepts_gzip {
            return Response::from_parts(parts, body);
        }
        parts
            .headers
            .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        parts.headers.remove(CONTENT_LENGTH);
        Response::from_parts(parts, Body::from_stream(gzip_stream(body, self.level())))
    }
}

fn media_type(content_type: &str) -> &str {
    content_type.split(';').next().unwrap_or_default().trim()
}

fn gzip(data: &[u8], level: Compression) -> io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::with_capacity(data.len() / 2), level);
    encoder.write_all(data)?;
    encoder.finish()
}

fn gzip_stream(body: Body, level: Compression) -> impl Stream<Item = Result<Bytes, BoxError>> {
    async_stream::try_stream! {
        let mut chunks = body.into_data_stream();
        let mut encoder = GzEncoder::new(Vec::new(), level);
        while let Some(chunk) = chunks.next().await {
            encoder.write_all(&chunk?)?;
            encoder.flush()?;
            yield Bytes::from(std::mem::take(encoder.get_mut()));
        }
        yield Bytes::from(encoder.finish()?);
    }
}

pub async fn compress(
    State(config): State<Arc<GZipMiddleware>>,
    request: Request,
    next: Next,
) -> Response {
    let accepts_gzip = request
        .headers()
        .get(ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("gzip"));
    let response = next.run(request).await;
    if response.status() == StatusCode::PARTIAL_CONTENT
        || response.headers().contains_key(CONTENT_ENCODING)
        || config.excludes(response.headers())
    {
        return response;
    }
    match response.body().size_hint().exact() {
        Some(size) if size < config.minimum_size as u64 => response,
        Some(_) => config.buffered(response, accepts_gzip).await,
        None => config.streamed(response, accepts_gzip),
    }
}

pub fn parse_gzip_params(kwargs: &Bound<'_, PyDict>) -> PyResult<GZipMiddleware> {
    kwargs
        .py()
        .get_type::<GZipMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}
