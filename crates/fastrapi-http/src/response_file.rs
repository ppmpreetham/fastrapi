//! This module converts a `FileResponse` into an axum response.
//! It finds the MIME type, streams the file, and supports conditional requests and range requests.

use super::response_convert::{RawHeaders, apply_raw_headers, apply_response_metadata};
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use pyo3::{Bound, Py, PyAny, intern};
use std::collections::hash_map::RandomState;
use std::fs;
use std::fs::Metadata;
use std::hash::BuildHasher;
use std::io;
use std::iter::once;
use std::ops::RangeInclusive;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;
use tracing::error;

struct MimeTables {
    suffixes: std::collections::HashMap<String, String>,
    encodings: std::collections::HashMap<String, String>,
    types: std::collections::HashMap<String, String>,
}

static MIME_TABLES: pyo3::sync::PyOnceLock<MimeTables> = pyo3::sync::PyOnceLock::new();

fn mime_tables(py: Python<'_>) -> PyResult<&MimeTables> {
    MIME_TABLES.get_or_try_init(py, || {
        let mimetypes = py.import(intern!(py, "mimetypes"))?;
        if mimetypes.getattr(intern!(py, "_db"))?.is_none() {
            mimetypes.call_method0(intern!(py, "init"))?;
        }
        let db = mimetypes.getattr(intern!(py, "_db"))?;
        Ok(MimeTables {
            suffixes: db.getattr(intern!(py, "suffix_map"))?.extract()?,
            encodings: db.getattr(intern!(py, "encodings_map"))?.extract()?,
            types: db
                .getattr(intern!(py, "types_map"))?
                .get_item(1)?
                .extract()?,
        })
    })
}

const fn is_separator(byte: u8) -> bool {
    byte == b'/' || (cfg!(windows) && byte == b'\\')
}

fn splitext(path: &str) -> (&str, &str) {
    let bytes = path.as_bytes();
    let filename_start = bytes
        .iter()
        .rposition(|&byte| is_separator(byte))
        .map_or(0, |index| index + 1);
    match path[filename_start..].rfind('.') {
        Some(dot)
            if path[filename_start..filename_start + dot]
                .bytes()
                .any(|byte| byte != b'.') =>
        {
            path.split_at(filename_start + dot)
        }
        _ => (path, ""),
    }
}

fn has_url_scheme(path: &str) -> bool {
    path.find(':').is_some_and(|colon| {
        colon > 1
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[..colon]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}

pub(crate) fn guess_media_type(py: Python<'_>, path: &str) -> PyResult<Option<String>> {
    if has_url_scheme(path) {
        return py
            .import(intern!(py, "mimetypes"))?
            .call_method1(intern!(py, "guess_type"), (path,))?
            .get_item(0)?
            .extract();
    }
    let tables = mime_tables(py)?;
    let (base, ext) = splitext(path);
    if !tables.suffixes.contains_key(&ext.to_lowercase()) {
        return Ok(lookup(tables, base, ext));
    }
    let (mut base, mut ext) = (base.to_owned(), ext.to_owned());
    while let Some(suffix) = tables.suffixes.get(&ext.to_lowercase()) {
        let joined = format!("{base}{suffix}");
        let (next_base, next_ext) = splitext(&joined);
        (base, ext) = (next_base.to_owned(), next_ext.to_owned());
    }
    Ok(lookup(tables, &base, &ext))
}

fn lookup(tables: &MimeTables, base: &str, ext: &str) -> Option<String> {
    let ext = if tables.encodings.contains_key(ext) {
        splitext(base).1
    } else {
        ext
    };
    tables.types.get(&ext.to_lowercase()).cloned()
}

/// A random 26-hex-digit boundary, the size of Starlette's `token_hex(13)`.
fn random_boundary() -> String {
    let high = RandomState::new().hash_one(0_u8);
    let low = RandomState::new().hash_one(1_u8) & 0xff_ffff_ffff;
    format!("{high:016x}{low:010x}")
}

fn etag_for(metadata: &Metadata) -> Option<String> {
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!("\"{:x}-{:x}\"", modified.as_secs(), metadata.len()))
}

fn etag_matches(value: &str, etag: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|item| item == "*" || item.trim_start_matches("W/") == etag.trim_start_matches("W/"))
}

fn if_range_matches(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    let Some(value) = headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if etag_matches(value, etag) {
        return true;
    }
    modified
        .and_then(|time| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| time <= date)
        })
        .unwrap_or(false)
}

fn conditional_not_modified(headers: &HeaderMap, etag: &str, modified: Option<SystemTime>) -> bool {
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        return etag_matches(value, etag);
    }
    modified
        .zip(
            headers
                .get(header::IF_MODIFIED_SINCE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| httpdate::parse_http_date(value).ok()),
        )
        .is_some_and(|(modified, since)| {
            modified
                .duration_since(UNIX_EPOCH)
                .is_ok_and(|value| UNIX_EPOCH + Duration::from_secs(value.as_secs()) <= since)
        })
}

