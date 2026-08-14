use cookie::Cookie;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::OnceLock;

pub type PathParams<'a> = OnceLock<SmallVec<[(Arc<str>, &'a str); 8]>>;
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

#[inline(always)]
pub fn decode_query_component(raw: &str) -> Cow<'_, str> {
    if !raw.bytes().any(|b| b == b'%' || b == b'+') {
        return Cow::Borrowed(raw);
    }

    let mut decoded = Vec::with_capacity(raw.len());
    let mut bytes = raw.as_bytes().iter().copied();

    while let Some(b) = bytes.next() {
        match b {
            b'+' => decoded.push(b' '),
            b'%' => {
                let mut peek = bytes.clone();
                if let (Some(h), Some(l)) = (peek.next(), peek.next())
                    && let Some(octet) = decode_hex_pair(h, l)
                {
                    decoded.push(octet);
                    bytes = peek;
                    continue;
                }

                decoded.push(b'%');
            }
            _ => decoded.push(b),
        }
    }

    match String::from_utf8(decoded) {
        Ok(s) => Cow::Owned(s),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

#[inline]
pub fn decode_hex_pair(h: u8, l: u8) -> Option<u8> {
    let hi = match h {
        b'0'..=b'9' => h - b'0',
        b'a'..=b'f' => h - b'a' + 10,
        b'A'..=b'F' => h - b'A' + 10,
        _ => return None,
    };
    let lo = match l {
        b'0'..=b'9' => l - b'0',
        b'a'..=b'f' => l - b'a' + 10,
        b'A'..=b'F' => l - b'A' + 10,
        _ => return None,
    };
    Some((hi << 4) | lo)
}

impl<'a> RequestInput<'a> {
    pub fn get_path_param(&self, key: &str) -> Option<&'a str> {
        self.path_params
            .get()?
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| *v)
    }

    pub fn get_all_query_params(&self) -> &SmallVec<[(Cow<'a, str>, Cow<'a, str>); 8]> {
        self.query_params.get_or_init(|| {
            if self.query_string.is_empty() {
                return SmallVec::new();
            }
            self.query_string
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    (decode_query_component(k), decode_query_component(v))
                })
                .collect()
        })
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
