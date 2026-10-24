use pyo3::exceptions::{PyImportError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

#[pyclass(name = "Jinja2Templates", module = "fastrapi.templating")]
pub struct PyJinja2Templates {
    inner: Py<PyAny>,
}

#[pymethods]
impl PyJinja2Templates {
    #[new]
    #[pyo3(signature = (directory=None, *, context_processors=None, env=None, **kwargs))]
    fn new(
        py: Python<'_>,
        directory: Option<Py<PyAny>>,
        context_processors: Option<Py<PyAny>>,
        env: Option<Py<PyAny>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let module = py
            .import("starlette.templating")
            .map_err(|_| PyImportError::new_err("jinja2 is required to use Jinja2Templates"))?;
        let cls = module.getattr("Jinja2Templates")?;
        let kwargs_out = kwargs.cloned().unwrap_or_else(|| PyDict::new(py));
        if let Some(value) = context_processors {
            kwargs_out.set_item("context_processors", value)?;
        }
        if let Some(value) = env {
            kwargs_out.set_item("env", value)?;
        }
        let inner = match directory {
            Some(value) => cls.call((value,), Some(&kwargs_out))?,
            None => cls.call0()?,
        };
        Ok(Self {
            inner: inner.unbind(),
        })
    }

    fn __getattr__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        Ok(self.inner.bind(py).getattr(name)?.unbind())
    }

    #[pyo3(signature = (request_or_name, name_or_context=None, context=None, status_code=200, headers=None, media_type=None, background=None))]
    #[pyo3(name = "TemplateResponse")]
    fn template_response(
        &self,
        py: Python<'_>,
        request_or_name: Bound<'_, PyAny>,
        name_or_context: Option<Bound<'_, PyAny>>,
        context: Option<Bound<'_, PyDict>>,
        status_code: u16,
        headers: Option<Bound<'_, PyDict>>,
        media_type: Option<String>,
        background: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("status_code", status_code)?;
        if let Some(value) = headers {
            kwargs.set_item("headers", value)?;
        }
        if let Some(value) = media_type {
            kwargs.set_item("media_type", value)?;
        }
        if let Some(value) = background {
            kwargs.set_item("background", value)?;
        }
        let (name, context) = if let Ok(name) = request_or_name.extract::<&str>() {
            (
                name.to_owned(),
                name_or_context.and_then(|value| value.cast::<PyDict>().ok().cloned()),
            )
        } else {
            let name = name_or_context
                .ok_or_else(|| PyTypeError::new_err("missing template name"))?
                .extract::<String>()?;
            let context = context.unwrap_or_else(|| PyDict::new(py));
            context.set_item("request", request_or_name)?;
            (name, Some(context))
        };
        let context = context.unwrap_or_else(|| PyDict::new(py));
        Ok(self
            .inner
            .bind(py)
            .call_method("TemplateResponse", (name, context), Some(&kwargs))?
            .unbind())
    }
}
