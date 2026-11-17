//! This module contains the `SessionMiddleware` class and its axum layer.
//! The session lives in a signed cookie that is compatible with the starlette `SessionMiddleware`.

use super::{AsgiApp, add_vary, asgi_middleware};
use axum::extract::{Request, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use fastrapi_pyutil::json::{json_value_to_py, write_py_json};
use hmac::{Hmac, Mac};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use sha1::{Digest, Sha1};
use smart_default::SmartDefault;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[pyclass(
    frozen,
    extends = PyDict,
    subclass,
    name = "Session",
    module = "fastrapi.middleware.sessions"
)]
#[derive(Default)]
pub struct PySession {
    accessed: AtomicBool,
    modified: AtomicBool,
}

fn dict_method<'py>(
    slf: &Bound<'py, PySession>,
    name: &str,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let call_args = PyTuple::new(
        py,
        std::iter::once(slf.as_any().clone())
            .chain(args)
            .collect::<Vec<_>>(),
    )?;
    py.get_type::<PyDict>()
        .getattr(name)?
        .call(call_args, kwargs)
}

#[pymethods]
impl PySession {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn py_new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self::default()
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        dict_method(slf, "__init__", args, kwargs).map(drop)
    }

    #[getter]
    fn accessed(&self) -> bool {
        self.accessed.load(Ordering::Relaxed)
    }

    #[setter]
    fn set_accessed(&self, value: bool) {
        self.accessed.store(value, Ordering::Relaxed);
    }

    #[getter]
    fn modified(&self) -> bool {
        self.modified.load(Ordering::Relaxed)
    }

    #[setter]
    fn set_modified(&self, value: bool) {
        self.modified.store(value, Ordering::Relaxed);
    }

    fn mark_accessed(&self) {
        self.set_accessed(true);
    }

    fn mark_modified(&self) {
        self.set_accessed(true);
        self.set_modified(true);
    }

    fn __setitem__(
        slf: &Bound<'_, Self>,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        slf.get().mark_modified();
        slf.as_super().set_item(key, value)
    }

    fn __delitem__(slf: &Bound<'_, Self>, key: &Bound<'_, PyAny>) -> PyResult<()> {
        slf.get().mark_modified();
        slf.as_super().del_item(key)
    }

    fn clear(slf: &Bound<'_, Self>) {
        slf.get().mark_modified();
        slf.as_super().clear();
    }

    #[pyo3(signature = (key, *default))]
    fn pop<'py>(
        slf: &Bound<'py, Self>,
        key: &Bound<'py, PyAny>,
        default: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if slf.as_super().contains(key)? {
            slf.get().mark_modified();
        }
        let args = PyTuple::new(
            slf.py(),
            std::iter::once(key.clone())
                .chain(default)
                .collect::<Vec<_>>(),
        )?;
        dict_method(slf, "pop", &args, None)
    }

    fn popitem<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let item = dict_method(slf, "popitem", &PyTuple::empty(slf.py()), None)?;
        slf.get().mark_modified();
        Ok(item)
    }

    #[pyo3(signature = (key, default=None))]
    fn setdefault<'py>(
        slf: &Bound<'py, Self>,
        key: &Bound<'py, PyAny>,
        default: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if !slf.as_super().contains(key)? {
            slf.get().mark_modified();
        }
        let args = PyTuple::new(
            slf.py(),
            [
                key.clone(),
                default.map_or_else(|| slf.py().None().into_bound(slf.py()), Clone::clone),
            ],
        )?;
        dict_method(slf, "setdefault", &args, None)
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn update(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        slf.get().mark_modified();
        dict_method(slf, "update", args, kwargs).map(drop)
    }

    fn __ior__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<()> {
        slf.get().mark_modified();
        dict_method(slf, "__ior__", &PyTuple::new(slf.py(), [other])?, None).map(drop)
    }
}

#[pyclass(frozen, name = "SessionMiddleware", get_all, from_py_object, eq)]
#[derive(SmartDefault, Clone, Debug, PartialEq, Eq)]
pub struct SessionMiddleware {
    pub app: AsgiApp,

    pub secret_key: String,

    #[default("session".to_string())]
    pub session_cookie: String,

    #[default(Some(1209600))]
    pub max_age: Option<i64>,

    #[default("/".to_string())]
    pub path: String,

    #[default("lax".to_string())]
    pub same_site: String,

    #[default(false)]
    pub https_only: bool,

