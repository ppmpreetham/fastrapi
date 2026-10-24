use super::response_convert::{RawHeaders, apply_raw_headers, apply_response_metadata};
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::{Bound, Py, PyAny, intern};
use std::convert::Infallible;
use std::fs;
use std::fs::Metadata;
use std::iter::once;
use std::ops::RangeInclusive;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

pub(crate) fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
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
        let Ok(mut file) = File::open(path).await else { return; };
        let mut buffer = vec![0_u8; 64 * 1024];
        for range in ranges {
            if let Some(boundary) = &boundary {
                yield Ok::<bytes::Bytes, Infallible>(Bytes::from(format!("--{boundary}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {}-{}\r\n\r\n", range.start(), range.end())));
            }
            if file.seek(SeekFrom::Start(*range.start())).await.is_err() { return; }
            let mut remaining = range.end().saturating_sub(*range.start()).saturating_add(1);
            while remaining > 0 {
                let size = remaining.min(buffer.len() as u64) as usize;
                let Ok(read) = file.read(&mut buffer[..size]).await else { return; };
                if read == 0 { return; }
                remaining -= read as u64;
                yield Ok::<bytes::Bytes, Infallible>(Bytes::copy_from_slice(&buffer[..read]));
            }
            if boundary.is_some() {
                yield Ok::<bytes::Bytes, Infallible>(Bytes::from_static(b"\r\n"));
            }
        }
        if let Some(boundary) = boundary {
            yield Ok::<bytes::Bytes, Infallible>(Bytes::from(format!("--{boundary}--\r\n")));
        }
    })
}

pub fn convert_file_response(
    py: Python<'_>,
    result: &Bound<'_, PyAny>,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let get_str = |attr: &str| -> Option<String> {
        result
            .getattr(attr)
            .ok()
            .filter(|value| !value.is_none())
            .and_then(|value| value.extract().ok())
    };
    let Some(path) = get_str("path") else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let status = result
        .getattr(intern!(py, "status_code"))
        .ok()
        .and_then(|value| value.extract::<u16>().ok())
        .and_then(|value| StatusCode::from_u16(value).ok())
        .unwrap_or(StatusCode::OK);
    let Ok(metadata) = fs::metadata(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let length = metadata.len();
    let modified = metadata.modified().ok();
    let etag = etag_for(&metadata).unwrap_or_else(|| format!("\"{length:x}\""));
    let headers = request_headers.cloned().unwrap_or_default();
    if conditional_not_modified(&headers, &etag, modified) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response());
    }
    let use_range =
        headers.get(header::RANGE).is_some() && if_range_matches(&headers, &etag, modified);
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
    let boundary = multi.then(|| format!("fastrapi-{length:x}-{status}"));
    let declared = get_str("media_type");
    let media_type = declared
        .as_deref()
        .unwrap_or_else(|| guess_mime(&path))
        .to_owned();
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
    if let Some(filename) = get_str("filename") {
        let escaped = filename.replace(['"', '\\'], "");
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{escaped}\""),
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
