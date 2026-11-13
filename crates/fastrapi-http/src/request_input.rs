//! This module contains `RequestInput`, a view on the data of one request.
//! It gives the path parameters, query parameters, cookies, and headers.
//! It parses the query parameters and cookies only when the code reads them.

use crate::background::PyBackgroundTasks;
use axum::extract::ConnectInfo;
use axum::http::header::COOKIE;
use axum::http::{Extensions, HeaderMap, HeaderName};
use cookie::Cookie;
use fastrapi_runtime::py_bridge::EventLoop;
use pyo3::prelude::*;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct NamedClient(pub Box<str>, pub u16);

#[derive(Clone)]
pub struct AsgiMount {
    pub root_path: Box<str>,
    pub path: Box<str>,
    pub app_root_path: Option<Py<PyAny>>,
    pub router: Option<Py<PyAny>>,
}

pub type PathParams<'a> = OnceLock<SmallVec<[(&'a str, &'a str); 8]>>;
pub type QueryParams<'a> = OnceLock<SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]>>;

pub struct RequestInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query_string: &'a str,

    pub path_params: PathParams<'a>,
    pub query_params: QueryParams<'a>,
    pub headers: &'a HeaderMap,
    pub extensions: &'a Extensions,
    pub cookies: OnceLock<SmallVec<[(&'a str, &'a str); 8]>>,
    pub server: Option<EventLoop>,
    pub background: OnceLock<Py<PyBackgroundTasks>>,
}

impl<'a> RequestInput<'a> {
    #[must_use]
    pub fn new(
        method: &'a axum::http::Method,
        uri: &'a axum::http::Uri,
        headers: &'a HeaderMap,
        extensions: &'a Extensions,
        server: Option<EventLoop>,
    ) -> Self {
        Self {
            method: method.as_str(),
            path: uri.path(),
            query_string: uri.query().unwrap_or(""),
            headers,
            extensions,
            path_params: OnceLock::new(),
            query_params: OnceLock::new(),
            cookies: OnceLock::new(),
            server,
            background: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn client(&self) -> Option<(Cow<'a, str>, u16)> {
        if let Some(NamedClient(host, port)) = self.extensions.get::<NamedClient>() {
            return Some((Cow::Borrowed(host), *port));
        }
        self.extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| (Cow::Owned(addr.ip().to_string()), addr.port()))
    }

    pub fn background_tasks(&self, py: Python<'_>) -> PyResult<Py<PyBackgroundTasks>> {
        if let Some(tasks) = self.background.get() {
            return Ok(tasks.clone_ref(py));
        }
        let created = Py::new(py, PyBackgroundTasks::new())?;
        Ok(self.background.get_or_init(|| created).clone_ref(py))
    }

    pub fn get_path_param(&self, key: &str) -> Option<&'a str> {
        self.path_params
            .get()?
            .iter()
            .find(|(k, _)| **k == *key)
            .map(|(_, v)| *v)
    }

    pub fn get_all_query_params(&self) -> &SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]> {
        self.query_params
            .get_or_init(|| form_urlencoded::parse(self.query_string.as_bytes()).collect())
    }

    pub fn get_query_param(&self, key: &str) -> Option<&str> {
        self.get_all_query_params()
            .iter()
            .rfind(|(k, _)| k == key)
            .map(|(_, v)| &**v)
    }

    pub fn get_all_cookies(&self) -> &SmallVec<[(&'a str, &'a str); 8]> {
        self.cookies.get_or_init(|| {
            self.headers
                .get_all(COOKIE)
                .iter()
                .filter_map(|header_value| header_value.to_str().ok())
                .flat_map(Cookie::split_parse)
                .filter_map(Result::ok)
                .filter_map(|cookie| Some((cookie.name_raw()?, cookie.value_raw()?)))
                .collect()
        })
    }

    pub fn get_cookie(&self, key: &str) -> Option<&'a str> {
        self.get_all_cookies()
            .iter()
            .rfind(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    pub fn get_header(&self, key: &str) -> Option<&'a str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }

    pub fn get_header_by_name(&self, key: &HeaderName) -> Option<&'a str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }

    pub fn get_header_values<'s>(&'s self, key: &str) -> SmallVec<[Cow<'s, str>; 4]> {
        self.headers
            .get_all(key)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(Cow::Borrowed)
            .collect()
    }
}
