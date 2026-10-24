use parking_lot::RwLock;
use pyo3::types::PyDict;
use std::sync::OnceLock;

use crate::py_bridge::EventLoop;

static SERVER_LOOP: RwLock<Option<EventLoop>> = RwLock::new(None);

pub fn set_server_loop(event_loop: EventLoop) {
    *SERVER_LOOP.write() = Some(event_loop);
}

pub fn server_loop() -> Option<EventLoop> {
    *SERVER_LOOP.read()
}

static SERVE_APP: OnceLock<pyo3::Py<pyo3::PyAny>> = OnceLock::new();
static LIFESPAN_STATE: RwLock<Option<pyo3::Py<pyo3::PyAny>>> = RwLock::new(None);

pub fn set_serve_app(app: pyo3::Py<pyo3::PyAny>) {
    _ = SERVE_APP.set(app);
}

pub fn serve_app() -> Option<&'static pyo3::Py<pyo3::PyAny>> {
    SERVE_APP.get()
}

pub fn set_lifespan_state(state: pyo3::Py<pyo3::PyAny>) {
    *LIFESPAN_STATE.write() = Some(state);
}

pub fn clear_lifespan_state() {
    *LIFESPAN_STATE.write() = None;
}

pub fn lifespan_state(py: pyo3::Python<'_>) -> Option<pyo3::Py<pyo3::PyAny>> {
    LIFESPAN_STATE
        .read()
        .as_ref()
        .map(|state| state.clone_ref(py))
}

static EXCEPTION_HANDLERS: RwLock<Option<pyo3::Py<PyDict>>> = RwLock::new(None);

pub fn set_exception_handlers(registry: pyo3::Py<PyDict>) {
    *EXCEPTION_HANDLERS.write() = Some(registry);
}

pub fn exception_handlers() -> Option<pyo3::Py<PyDict>> {
    EXCEPTION_HANDLERS.read().clone()
}