    #[default(None)]
    pub domain: Option<String>,

    #[default(false)]
    pub partitioned: bool,
}

#[pymethods]
impl SessionMiddleware {
    #[new]
    #[pyo3(signature = (
        app=None,
        secret_key=None,
        session_cookie="session".to_string(),
        max_age=Some(1_209_600),
        path="/".to_string(),
        same_site="lax".to_string(),
        https_only=false,
        domain=None,
        partitioned=false
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the signature copies the starlette SessionMiddleware"
    )]
    fn new(
        app: Option<&Bound<'_, PyAny>>,
        secret_key: Option<&Bound<'_, PyAny>>,
        session_cookie: String,
        max_age: Option<i64>,
        path: String,
        same_site: String,
        https_only: bool,
        domain: Option<String>,
        partitioned: bool,
    ) -> PyResult<Self> {
        let secret_key = secret_key
            .ok_or_else(|| {
                PyTypeError::new_err(
                    "SessionMiddleware.__init__() missing 1 required positional argument: 'secret_key'",
                )
            })?
            .str()?
            .to_cow()?
            .into_owned();
        let config = Self {
            app: AsgiApp::new(app),
            secret_key,
            session_cookie,
            max_age,
            path,
            same_site,
            https_only,
            domain,
            partitioned,
        };
        // Starlette builds a bad `Set-Cookie` on every response; fail once, at startup, instead.
        let (attributes, cleared) = config.cookie_headers();
        for header in [format!("{}=x{attributes}", config.session_cookie), cleared] {
            HeaderValue::from_str(&header).map_err(|_| {
                PyValueError::new_err(format!(
                    "SessionMiddleware settings make an invalid Set-Cookie header: {header:?}"
                ))
            })?;
        }
        Ok(config)
    }
}

impl SessionMiddleware {
    /// The `Set-Cookie` attributes that follow the cookie value, and the whole header that clears it.
    fn cookie_headers(&self) -> (String, String) {
        let mut flags = format!("httponly; samesite={}", self.same_site);
        if self.https_only {
            flags.push_str("; secure");
        }
        if let Some(domain) = &self.domain {
            flags.push_str("; domain=");
            flags.push_str(domain);
        }
        if self.partitioned {
            flags.push_str("; partitioned");
        }
        let max_age = self
            .max_age
            .filter(|&max_age| max_age != 0)
            .map_or_else(String::new, |max_age| format!("Max-Age={max_age}; "));
        let cleared = format!(
            "{}=null; path={}; expires=Thu, 01 Jan 1970 00:00:00 GMT; {flags}",
            self.session_cookie, self.path
        );
        (format!("; path={}; {max_age}{flags}", self.path), cleared)
    }
}

asgi_middleware!(
    SessionMiddleware,
    "starlette.middleware.sessions",
    "SessionMiddleware",
    [
        "secret_key",
        "session_cookie",
        "max_age",
        "path",
        "same_site",
        "https_only",
        "domain",
        "partitioned"
    ]
);

pub fn parse_session_params(kwargs: &Bound<'_, PyDict>) -> PyResult<SessionMiddleware> {
    kwargs
        .py()
        .get_type::<SessionMiddleware>()
        .call((), Some(kwargs))?
        .extract()
        .map_err(Into::into)
}

struct TimestampSigner {
    key: [u8; 20],
}

impl TimestampSigner {
    fn new(secret_key: &str) -> Self {
        let mut digest = Sha1::new();
        digest.update(b"itsdangerous.Signer");
        digest.update(b"signer");
        digest.update(secret_key.as_bytes());
        Self {
            key: digest.finalize().into(),
        }
    }

    fn mac(&self, value: &[u8]) -> Hmac<Sha1> {
        let mut mac = Hmac::<Sha1>::new_from_slice(&self.key).expect("hmac accepts any key length");
        mac.update(value);
        mac
    }

    fn sign(&self, value: &str, now: u64) -> String {
        let timestamp = now.to_be_bytes();
        let start = (now.leading_zeros() / 8) as usize;
        let mut signed = String::with_capacity(value.len() + 40);
        signed.push_str(value);
        signed.push('.');
        URL_SAFE_NO_PAD.encode_string(&timestamp[start..], &mut signed);
        let signature = self.mac(signed.as_bytes()).finalize().into_bytes();
        signed.push('.');
        URL_SAFE_NO_PAD.encode_string(signature, &mut signed);
        signed
    }

