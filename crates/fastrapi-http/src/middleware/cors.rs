//! This module contains the `CORSMiddleware` class.
//! It reads the CORS configuration from Python keyword arguments and answers CORS requests like Starlette.

use super::add_vary;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, CONTENT_TYPE, ORIGIN, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use regex::Regex;
use smart_default::SmartDefault;
use std::collections::BTreeSet;
use std::sync::Arc;

const ALL_METHODS: [&str; 8] = [
    "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "QUERY",
];
const SAFELISTED_HEADERS: [&str; 4] = [
    "Accept",
    "Accept-Language",
    "Content-Language",
    "Content-Type",
];
const PREFLIGHT_VARY: &str = "Origin, Access-Control-Request-Method, Access-Control-Request-Headers, Access-Control-Request-Private-Network";
const REQUEST_PRIVATE_NETWORK: HeaderName =
    HeaderName::from_static("access-control-request-private-network");
const ALLOW_PRIVATE_NETWORK: HeaderName =
    HeaderName::from_static("access-control-allow-private-network");

fn default_methods() -> Vec<String> {
    vec!["GET".into()]
}

#[pyclass(frozen, name = "CORSMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct CORSMiddleware {
    pub allow_origins: Vec<String>,
    #[default(_code = "default_methods()")]
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub allow_credentials: bool,
    pub allow_origin_regex: Option<String>,
    pub allow_private_network: bool,
    pub expose_headers: Vec<String>,
    #[default(600)]
    pub max_age: u64,
}

#[pymethods]
impl CORSMiddleware {
    #[new]
    #[pyo3(
        signature = (
            _app=None,
            allow_origins=Vec::new(),
            allow_methods=default_methods(),
            allow_headers=Vec::new(),
            allow_credentials=false,
            allow_origin_regex=None,
            allow_private_network=false,
            expose_headers=Vec::new(),
            max_age=600
        ),
        text_signature = "(app=None, allow_origins=(), allow_methods=('GET',), allow_headers=(), allow_credentials=False, allow_origin_regex=None, allow_private_network=False, expose_headers=(), max_age=600)"
    )]
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    const fn new(
        _app: Option<&Bound<'_, PyAny>>,
        allow_origins: Vec<String>,
        allow_methods: Vec<String>,
        allow_headers: Vec<String>,
        allow_credentials: bool,
        allow_origin_regex: Option<String>,
        allow_private_network: bool,
        expose_headers: Vec<String>,
        max_age: u64,
    ) -> Self {
        Self {
            allow_origins,
            allow_methods,
            allow_headers,
            allow_credentials,
            allow_origin_regex,
            allow_private_network,
            expose_headers,
            max_age,
        }
    }
}

pub fn parse_cors_params(kwargs: &Bound<'_, PyDict>) -> PyResult<CORSMiddleware> {
    kwargs
        .py()
        .get_type::<CORSMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}

#[allow(clippy::struct_excessive_bools)]
pub struct CorsPolicy {
    origins: Box<[String]>,
    origin_regex: Option<Regex>,
    methods: Box<[String]>,
    headers: Box<[String]>,
    allow_all_origins: bool,
    allow_all_headers: bool,
    allow_credentials: bool,
    allow_private_network: bool,
    explicit_preflight_origin: bool,
    simple: HeaderMap,
    preflight: HeaderMap,
}

fn header_value(value: &str) -> PyResult<HeaderValue> {
    HeaderValue::from_str(value)
        .map_err(|err| PyValueError::new_err(format!("Invalid CORS header value {value:?}: {err}")))
}

