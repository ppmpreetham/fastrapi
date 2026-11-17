use super::asgi_websocket::AsgiSocket;
use super::lifecycle::{run_shutdown_phase, run_startup_phase, stop_background_asyncio_loop};
use super::serve::{PreparedApp, prepare_app};
use super::telemetry::Telemetry;
use super::websocket::WebSocketOutcome;
use crate::engine::config::FastrAPI;
use crate::engine::deferred::take_request_work;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, Version};
use fastrapi_http::background::AfterResponse;
use fastrapi_http::exceptions::UnhandledError;
use fastrapi_http::middleware::asgi::{
    ResponseMessages, asgi_call, drain_receive, payload_too_large, send_response, send_response_as,
};
use fastrapi_http::request_input::{AsgiMount, NamedClient};
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge::{self, TaskLocals};
use pyo3::exceptions::{PyAssertionError, PyKeyError, PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBytes, PyDict, PyString};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;

/// The ASGI app of a `FastrAPI` instance, built on its first ASGI call.
///
/// The cell sits behind an `Arc` so it can be initialized without holding a
/// borrow of the app (`prepare_app` borrows it mutably), and a reset swaps in a
/// fresh cell. `PyOnceLock` runs `prepare_app` once even when two threads make
/// the first call at the same time, and detaches while it waits.
pub(crate) type AsgiCell = Arc<PyOnceLock<Arc<AsgiApp>>>;

pub(crate) fn new_asgi_cell() -> AsgiCell {
    Arc::new(PyOnceLock::new())
}

pub(crate) struct AsgiApp {
    app: Py<FastrAPI>,
    prepared: PreparedApp,
    router: OnceLock<Router>,
    max_body_size: usize,
}

impl AsgiApp {
    /// The router, with telemetry added on the first request when a provider is configured.
    fn router(&self) -> Router {
        self.router
            .get_or_init(|| match self.prepared.telemetry {
                Some(telemetry) => {
                    Python::attach(|py| telemetry.instrument(py, self.prepared.router.clone()))
                }
                None => self.prepared.router.clone(),
            })
            .clone()
    }
}

/// Stops the background asyncio loop of an app that a reset of the
/// middleware stack (or the drop of the `FastrAPI` instance) discarded, once
/// its last in-flight request has finished.
impl Drop for AsgiApp {
    fn drop(&mut self) {
        let event_loop = self.prepared.event_loop;
        _ = Python::try_attach(|py| stop_background_asyncio_loop(py, event_loop));
    }
}

fn asgi_app(slf: &Bound<'_, FastrAPI>) -> PyResult<Arc<AsgiApp>> {
    let py = slf.py();
    let cell = Arc::clone(&slf.borrow().asgi);
    if let Some(app) = cell.get(py) {
        return Ok(Arc::clone(app));
    }
    cell.get_or_try_init(py, || {
        let app = slf.clone().unbind();
        let prepared = prepare_app(py, &app)?;
        let max_body_size = slf.borrow().max_body_size.unwrap_or(usize::MAX);
        Ok(Arc::new(AsgiApp {
            app,
            prepared,
            router: OnceLock::new(),
            max_body_size,
        }))
    })
    .map(Arc::clone)
}

pub(crate) fn call<'py>(
    slf: &Bound<'py, FastrAPI>,
    scope: &Bound<'py, PyDict>,
    receive: Bound<'py, PyAny>,
    send: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let asgi = asgi_app(slf)?;
    let locals = py_bridge::get_current_locals(py)?;
    let receive = receive.unbind();
    let send = send.unbind();
    let kind = scope
        .get_item(intern!(py, "type"))?
        .ok_or_else(|| PyKeyError::new_err("type"))?;
    match kind.cast::<PyString>()?.to_str()? {
        "http" => {
            let request = http_request(scope)?;
            py_bridge::future_into_py_with_locals(
                py,
                locals.clone(),
                http(asgi, locals, request, receive, send),
            )
        }
        "websocket" => {
            let request = http_request(scope)?;
            let denials = supports_denial_response(scope)?;
            py_bridge::future_into_py_with_locals(
                py,
                locals.clone(),
                websocket(
                    asgi,
                    AsgiSocket::new(locals, receive, send),
                    request,
                    denials,
                ),
            )
        }
        "lifespan" => py_bridge::future_into_py_with_locals(
            py,
            locals.clone(),
            lifespan(asgi, locals, receive, send),
        ),
        _ => Err(PyAssertionError::new_err(())),
    }
}

