use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};

// --- Utilities ---

/// from route patterns like "/users/{user_id}"
pub fn extract_path_param_names(path: &str) -> Vec<String> {
    let mut params = Vec::new();
    let mut in_param = false;
    let mut current_param = String::new();
    for c in path.chars() {
        match c {
            '{' => {
                in_param = true;
                current_param.clear();
            }
            '}' => {
                if in_param && !current_param.is_empty() {
                    params.push(current_param.clone());
                }
                in_param = false;
            }
            _ => {
                if in_param {
                    current_param.push(c);
                }
            }
        }
    }
    params
}

// --- Sentinels ---

#[pyclass(name = "_Unset")]
#[derive(Clone)]
pub struct Unset;

#[pymethods]
impl Unset {
    #[new]
    fn new() -> Self {
        Self
    }
}

#[pyclass(name = "Undefined")]
#[derive(Clone)]
pub struct Undefined;

#[pymethods]
impl Undefined {
    #[new]
    fn new() -> Self {
        Self
    }
}

// --- Dependency Classes ---

#[pyclass(name = "Depends", subclass)]
#[derive(Clone)]
pub struct PyDepends {
    #[pyo3(get)]
    pub dependency: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub use_cache: bool,
}

#[pymethods]
impl PyDepends {
    #[new]
    #[pyo3(signature = (dependency=None, *, use_cache=true))]
    pub fn new(dependency: Option<Py<PyAny>>, use_cache: bool) -> Self {
        Self {
            dependency,
            use_cache,
        }
    }
}

#[pyclass(name = "Security", extends = PyDepends)]
#[derive(Clone)]
pub struct PySecurity {
    #[pyo3(get)]
    pub scopes: Vec<String>,
}

#[pymethods]
impl PySecurity {
    #[new]
    #[pyo3(signature = (dependency=None, *, scopes=None, use_cache=true))]
    fn new(
        dependency: Option<Py<PyAny>>,
        scopes: Option<Vec<String>>,
        use_cache: bool,
    ) -> (Self, PyDepends) {
        (
            Self {
                scopes: scopes.unwrap_or_default(),
            },
            PyDepends::new(dependency, use_cache),
        )
    }
}

// --- Parameter Classes ---

// Query
#[pyclass(name = "Query")]
#[derive(Clone)]
pub struct PyQuery {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyQuery {
    #[new]
    #[pyo3(signature = (default=None, *, alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

// Path
#[pyclass(name = "Path")]
#[derive(Clone)]
pub struct PyPath {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyPath {
    #[new]
    #[pyo3(signature = (default=None, *, alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

// Body (embed, media_type)
#[pyclass(name = "Body")]
#[derive(Clone)]
pub struct PyBody {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub embed: Option<bool>,
    #[pyo3(get)]
    pub media_type: String,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyBody {
    #[new]
    #[pyo3(signature = (default=None, *, embed=None, media_type="application/json".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        embed: Option<bool>,
        media_type: String,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            embed,
            media_type,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

// Cookie
#[pyclass(name = "Cookie")]
#[derive(Clone)]
pub struct PyCookie {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyCookie {
    #[new]
    #[pyo3(signature = (default=None, *, alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

#[pyclass(name = "Header")]
#[derive(Clone)]
pub struct PyHeader {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub convert_underscores: bool,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyHeader {
    #[new]
    #[pyo3(signature = (default=None, *, alias=None, convert_underscores=true, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        alias: Option<String>,
        convert_underscores: bool,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            alias,
            convert_underscores,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

// Form (media_type)
#[pyclass(name = "Form")]
#[derive(Clone)]
pub struct PyForm {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub media_type: String,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyForm {
    #[new]
    #[pyo3(signature = (default=None, *, media_type="application/x-www-form-urlencoded".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        media_type: String,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            media_type,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

// File (file type)
#[pyclass(name = "File")]
#[derive(Clone)]
pub struct PyFile {
    #[pyo3(get)]
    pub default: Option<Py<PyAny>>,
    #[pyo3(get)]
    pub media_type: String,
    #[pyo3(get)]
    pub alias: Option<String>,
    #[pyo3(get)]
    pub title: Option<String>,
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub gt: Option<f64>,
    #[pyo3(get)]
    pub ge: Option<f64>,
    #[pyo3(get)]
    pub lt: Option<f64>,
    #[pyo3(get)]
    pub le: Option<f64>,
    #[pyo3(get)]
    pub min_length: Option<usize>,
    #[pyo3(get)]
    pub max_length: Option<usize>,
    #[pyo3(get)]
    pub pattern: Option<String>,
    #[pyo3(get)]
    pub deprecated: Option<bool>,
    #[pyo3(get)]
    pub include_in_schema: bool,
    #[pyo3(get)]
    pub examples: Option<Py<PyAny>>,
}

#[pymethods]
impl PyFile {
    #[new]
    #[pyo3(signature = (default=None, *, media_type="multipart/form-data".to_string(), alias=None, title=None, description=None, gt=None, ge=None, lt=None, le=None, min_length=None, max_length=None, pattern=None, deprecated=None, include_in_schema=true, examples=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        default: Option<Py<PyAny>>,
        media_type: String,
        alias: Option<String>,
        title: Option<String>,
        description: Option<String>,
        gt: Option<f64>,
        ge: Option<f64>,
        lt: Option<f64>,
        le: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        pattern: Option<String>,
        deprecated: Option<bool>,
        include_in_schema: bool,
        examples: Option<Py<PyAny>>,
        _extra: Option<&Bound<'_, PyDict>>,
    ) -> Self {
        Self {
            default,
            media_type,
            alias,
            title,
            description,
            gt,
            ge,
            lt,
            le,
            min_length,
            max_length,
            pattern,
            deprecated,
            include_in_schema,
            examples,
        }
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Unset>()?;
    m.add_class::<Undefined>()?;
    m.add_class::<PyDepends>()?;
    m.add_class::<PySecurity>()?;

    m.add_class::<PyQuery>()?;
    m.add_class::<PyPath>()?;
    m.add_class::<PyBody>()?;
    m.add_class::<PyCookie>()?;
    m.add_class::<PyHeader>()?;
    m.add_class::<PyForm>()?;
    m.add_class::<PyFile>()?;
    Ok(())
}
