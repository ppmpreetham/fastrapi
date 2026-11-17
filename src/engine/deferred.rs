use axum::response::Response;
use fastrapi_http::background::{
    AfterResponse, BackgroundJob, BackgroundRun, DeferredWork, OwnBackground, run_after_body,
};
use fastrapi_http::exceptions::is_http_exception;
use fastrapi_http::request_input::RequestInput;
use fastrapi_routing::dependencies::{
    TeardownTasks, execute_teardowns_async, execute_teardowns_sync,
};
use fastrapi_runtime::blocking;
use fastrapi_runtime::py_bridge::{self, EventLoop};
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;

pub(crate) enum Cleanup {
    Sync(TeardownTasks),
    Async(TeardownTasks, EventLoop),
    Stack(Py<PyAny>, EventLoop),
}

impl Cleanup {
    async fn run(self) -> PyResult<()> {
        match self {
            Self::Sync(teardowns) => blocking::run_python(move |py| {
                execute_teardowns_sync(py, teardowns, None);
            })
            .await
            .map_err(|err| PyRuntimeError::new_err(err.to_string())),
            Self::Async(teardowns, event_loop) => {
                execute_teardowns_async(event_loop, teardowns, None).await;
                Ok(())
            }
            Self::Stack(stack, event_loop) => {
                let exit = Python::attach(|py| {
                    let none = py.None();
                    let coroutine = stack
                        .bind(py)
                        .call_method1(intern!(py, "__aexit__"), (&none, &none, &none))?;
                    py_bridge::schedule_task(py, event_loop, coroutine)
                })?;
                exit.await.map(drop).map_err(|err| {
                    // Like Starlette: a handled exception after the response started.
                    Python::attach(|py| {
                        if is_http_exception(err.value(py).as_any()) {
                            PyRuntimeError::new_err(
                                "Caught handled exception, but response already started.",
                            )
                        } else {
                            err
                        }
                    })
                })
            }
        }
    }
}

pub(crate) struct RequestWork {
    background: Option<BackgroundRun>,
    cleanup: Option<Cleanup>,
}

impl From<BackgroundRun> for RequestWork {
    fn from(background: BackgroundRun) -> Self {
        Self {
            background: Some(background),
            cleanup: None,
        }
    }
}

impl AfterResponse for RequestWork {
    async fn run(self) -> PyResult<()> {
        if let Some(background) = self.background {
            background.run().await?;
        }
        match self.cleanup {
            Some(cleanup) => cleanup.run().await,
            None => Ok(()),
        }
    }
}

pub(crate) fn finish_request(
    event_loop: EventLoop,
    request_input: &RequestInput<'_>,
    response: Response,
    cleanup: Option<Cleanup>,
) -> Response {
    let background = match request_input.background.get() {
        Some(tasks) if response.extensions().get::<OwnBackground>().is_none() => {
            Some(BackgroundRun(
                BackgroundJob::Tasks(Python::attach(|py| tasks.clone_ref(py))),
                event_loop,
            ))
        }
        _ => response
            .extensions()
            .get::<DeferredWork<BackgroundRun>>()
            .and_then(DeferredWork::take),
    };
    defer(response, background, cleanup)
}

pub(crate) fn with_cleanup(response: Response, cleanup: Option<Cleanup>) -> Response {
    defer(response, None, cleanup)
}

fn defer(
    response: Response,
    background: Option<BackgroundRun>,
    cleanup: Option<Cleanup>,
) -> Response {
    if background.is_none() && cleanup.is_none() {
        return response;
    }
    run_after_body(
        response,
        RequestWork {
            background,
            cleanup,
        },
    )
}

pub(crate) fn take_request_work(response: &Response) -> Option<RequestWork> {
    let extensions = response.extensions();
    extensions
        .get::<DeferredWork<RequestWork>>()
        .and_then(DeferredWork::take)
        .or_else(|| {
            extensions
                .get::<DeferredWork<BackgroundRun>>()
                .and_then(DeferredWork::take)
                .map(RequestWork::from)
        })
}
