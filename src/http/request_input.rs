use cookie::Cookie;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::OnceLock;

pub type PathParams<'a> = OnceLock<SmallVec<[(&'a str, &'a str); 8]>>;
pub type QueryParams<'a> = OnceLock<SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]>>;

pub struct RequestInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query_string: &'a str,

    pub path_params: PathParams<'a>,
    pub query_params: QueryParams<'a>,
    pub headers: &'a axum::http::HeaderMap,
    pub cookies: OnceLock<SmallVec<[(&'a str, &'a str); 8]>>,
}

impl<'a> RequestInput<'a> {
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

    pub fn get_query_param(&self, key: &str) -> Option<Cow<'a, str>> {
        self.get_all_query_params()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    pub fn get_all_cookies(&self) -> &SmallVec<[(&'a str, &'a str); 8]> {
        self.cookies.get_or_init(|| {
            self.headers
                .get_all(axum::http::header::COOKIE)
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
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    pub fn get_header(&self, key: &str) -> Option<&'a str> {
        self.headers.get(key).and_then(|v| v.to_str().ok())
    }
}
