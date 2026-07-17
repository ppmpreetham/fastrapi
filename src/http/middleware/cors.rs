use axum::http::{HeaderName, HeaderValue, Method};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashSet;
use std::str::FromStr;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

#[pyclass(name = "CORSMiddleware", skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct CORSMiddleware {
    pub allow_origins: Vec<String>,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub allow_credentials: bool,
    pub expose_headers: Vec<String>,
    pub max_age: u64,
}

impl Default for CORSMiddleware {
    fn default() -> Self {
        Self {
            allow_origins: vec![],
            allow_methods: vec![
                String::from("GET"),
                String::from("POST"),
                String::from("PUT"),
                String::from("DELETE"),
                String::from("PATCH"),
            ],
            allow_headers: vec![],
            allow_credentials: false,
            expose_headers: vec![],
            max_age: 600,
        }
    }
}

#[pymethods]
impl CORSMiddleware {
    #[new]
    #[pyo3(signature = (
        allow_origins=vec![],
        allow_methods=vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into(), "PATCH".into()],
        allow_headers=vec![],
        allow_credentials=false,
        expose_headers=vec![],
        max_age=600,
    ))]
    fn new(
        allow_origins: Vec<String>,
        allow_methods: Vec<String>,
        allow_headers: Vec<String>,
        allow_credentials: bool,
        expose_headers: Vec<String>,
        max_age: u64,
    ) -> Self {
        Self {
            allow_origins,
            allow_methods,
            allow_headers,
            allow_credentials,
            expose_headers,
            max_age,
        }
    }
}

fn parse_and_validate_vec<T, E, F>(
    values: &[String],
    parser: F,
    kind: &str,
    err_example: &str,
    normalize_case: bool,
) -> PyResult<(Vec<T>, bool)>
where
    F: Fn(&str) -> Result<T, E>,
{
    let mut parsed_items = Vec::with_capacity(values.len());
    let mut seen_strings = HashSet::with_capacity(values.len());
    let mut has_wildcard = false;

    for v in values {
        if v.as_str() == "*" {
            has_wildcard = true;
            continue;
        }

        let normalized = if normalize_case {
            v.to_ascii_uppercase()
        } else {
            v.clone()
        };

        if !seen_strings.insert(normalized.clone()) {
            continue;
        }

        let parsed = parser(normalized.as_str()).map_err(|_| {
            PyValueError::new_err(format!(
                "Invalid CORS {kind}:\n{v}\n\nExpected a valid {kind} format like:\n{err_example}"
            ))
        })?;

        parsed_items.push(parsed);
    }

    if has_wildcard && !parsed_items.is_empty() {
        return Err(PyValueError::new_err(format!(
            "CORS configuration error: Wildcard '*' cannot be mixed with explicit values in {kind} list."
        )));
    }

    Ok((parsed_items, has_wildcard))
}

pub fn parse_cors_params(kwargs: &Bound<'_, PyDict>) -> PyResult<CORSMiddleware> {
    let mut config = CORSMiddleware::default();
    set_field!(kwargs, config, "allow_origins", allow_origins: Vec<String>);
    set_field!(kwargs, config, "allow_methods", allow_methods: Vec<String>);
    set_field!(kwargs, config, "allow_headers", allow_headers: Vec<String>);
    set_field!(kwargs, config, "allow_credentials", allow_credentials: bool);
    set_field!(kwargs, config, "expose_headers", expose_headers: Vec<String>);
    set_field!(kwargs, config, "max_age", max_age: u64);
    Ok(config)
}

pub fn build_cors_layer(config: &CORSMiddleware) -> PyResult<CorsLayer> {
    let mut layer = CorsLayer::new();

    let (origins, has_wildcard_origin) = parse_and_validate_vec(
        &config.allow_origins,
        HeaderValue::from_str,
        "origin",
        "https://example.com",
        false,
    )?;

    if config.allow_credentials && has_wildcard_origin {
        return Err(PyValueError::new_err(
            "CORS configuration error: allow_credentials=True cannot be used with allow_origins=['*'].",
        ));
    }

    if has_wildcard_origin {
        layer = layer.allow_origin(AllowOrigin::any());
    } else if !config.allow_origins.is_empty() {
        layer = layer.allow_origin(origins);
    }

    let (methods, has_wildcard_method) = parse_and_validate_vec(
        &config.allow_methods,
        |m| Method::from_str(m),
        "HTTP method",
        "GET",
        true,
    )?;

    if has_wildcard_method {
        layer = layer.allow_methods(AllowMethods::any());
    } else {
        layer = layer.allow_methods(methods);
    }

    let (headers, has_wildcard_header) = parse_and_validate_vec(
        &config.allow_headers,
        HeaderName::from_str,
        "HTTP header name",
        "Content-Type",
        false,
    )?;

    if has_wildcard_header {
        layer = layer.allow_headers(AllowHeaders::any());
    } else if !config.allow_headers.is_empty() {
        layer = layer.allow_headers(headers);
    }

    if config.allow_credentials {
        layer = layer.allow_credentials(true);
    }

    if !config.expose_headers.is_empty() {
        let (expose_headers, _) = parse_and_validate_vec(
            &config.expose_headers,
            HeaderName::from_str,
            "exposed header name",
            "X-Custom-Header",
            false,
        )?;
        layer = layer.expose_headers(expose_headers);
    }

    layer = layer.max_age(std::time::Duration::from_secs(config.max_age));
    Ok(layer)
}
