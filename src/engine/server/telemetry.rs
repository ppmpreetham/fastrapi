//! FastAPI's OpenTelemetry instrumentation, built on the Python OpenTelemetry API so that
//! spans join the providers, exporters and traces the application already uses.
//! The layer is only added when telemetry is enabled and a provider is configured,
//! so other apps pay nothing per request.

use super::dispatch::dispatch_path;
use super::serve::AppState;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{HOST, UPGRADE};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use fastrapi_core::method::HttpMethod;
use fastrapi_core::path::{host_without_port, strip_path_convertors};
use fastrapi_core::router::RouteMatch;
use fastrapi_http::background::{AfterResponse, run_after_body};
use fastrapi_http::exceptions::UnhandledError;
use fastrapi_http::request::asgi_scope;
use fastrapi_http::request_input::RequestInput;
use fastrapi_routing::route::RouteHandler;
use fastrapi_runtime::py_bridge::{RequestContext, request_context};
use pyo3::intern;
use pyo3::prelude::*;
use std::sync::Arc;

/// FastAPI's `NativeTelemetry` for one app, created at startup when OpenTelemetry is installed.
pub(crate) struct Telemetry {
    native: Py<PyAny>,
    state: &'static AppState,
}

impl Telemetry {
    /// Returns `None` when telemetry is switched off or OpenTelemetry is not installed.
    pub(crate) fn new(
        py: Python<'_>,
        config: &Bound<'_, PyAny>,
        state: &'static AppState,
    ) -> Option<&'static Self> {
        let wanted = ["tracing", "metrics", "logs"].into_iter().any(|signal| {
            config
                .get_item(signal)
                .and_then(|value| value.is_truthy())
                .unwrap_or(false)
        });
        if !wanted {
            return None;
        }
        let native = py
            .import(intern!(py, "fastrapi.telemetry"))
            .and_then(|module| {
                module
                    .getattr(intern!(py, "NativeTelemetry"))?
                    .call1((config,))
            })
            .ok()?;
        Some(Box::leak(Box::new(Self {
            native: native.unbind(),
            state,
        })))
    }

    /// Adds OTLP exporters from the environment when `auto_configure` is set, before the lifespan starts.
    pub(crate) fn configure(&self, py: Python<'_>) {
        if let Err(err) = self.native.call_method0(py, intern!(py, "configure")) {
            err.print(py);
        }
    }

    /// Whether routes record operation spans, decided once from the providers configured at startup.
    pub(crate) fn operation_spans(&self, py: Python<'_>) -> bool {
        let enabled = |name: &Bound<'_, pyo3::types::PyString>| {
            self.native
                .call_method0(py, name)
                .and_then(|value| value.is_truthy(py))
                .unwrap_or(false)
        };
        let config = self.native.getattr(py, intern!(py, "config")).ok();
        let wanted = config
            .and_then(|config| {
                config
                    .bind(py)
                    .get_item(intern!(py, "operation_spans"))
                    .ok()
            })
            .and_then(|value| value.is_truthy().ok())
            .unwrap_or(false);
        wanted && enabled(intern!(py, "enabled"))
    }

    /// Wraps the router when a provider is configured, after the lifespan has started.
    pub(crate) fn instrument(&'static self, py: Python<'_>, router: Router) -> Router {
        let enabled = self
            .native
            .call_method0(py, intern!(py, "enabled"))
            .and_then(|enabled| enabled.is_truthy(py))
            .unwrap_or(false);
        if enabled {
            router.layer(from_fn_with_state(self, observe))
        } else {
            router
        }
    }

    /// Flushes the exporters `auto_configure` added, after the lifespan shuts down.
    pub(crate) fn flush(py: Python<'_>) {
        if let Err(err) = py
            .import(intern!(py, "fastrapi.telemetry"))
            .and_then(|module| module.call_method0(intern!(py, "flush")))
        {
            err.print(py);
        }
    }

    fn start(
        &self,
        py: Python<'_>,
        request: &Request,
        route: Option<String>,
    ) -> PyResult<Option<Exchange>> {
        let input = RequestInput::new(
            request.method(),
            request.uri(),
            request.headers(),
            request.extensions(),
            Some(self.state.event_loop),
        );
        let scope = asgi_scope(py, &input)?;
        if request.headers().contains_key(UPGRADE) {
            scope.set_item(intern!(py, "type"), intern!(py, "websocket"))?;
            scope.set_item(intern!(py, "scheme"), intern!(py, "ws"))?;
        }
        let exchange = self
            .native
            .call_method1(py, intern!(py, "start"), (scope, route))?;
        if exchange.is_none(py) {
            return Ok(None);
        }
        let context = exchange.getattr(py, intern!(py, "context"))?;
        Ok(Some(Exchange {
            exchange,
            context: RequestContext::from_context(context),
        }))
    }

    fn route(&self, request: &Request) -> Option<String> {
        let router = self.state.router.get()?;
        let method = HttpMethod::try_from(request.method()).ok()?;
        let host = router
            .has_host_routes()
            .then(|| {
                request
                    .headers()
                    .get(HOST)?
                    .to_str()
                    .ok()
                    .map(host_without_port)
            })
            .flatten();
        let path = dispatch_path(self.state, request.uri().path());
        let target = match router.resolve(method, path, host)? {
            RouteMatch::Static(target) | RouteMatch::Params(target, _) => target,
        };
        Some(strip_path_convertors(&target.pattern))
    }
}

