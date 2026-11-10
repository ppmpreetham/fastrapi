//! Generator endpoints: the factory that wraps the generator in a response, and the item encoding
//! the response stream applies to each yielded item.

use crate::responses::PyStreamingResponse;
use crate::sse::{format_sse_event, json_dumps};
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use pyo3::{intern, Bound};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamMode {
    Jsonl,
    Sse,
    Raw,
}

#[pyclass(frozen, name = "_StreamFactory", module = "fastrapi.responses")]
pub struct StreamFactory {
    mode: StreamMode,
    adapter: Option<Py<PyAny>>,
    response_class: Option<Py<PyAny>>,
    options: Option<Py<PyDict>>,
}

#[pyclass(frozen, name = "_StreamItems", module = "fastrapi.responses")]
pub struct StreamItems {
    pub content: Py<PyAny>,
    sse: bool,
    adapter: Option<Py<PyAny>>,
    options: Option<Py<PyDict>>,
}

pub fn stream_factory(
    py: Python<'_>,
    mode: &str,
    item_type: Option<&Bound<'_, PyAny>>,
    response_class: Option<&Bound<'_, PyAny>>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    let mode = match mode {
        "jsonl" => StreamMode::Jsonl,
        "sse" => StreamMode::Sse,
        _ => StreamMode::Raw,
    };
    let adapter = item_type
        .map(|item_type| {
            py.import(intern!(py, "pydantic"))?
                .getattr(intern!(py, "TypeAdapter"))?
                .call1((item_type,))
                .map(Bound::unbind)
        })
        .transpose()?;
    let options = options
        .map(|options| options.copy().map(Bound::unbind))
        .transpose()?;
    let factory = StreamFactory {
        mode,
        adapter,
        response_class: response_class.map(|class| class.clone().unbind()),
        options,
    };
    Ok(Bound::new(py, factory)?.into_any().unbind())
}

#[pymethods]
impl StreamFactory {
    fn __call__<'py>(&self, py: Python<'py>, generator: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let response_class = || {
            self.response_class
                .as_ref()
                .map_or_else(|| py.get_type::<PyStreamingResponse>().into_any(), |class| class.bind(py).clone())
        };
        if self.mode == StreamMode::Raw {
            return response_class().call1((generator,));
        }
        let items = Bound::new(
            py,
            StreamItems {
                content: generator.unbind(),
                sse: self.mode == StreamMode::Sse,
                adapter: self.adapter.as_ref().map(|adapter| adapter.clone_ref(py)),
                options: self.options.as_ref().map(|options| options.clone_ref(py)),
            },
        )?;
        let kwargs = PyDict::new(py);
        if self.mode == StreamMode::Jsonl {
            kwargs.set_item(intern!(py, "media_type"), "application/jsonl")?;
            return py.get_type::<PyStreamingResponse>().call((items,), Some(&kwargs));
        }
        let headers = PyDict::new(py);
        headers.set_item("cache-control", "no-cache")?;
        headers.set_item("x-accel-buffering", "no")?;
        kwargs.set_item(intern!(py, "headers"), headers)?;
        response_class().call((items,), Some(&kwargs))
    }
}

impl StreamItems {
    fn dump_json<'py>(
        &self,
        py: Python<'py>,
        adapter: &Bound<'py, PyAny>,
        item: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let kwargs = PyDict::new(py);
        kwargs.set_item(intern!(py, "from_attributes"), true)?;
        let validated = match adapter.call_method(intern!(py, "validate_python"), (item,), Some(&kwargs)) {
            Ok(validated) => validated,
            Err(_) => crate::response_convert::validate_stream_item(py, adapter, item)?,
        };
        adapter
            .call_method(
                intern!(py, "dump_json"),
                (validated,),
                self.options.as_ref().map(|options| options.bind(py)),
            )?
            .cast_into::<PyBytes>()
            .map_err(PyErr::from)
    }

    pub fn encode(&self, py: Python<'_>, item: &Bound<'_, PyAny>, sep: &str) -> PyResult<Bytes> {
        if self.sse {
            if crate::sse::event_fields(py, item).is_some() {
                return crate::response_convert::sse_event_chunk(py, item, sep);
            }
            let data = match &self.adapter {
                Some(adapter) => {
                    let json = self.dump_json(py, adapter.bind(py), item)?;
                    String::from_utf8_lossy(json.as_bytes()).into_owned()
                }
                None => json_dumps(item)?,
            };
            return Ok(format_sse_event(Some(&data), None, None, None, None, sep));
        }
        let mut line = match &self.adapter {
            Some(adapter) => self.dump_json(py, adapter.bind(py), item)?.as_bytes().to_vec(),
            None => json_dumps(item)?.into_bytes(),
        };
        line.push(b'\n');
        Ok(Bytes::from(line))
    }
}