fn http_request<'py>(scope: &Bound<'py, PyDict>) -> PyResult<Request<Body>> {
    let py = scope.py();
    let item = |key: &Bound<'py, PyString>| -> PyResult<Option<Bound<'py, PyAny>>> {
        Ok(scope.get_item(key)?.filter(|value| !value.is_none()))
    };
    let path = match item(intern!(py, "raw_path"))? {
        Some(raw) => raw.cast::<PyBytes>()?.as_bytes().to_vec(),
        None => item(intern!(py, "path"))?
            .ok_or_else(|| PyValueError::new_err("ASGI scope has no path"))?
            .extract::<String>()?
            .into_bytes(),
    };
    let root_path: String =
        item(intern!(py, "root_path"))?.map_or(Ok(String::new()), |root| root.extract())?;
    let mut target = route_path(&path, root_path.as_bytes()).to_vec();
    if let Some(query) = item(intern!(py, "query_string"))?
        .map(Bound::cast_into::<PyBytes>)
        .transpose()?
        .filter(|query| !query.as_bytes().is_empty())
    {
        target.push(b'?');
        target.extend_from_slice(query.as_bytes());
    }
    let method: String =
        item(intern!(py, "method"))?.map_or(Ok("GET".to_owned()), |method| method.extract())?;
    let version = match item(intern!(py, "http_version"))?
        .map(|version| version.extract::<String>())
        .transpose()?
        .as_deref()
    {
        Some("1.0") => Version::HTTP_10,
        Some("2" | "2.0") => Version::HTTP_2,
        _ => Version::HTTP_11,
    };
    let mut builder = Request::builder()
        .method(method.as_str())
        .uri(target)
        .version(version);
    if let Some(headers) = item(intern!(py, "headers"))? {
        for pair in headers.try_iter()? {
            let (name, value) = pair?.extract::<(Bound<'py, PyBytes>, Bound<'py, PyBytes>)>()?;
            builder = builder.header(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|err| PyValueError::new_err(err.to_string()))?,
                HeaderValue::from_bytes(value.as_bytes())
                    .map_err(|err| PyValueError::new_err(err.to_string()))?,
            );
        }
    }
    if let Some((host, port)) =
        item(intern!(py, "client"))?.and_then(|client| client.extract::<(String, u16)>().ok())
    {
        builder = builder.extension(NamedClient(host.into_boxed_str(), port));
    }
    if !root_path.is_empty() {
        builder = builder.extension(AsgiMount {
            root_path: root_path.into_boxed_str(),
            path: String::from_utf8_lossy(&path).into(),
            app_root_path: item(intern!(py, "app_root_path"))?.map(Bound::unbind),
            router: item(intern!(py, "router"))?.map(Bound::unbind),
        });
    }
    builder
        .body(Body::empty())
        .map_err(|err| PyValueError::new_err(err.to_string()))
}

fn route_path<'a>(path: &'a [u8], root_path: &[u8]) -> &'a [u8] {
    match path.strip_prefix(root_path) {
        Some([]) if !root_path.is_empty() => b"/",
        Some(rest) if !root_path.is_empty() && rest.first() == Some(&b'/') => rest,
        _ => path,
    }
}

async fn http(
    asgi: Arc<AsgiApp>,
    locals: TaskLocals,
    mut request: Request<Body>,
    receive: Py<PyAny>,
    send: Py<PyAny>,
) -> PyResult<()> {
    let Some(body) = drain_receive(&locals, &receive, asgi.max_body_size).await? else {
        return send_response(&locals, &send, payload_too_large()).await;
    };
    *request.body_mut() = Body::from(body);
    let mut response = match asgi.router().oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let unhandled = response.extensions_mut().remove::<UnhandledError>();
    let deferred = take_request_work(&response);
    send_response(&locals, &send, response).await?;
    if let Some(work) = deferred {
        work.run().await?;
    }
    unhandled.map_or(Ok(()), |UnhandledError(err)| {
        Err(Python::attach(|py| err.clone_ref(py)))
    })
}

