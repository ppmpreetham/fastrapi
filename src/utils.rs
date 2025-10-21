use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{Map, Value};
use serde_pyobject::to_pyobject;

// TODO: handle the Ctrl + C shutdown without affecting speed
// pub async fn shutdown_signal() {
//     let ctrl_c = async {
//         tokio::signal::ctrl_c()
//             .await
//             .expect("failed to install Ctrl+C handler");
//     };

//     #[cfg(unix)]
//     let terminate = async {
//         signal::unix::signal(signal::unix::SignalKind::terminate())
//             .expect("failed to install signal handler")
//             .recv()
//             .await;
//     };

//     #[cfg(not(unix))]
//     let terminate = std::future::pending::<()>();

//     tokio::select! {
//         _ = ctrl_c => {},
//         _ = terminate => {},
//     }
// }

pub fn json_to_py_object<'py>(py: Python<'py>, value: &Value) -> Py<PyAny> {
    match to_pyobject(py, value) {
        Ok(obj) => obj.into(),
        Err(e) => {
            eprintln!("Error converting JSON to Python object: {}", e);
            format!("Error: {}", e).into_pyobject(py).unwrap().into()
        }
    }
}

pub fn py_to_response(py: Python<'_>, obj: &Bound<'_, PyAny>) -> Response {
    if let Ok(s) = obj.extract::<String>() {
        return s.into_response();
    }
    if let Ok(i) = obj.extract::<i64>() {
        return i.to_string().into_response();
    }
    if let Ok(f) = obj.extract::<f64>() {
        return f.to_string().into_response();
    }
    if let Ok(b) = obj.extract::<bool>() {
        return b.to_string().into_response();
    }

    if let Ok(dict) = obj.cast::<PyDict>() {
        let json = py_dict_to_json(py, dict);
        return Json(json).into_response();
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let json = py_list_to_json(py, list);
        return Json(json).into_response();
    }

    // Handle None
    if obj.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    // Fallback
    format!("{:?}", obj).into_response()
}

/// JSON/Python conversion helpers
pub fn py_dict_to_json(py: Python<'_>, dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Map::new();
    for (key, value) in dict.iter() {
        let k: String = match key.extract() {
            Ok(s) => s,
            Err(_) => continue,
        };

        map.insert(k, py_any_to_json(py, &value));
    }
    Value::Object(map)
}

/// Convert Python list to JSON
pub fn py_list_to_json(py: Python<'_>, list: &Bound<'_, PyList>) -> Value {
    let mut vec = Vec::new();
    for item in list.iter() {
        vec.push(py_any_to_json(py, &item));
    }
    Value::Array(vec)
}

/// Convert any Python object to JSON Value
fn py_any_to_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    // Try simple types first
    if let Ok(s) = value.extract::<String>() {
        return Value::String(s);
    }
    if let Ok(i) = value.extract::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = value.extract::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(f) {
            return Value::Number(num);
        }
        return Value::Null;
    }
    if let Ok(b) = value.extract::<bool>() {
        return Value::Bool(b);
    }
    if value.is_none() {
        return Value::Null;
    }

    if let Ok(nested_dict) = value.cast::<PyDict>() {
        return py_dict_to_json(py, nested_dict);
    }
    if let Ok(nested_list) = value.cast::<PyList>() {
        return py_list_to_json(py, nested_list);
    }

    Value::String(format!("{:?}", value))
}
