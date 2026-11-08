//! The state of each running server.
//! Every server keeps its event loop, application object, lifespan state, and exception handlers in its own context.

use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::{Arc, OnceLock};

use crate::py_bridge::{EventLoop, request_loop};

pub struct ServerContext {
    pub(crate) event_loop: Py<PyAny>,
    pub(crate) bridge: Arc<pyaio_faq::Bridge>,
    app: OnceLock<Py<PyAny>>,
    exception_handlers: OnceLock<Py<PyDict>>,
    status_handlers: OnceLock<bool>,
    lifespan_state: RwLock<Option<Py<PyAny>>>,
}

impl ServerContext {
    pub(crate) const fn new(event_loop: Py<PyAny>, bridge: Arc<pyaio_faq::Bridge>) -> Self {
        Self {
            event_loop,
            bridge,
            app: OnceLock::new(),
            exception_handlers: OnceLock::new(),
            status_handlers: OnceLock::new(),
            lifespan_state: RwLock::new(None),
        }
    }

    pub fn set_app(&self, app: Py<PyAny>) {
        _ = self.app.set(app);
    }

    pub fn app(&self) -> Option<&Py<PyAny>> {
        self.app.get()
    }

    pub fn set_exception_handlers(&self, py: Python<'_>, registry: Py<PyDict>) {
        let has_status = registry
            .bind(py)
            .keys()
            .iter()
            .any(|key| key.is_instance_of::<pyo3::types::PyInt>());
        _ = self.status_handlers.set(has_status);
        _ = self.exception_handlers.set(registry);
    }

    pub fn exception_handlers(&self) -> Option<&Py<PyDict>> {
        self.exception_handlers.get()
    }

    pub fn has_status_handlers(&self) -> bool {
        self.status_handlers.get().copied().unwrap_or(false)
    }

    pub fn set_lifespan_state(&self, state: Py<PyAny>) {
        *self.lifespan_state.write() = Some(state);
    }

    pub fn clear_lifespan_state(&self) {
        *self.lifespan_state.write() = None;
    }

    pub fn lifespan_state(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.lifespan_state
            .read()
            .as_ref()
            .map(|state| state.clone_ref(py))
    }
}

static SERVER_LOOP: RwLock<Option<EventLoop>> = RwLock::new(None);

pub fn set_server_loop(event_loop: EventLoop) {
    *SERVER_LOOP.write() = Some(event_loop);
}

pub fn server_loop() -> Option<EventLoop> {
    *SERVER_LOOP.read()
}

pub fn current_server() -> Option<EventLoop> {
    request_loop().or_else(server_loop)
}