fn supports_denial_response(scope: &Bound<'_, PyDict>) -> PyResult<bool> {
    let py = scope.py();
    Ok(scope
        .get_item(intern!(py, "extensions"))?
        .and_then(|extensions| extensions.cast_into::<PyDict>().ok())
        .is_some_and(|extensions| {
            extensions
                .contains(intern!(py, "websocket.http.response"))
                .unwrap_or(false)
        }))
}

async fn websocket(
    asgi: Arc<AsgiApp>,
    socket: AsgiSocket,
    mut request: Request<Body>,
    denials: bool,
) -> PyResult<()> {
    socket.receive().await?;
    request.extensions_mut().insert(socket.clone());
    let mut response = match asgi.router().oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    match response.extensions_mut().remove::<WebSocketOutcome>() {
        Some(WebSocketOutcome::Handled) => Ok(()),
        Some(WebSocketOutcome::Denied) if denials => {
            send_response_as(
                socket.locals(),
                socket.send_callable(),
                response,
                ResponseMessages::WebSocketDenial,
            )
            .await
        }
        Some(WebSocketOutcome::Close(code, reason)) => socket.close(code, &reason).await,
        _ => socket.close(1000, "").await,
    }
}

async fn lifespan(
    asgi: Arc<AsgiApp>,
    locals: TaskLocals,
    receive: Py<PyAny>,
    send: Py<PyAny>,
) -> PyResult<()> {
    let mut entered = None;
    loop {
        match message_type(&locals, &receive).await?.as_str() {
            "lifespan.startup" => {
                let app = asgi.clone();
                let started = run_python(move |py| {
                    if let Some(telemetry) = app.prepared.telemetry {
                        telemetry.configure(py);
                    }
                    let hooks = &app.prepared.hooks;
                    run_startup_phase(
                        py,
                        &app.app,
                        hooks
                            .lifespan
                            .as_ref()
                            .map(|lifespan| lifespan.clone_ref(py)),
                        hooks
                            .on_startup
                            .as_ref()
                            .map(|handlers| handlers.clone_ref(py)),
                        app.prepared.event_loop,
                    )
                })
                .await
                .map_err(|panic| PyRuntimeError::new_err(panic.to_string()))?;
                match started {
                    Ok(lifespan) => {
                        entered = lifespan;
                        reply(&locals, &send, "lifespan.startup.complete", None).await?;
                    }
                    Err(err) => {
                        reply(&locals, &send, "lifespan.startup.failed", Some(&err)).await?;
                        return Err(err);
                    }
                }
            }
            "lifespan.shutdown" => {
                let app = asgi.clone();
                let entered = entered.take();
                let stopped = run_python(move |py| {
                    run_shutdown_phase(
                        py,
                        entered,
                        app.prepared
                            .hooks
                            .on_shutdown
                            .as_ref()
                            .map(|handlers| handlers.clone_ref(py)),
                        app.prepared.event_loop,
                    )
                })
                .await
                .map_err(|panic| PyRuntimeError::new_err(panic.to_string()))?;
                if asgi.prepared.telemetry.is_some() {
                    Python::attach(Telemetry::flush);
                }
                return match stopped {
                    Ok(()) => reply(&locals, &send, "lifespan.shutdown.complete", None).await,
                    Err(err) => {
                        reply(&locals, &send, "lifespan.shutdown.failed", Some(&err)).await?;
                        Err(err)
                    }
                };
            }
            _ => return Ok(()),
        }
    }
}

async fn message_type(locals: &TaskLocals, receive: &Py<PyAny>) -> PyResult<String> {
    let message =
        Python::attach(|py| py_bridge::into_future_with_locals(locals, receive.bind(py).call0()?))?
            .await?;
    Python::attach(|py| message.bind(py).get_item(intern!(py, "type"))?.extract())
}

async fn reply(
    locals: &TaskLocals,
    send: &Py<PyAny>,
    kind: &str,
    error: Option<&PyErr>,
) -> PyResult<()> {
    let message = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let message = PyDict::new(py);
        message.set_item(intern!(py, "type"), kind)?;
        if let Some(error) = error {
            message.set_item(intern!(py, "message"), error.value(py).str()?)?;
        }
        Ok(message.into_any().unbind())
    })?;
    asgi_call(locals, send, message).await
}
