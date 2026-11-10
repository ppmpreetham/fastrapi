use super::lifecycle::{run_shutdown_phase, run_startup_phase};
use super::serve::{PreparedApp, prepare_app};
use crate::engine::config::FastrAPI;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Request, Version};
use fastrapi_http::middleware::asgi::{asgi_call, drain_receive, send_response};
use fastrapi_runtime::blocking::run_python;
use fastrapi_runtime::py_bridge::{self, TaskLocals};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

pub(crate) struct AsgiApp {
    app: Py<FastrAPI>,
    prepared: PreparedApp,
}

fn asgi_app(slf: &Bound<'_, FastrAPI>) -> PyResult<Arc<AsgiApp>> {
    if let Some(app) = slf.borrow().asgi.get() {
        return Ok(app.clone());
    }
    let app = slf.clone().unbind();
    let prepared = prepare_app(slf.py(), &app)?;
    let built = Arc::new(AsgiApp { app, prepared });
    Ok(slf.borrow().asgi.get_or_init(|| built).clone())
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
        .ok_or_else(|| PyValueError::new_err("ASGI scope has no type"))?;
    match kind.cast::<PyString>()?.to_str()? {
        "http" => {
            let request = http_request(scope)?;
            py_bridge::future_into_py_with_locals(
                py,
                locals.clone(),
                http(asgi, locals, request, receive, send),
            )
        }
        "lifespan" => py_bridge::future_into_py_with_locals(
            py,
            locals.clone(),
            lifespan(asgi, locals, receive, send),
        ),
        other => Err(PyRuntimeError::new_err(format!(
            "unsupported ASGI scope type: {other}"
        ))),
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
    let root_path: String = item(intern!(py, "root_path"))?
        .map_or(Ok(String::new()), |root| root.extract())?;
    let mut target = route_path(&path, root_path.as_bytes()).to_vec();
    if let Some(query) = item(intern!(py, "query_string"))?
        .map(Bound::cast_into::<PyBytes>)
        .transpose()?
        .filter(|query| !query.as_bytes().is_empty())
    {
        target.push(b'?');
        target.extend_from_slice(query.as_bytes());
    }
    let method: String = item(intern!(py, "method"))?
        .map_or(Ok("GET".to_owned()), |method| method.extract())?;
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
    if let Some(client) = item(intern!(py, "client"))?
        .and_then(|client| client.extract::<(String, u16)>().ok())
        .and_then(|(host, port)| Some(SocketAddr::new(host.parse().ok()?, port)))
    {
        builder = builder.extension(ConnectInfo(client));
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
    *request.body_mut() = Body::from(drain_receive(&locals, &receive).await?);
    let response = match asgi.prepared.router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    send_response(&locals, &send, response).await
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
                    let hooks = &app.prepared.hooks;
                    run_startup_phase(
                        py,
                        &app.app,
                        hooks.lifespan.as_ref().map(|lifespan| lifespan.clone_ref(py)),
                        hooks.on_startup.as_ref().map(|handlers| handlers.clone_ref(py)),
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
    let message = Python::attach(|py| {
        py_bridge::into_future_with_locals(locals, receive.bind(py).call0()?)
    })?
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