    fn unsign<'a>(&self, signed: &'a str, max_age: Option<i64>, now: u64) -> Option<&'a str> {
        let (value, signature) = signed.rsplit_once('.')?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature.trim_end_matches('='))
            .ok()?;
        self.mac(value.as_bytes()).verify_slice(&signature).ok()?;
        let (payload, timestamp) = value.rsplit_once('.')?;
        let timestamp = URL_SAFE_NO_PAD
            .decode(timestamp.trim_end_matches('='))
            .ok()?;
        let signed_at = timestamp.iter().try_fold(0_u64, |total, byte| {
            total.checked_mul(256).map(|total| total + u64::from(*byte))
        })?;
        let fresh = max_age.is_none_or(|max_age| {
            let age = i128::from(now) - i128::from(signed_at);
            (0..=i128::from(max_age)).contains(&age)
        });
        fresh.then_some(payload)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub struct SessionPolicy {
    signer: TimestampSigner,
    cookie: String,
    max_age: Option<i64>,
    attributes: String,
    cleared: Option<HeaderValue>,
}

fn invalid_cookie() -> PyErr {
    PyValueError::new_err("the session cookie is not a valid Set-Cookie header")
}

impl SessionPolicy {
    /// `SessionMiddleware` checks at construction that these headers are valid.
    #[must_use]
    pub fn new(config: &SessionMiddleware) -> Self {
        let (attributes, cleared) = config.cookie_headers();
        Self {
            signer: TimestampSigner::new(&config.secret_key),
            cookie: config.session_cookie.clone(),
            max_age: config.max_age,
            attributes,
            cleared: HeaderValue::from_str(&cleared).ok(),
        }
    }

    fn read(&self, headers: &HeaderMap) -> Option<Vec<u8>> {
        let signed = headers
            .get_all(COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find_map(|(name, value)| (name == self.cookie).then_some(value))?;
        let payload = self.signer.unsign(signed, self.max_age, unix_now())?;
        STANDARD.decode(payload).ok()
    }

    fn write(&self, slot: &SessionSlot, headers: &mut HeaderMap) -> PyResult<()> {
        let Some(session) = slot.session.get() else {
            return Ok(());
        };
        Python::attach(|py| {
            let session = session.bind(py);
            let state = session.get();
            if state.accessed() {
                add_vary(headers, "Cookie");
            }
            if !state.modified() {
                return Ok(());
            }
            let cookie = if session.as_super().len() > 0 {
                let mut json = Vec::new();
                write_py_json(py, session.as_any(), &mut json)?;
                let signed = self.signer.sign(&STANDARD.encode(json), unix_now());
                Some(
                    HeaderValue::from_str(&format!("{}={signed}{}", self.cookie, self.attributes))
                        .map_err(|_| invalid_cookie())?,
                )
            } else if slot.initial.is_some() {
                Some(self.cleared.clone().ok_or_else(invalid_cookie)?)
            } else {
                None
            };
            if let Some(cookie) = cookie {
                headers.append(SET_COOKIE, cookie);
            }
            Ok(())
        })
    }
}

pub struct SessionSlot {
    initial: Option<Vec<u8>>,
    session: OnceLock<Py<PySession>>,
}

impl SessionSlot {
    pub fn session<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySession>> {
        if let Some(session) = self.session.get() {
            return Ok(session.bind(py).clone());
        }
        let session = Bound::new(py, PySession::default())?;
        if let Some(mut json) = self.initial.clone()
            && let Ok(value) = simd_json::to_owned_value(&mut json)
            && let Ok(decoded) = json_value_to_py(py, &value)?.cast_into::<PyDict>()
        {
            session.as_super().update(decoded.as_mapping())?;
        }
        Ok(self
            .session
            .get_or_init(|| session.unbind())
            .bind(py)
            .clone())
    }
}

#[derive(Clone)]
pub struct SessionHandle(pub Arc<SessionSlot>);

pub async fn session(
    State(policy): State<Arc<SessionPolicy>>,
    mut request: Request,
    next: Next,
) -> Response {
    let slot = Arc::new(SessionSlot {
        initial: policy.read(request.headers()),
        session: OnceLock::new(),
    });
    request
        .extensions_mut()
        .insert(SessionHandle(Arc::clone(&slot)));
    let mut response = next.run(request).await;
    if let Err(err) = policy.write(&slot, response.headers_mut()) {
        tracing::error!("session cookie could not be written: {err}");
    }
    response
}