fastrapi_pyutil::cached_py_import!(START_OPERATION, "fastrapi.telemetry", "start_operation");
fastrapi_pyutil::cached_py_import!(FINISH_OPERATION, "fastrapi.telemetry", "finish_operation");

/// A `fastapi.dependencies`, `fastapi.endpoint` or `fastapi.serialization` span, current in the
/// request's context while it is open. It does nothing on routes without operation spans.
pub(crate) struct Operation {
    name: &'static str,
    open: Option<(Py<PyAny>, Py<PyAny>)>,
}

impl Operation {
    pub(crate) fn start(py: Python<'_>, handler: &RouteHandler, name: &'static str) -> Self {
        let open = handler
            .execution
            .operation_spans
            .then(|| Self::open(py, name, handler.execution.func.bind(py)))
            .and_then(|opened| {
                opened.unwrap_or_else(|err| {
                    err.print(py);
                    None
                })
            });
        Self { name, open }
    }

    fn open(
        py: Python<'_>,
        name: &'static str,
        function: &Bound<'_, PyAny>,
    ) -> PyResult<Option<(Py<PyAny>, Py<PyAny>)>> {
        let Some(context) = request_context(py)? else {
            return Ok(None);
        };
        let operation = START_OPERATION.get(py)?.call1((&context, name, function))?;
        Ok((!operation.is_none()).then(|| (context, operation.unbind())))
    }

    pub(crate) fn finish(self, error: Option<&PyErr>) {
        let Some((context, operation)) = self.open else {
            return;
        };
        Python::attach(|py| {
            let error = error.map(|err| err.value(py).clone());
            if let Err(err) = FINISH_OPERATION
                .get(py)
                .and_then(|finish| finish.call1((context, self.name, operation, error)))
            {
                err.print(py);
            }
        });
    }
}

struct Exchange {
    exchange: Py<PyAny>,
    context: RequestContext,
}

struct Finished {
    exchange: Py<PyAny>,
    status: u16,
    error: Option<Arc<PyErr>>,
}

impl AfterResponse for Finished {
    fn run(self) -> impl Future<Output = PyResult<()>> + Send {
        std::future::ready(Python::attach(|py| {
            let error = self.error.as_deref().map(|err| err.value(py).clone());
            self.exchange
                .call_method1(py, intern!(py, "finish"), (self.status, error))
                .map(drop)
        }))
    }
}

async fn observe(
    State(telemetry): State<&'static Telemetry>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = telemetry.route(&request);
    let started = Python::attach(|py| {
        telemetry.start(py, &request, route).unwrap_or_else(|err| {
            err.print(py);
            None
        })
    });
    let Some(Exchange { exchange, context }) = started else {
        return next.run(request).await;
    };
    request.extensions_mut().insert(context);
    let response = next.run(request).await;
    let finished = Finished {
        exchange,
        status: response.status().as_u16(),
        error: response
            .extensions()
            .get::<UnhandledError>()
            .map(|UnhandledError(err)| Arc::clone(err)),
    };
    run_after_body(response, finished)
}