fn file_stream(
    path: String,
    ranges: Vec<RangeInclusive<u64>>,
    boundary: Option<String>,
    content_type: String,
) -> Body {
    Body::from_stream(async_stream::stream! {
        // The headers are already sent, so a failure aborts the connection instead of
        // ending a body shorter than its declared length.
        let mut file = match File::open(&path).await {
            Ok(file) => file,
            Err(err) => {
                error!("FileResponse could not open {path}: {err}");
                yield Err(err);
                return;
            }
        };
        for range in ranges {
            if let Some(boundary) = &boundary {
                yield Ok::<bytes::Bytes, io::Error>(Bytes::from(format!("--{boundary}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {}-{}\r\n\r\n", range.start(), range.end())));
            }
            if let Err(err) = file.seek(SeekFrom::Start(*range.start())).await {
                error!("FileResponse could not seek in {path}: {err}");
                yield Err(err);
                return;
            }
            let length = range.end().saturating_sub(*range.start()).saturating_add(1);
            let mut sent = 0_u64;
            let mut chunks = ReaderStream::with_capacity((&mut file).take(length), 64 * 1024);
            while let Some(chunk) = chunks.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        error!("FileResponse could not read {path}: {err}");
                        yield Err(err);
                        return;
                    }
                };
                sent += chunk.len() as u64;
                yield Ok(chunk);
            }
            if sent < length {
                error!("FileResponse: {path} became shorter while it was sent");
                yield Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                return;
            }
            if boundary.is_some() {
                yield Ok(Bytes::from_static(b"\r\n"));
            }
        }
        if let Some(boundary) = boundary {
            yield Ok(Bytes::from(format!("--{boundary}--\r\n")));
        }
    })
}

pub fn convert_file_response(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let get_str = |attr: &Bound<'_, PyString>| -> Option<String> {
        result
            .getattr_opt(attr)
            .ok()
            .flatten()
            .filter(|value| !value.is_none())
            .and_then(|value| value.extract().ok())
    };
    let Some(path) = get_str(intern!(py, "path")) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let status = result
        .getattr(intern!(py, "status_code"))
        .ok()
        .and_then(|value| value.extract::<u16>().ok())
        .and_then(|value| StatusCode::from_u16(value).ok())
        .unwrap_or(StatusCode::OK);
    let Ok(metadata) = py.detach(|| fs::metadata(&path)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let length = metadata.len();
    let modified = metadata.modified().ok();
    let etag = etag_for(&metadata).unwrap_or_else(|| format!("\"{length:x}\""));
    let empty = HeaderMap::new();
    let headers = request_headers.unwrap_or(&empty);
    if conditional_not_modified(headers, &etag, modified) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
    }
    let use_range =
        headers.get(header::RANGE).is_some() && if_range_matches(headers, &etag, modified);
    let ranges = if use_range {
        let value = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let Some(ranges) = http_range_header::parse_range_header(value)
            .ok()
            .and_then(|ranges| ranges.validate(length).ok())
        else {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response());
        };
        ranges
    } else if length == 0 {
        Vec::new()
    } else {
        once(0..=length - 1).collect()
    };
    let partial = use_range;
    let multi = ranges.len() > 1;
    let boundary = multi.then(random_boundary);
    let declared = get_str(intern!(py, "media_type")).or_else(|| {
        let filename = get_str(intern!(py, "filename"));
        guess_media_type(py, filename.as_deref().unwrap_or(&path))
            .ok()
            .flatten()
    });
    let media_type = crate::response_api::content_type(
        declared.as_deref().unwrap_or("application/octet-stream"),
        "utf-8",
    )
    .into_owned();
    let body_length = (!multi).then(|| {
        ranges.first().map_or(0, |range| {
            range.end().saturating_sub(*range.start()).saturating_add(1)
        })
    });
    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            status
        })
        .header(header::ETAG, &etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_TYPE,
            boundary.as_ref().map_or_else(
                || media_type.clone(),
                |value| format!("multipart/byteranges; boundary={value}"),
            ),
        );
    if let Some(modified) = modified {
        builder = builder.header(header::LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    if let Some(length) = body_length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    if partial
        && !multi
        && let Some(range) = ranges.first()
    {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start(), range.end(), length),
        );
    }
    if let Some(filename) = get_str(intern!(py, "filename")) {
        let escaped = filename.replace(['"', '\\'], "");
        let disposition_type = get_str(intern!(py, "content_disposition_type"))
            .unwrap_or_else(|| "attachment".to_owned());
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!("{disposition_type}; filename=\"{escaped}\""),
        );
    }
    let response = builder
        .body(file_stream(path, ranges, boundary, media_type))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    let custom_headers = result
        .getattr(intern!(py, "headers"))
        .ok()
        .and_then(|value| value.extract::<Py<PyDict>>().ok());
    apply_raw_headers(
        apply_response_metadata(py, response, custom_headers.as_ref(), None),
        result,
        RawHeaders::CookiesOnly,
    )
}