impl CorsPolicy {
    pub fn new(config: &CORSMiddleware) -> PyResult<Self> {
        let wildcard = |values: &[String]| values.iter().any(|value| value == "*");
        let methods: Box<[String]> = if wildcard(&config.allow_methods) {
            ALL_METHODS.map(str::to_owned).into()
        } else {
            config.allow_methods.clone().into()
        };
        let allow_all_origins = wildcard(&config.allow_origins);
        let allow_all_headers = wildcard(&config.allow_headers);
        let explicit_preflight_origin = !allow_all_origins || config.allow_credentials;
        let origin_regex = config
            .allow_origin_regex
            .as_deref()
            .map(|pattern| Regex::new(&format!("^(?:{pattern})$")))
            .transpose()
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        let headers: BTreeSet<&str> = SAFELISTED_HEADERS
            .into_iter()
            .chain(config.allow_headers.iter().map(String::as_str))
            .collect();
        let credentials = HeaderValue::from_static("true");

        let mut simple = HeaderMap::new();
        if allow_all_origins {
            simple.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        }
        if config.allow_credentials {
            simple.insert(ACCESS_CONTROL_ALLOW_CREDENTIALS, credentials.clone());
        }
        if !config.expose_headers.is_empty() {
            simple.insert(
                ACCESS_CONTROL_EXPOSE_HEADERS,
                header_value(&config.expose_headers.join(", "))?,
            );
        }

        let mut preflight = HeaderMap::new();
        preflight.insert(VARY, HeaderValue::from_static(PREFLIGHT_VARY));
        if !explicit_preflight_origin {
            preflight.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        }
        preflight.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            header_value(&methods.join(", "))?,
        );
        preflight.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from(config.max_age));
        if !allow_all_headers {
            let joined = headers.iter().copied().collect::<Vec<_>>().join(", ");
            preflight.insert(ACCESS_CONTROL_ALLOW_HEADERS, header_value(&joined)?);
        }
        if config.allow_credentials {
            preflight.insert(ACCESS_CONTROL_ALLOW_CREDENTIALS, credentials);
        }

        Ok(Self {
            origins: config.allow_origins.clone().into(),
            origin_regex,
            methods,
            headers: headers
                .iter()
                .map(|header| header.to_ascii_lowercase())
                .collect(),
            allow_all_origins,
            allow_all_headers,
            allow_credentials: config.allow_credentials,
            allow_private_network: config.allow_private_network,
            explicit_preflight_origin,
            simple,
            preflight,
        })
    }

    fn is_allowed_origin(&self, origin: &HeaderValue) -> bool {
        self.allow_all_origins
            || origin.to_str().is_ok_and(|origin| {
                self.origin_regex
                    .as_ref()
                    .is_some_and(|regex| regex.is_match(origin))
                    || self.origins.iter().any(|allowed| allowed == origin)
            })
    }

    fn allows_headers(&self, requested: &HeaderValue) -> bool {
        requested.to_str().is_ok_and(|requested| {
            requested.split(',').all(|header| {
                self.headers
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(header.trim()))
            })
        })
    }

    fn preflight(&self, request: &HeaderMap, origin: &HeaderValue) -> Response {
        let mut headers = self.preflight.clone();
        let mut failures = Vec::new();
        if !self.is_allowed_origin(origin) {
            failures.push("origin");
        } else if self.explicit_preflight_origin {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        }
        let method = request.get(ACCESS_CONTROL_REQUEST_METHOD);
        if !method.is_some_and(|method| {
            self.methods
                .iter()
                .any(|allowed| allowed.as_bytes() == method.as_bytes())
        }) {
            failures.push("method");
        }
        match request.get(ACCESS_CONTROL_REQUEST_HEADERS) {
            Some(requested) if self.allow_all_headers => {
                headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
            }
            Some(requested) if !self.allows_headers(requested) => failures.push("headers"),
            _ => {}
        }
        if request.contains_key(REQUEST_PRIVATE_NETWORK) {
            if self.allow_private_network {
                headers.insert(ALLOW_PRIVATE_NETWORK, HeaderValue::from_static("true"));
            } else {
                failures.push("private-network");
            }
        }
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        if failures.is_empty() {
            (StatusCode::OK, headers, "OK").into_response()
        } else {
            let body = format!("Disallowed CORS {}", failures.join(", "));
            (StatusCode::BAD_REQUEST, headers, body).into_response()
        }
    }

    fn simple(&self, headers: &mut HeaderMap, origin: Option<&HeaderValue>) {
        if let Some(origin) = origin {
            for (name, value) in &self.simple {
                headers.insert(name, value.clone());
            }
            if self.allow_all_origins && self.allow_credentials
                || !self.allow_all_origins && self.is_allowed_origin(origin)
            {
                headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            }
        }
        add_vary(headers, "Origin");
    }
}

pub async fn cors(State(policy): State<Arc<CorsPolicy>>, request: Request, next: Next) -> Response {
    let origin = request.headers().get(ORIGIN).cloned();
    if let Some(origin) = &origin
        && request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        return policy.preflight(request.headers(), origin);
    }
    let mut response = next.run(request).await;
    policy.simple(response.headers_mut(), origin.as_ref());
    response
}
