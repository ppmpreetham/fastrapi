use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{Value, Map};
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
        s.into_response()
    }
    else if let Ok(i) = obj.extract::<i64>() {
        i.to_string().into_response()
    }
    else if let Ok(dict) = obj.downcast::<PyDict>() {
        let json = py_dict_to_json(dict);
        Json(json).into_response()
    }
    else if obj.is_none() {
        StatusCode::NO_CONTENT.into_response()
    }
    else {
        format!("{:?}", obj).into_response()
    }
}

/// JSON/Python conversion helpers
pub fn py_dict_to_json(dict: &Bound<'_, PyDict>) -> Value {
    let mut map = Map::new();
    for (key, value) in dict.iter() {
        let k: String = match key.extract() { 
            Ok(s) => s, 
            Err(_) => continue 
        };
       
        if let Ok(s) = value.extract::<String>() {
            map.insert(k, Value::String(s));
        }
        else if let Ok(i) = value.extract::<i64>() {
            map.insert(k, Value::Number(i.into()));
        }
        else if let Ok(f) = value.extract::<f64>() {
            if let Some(num) = serde_json::Number::from_f64(f) {
                map.insert(k, Value::Number(num));
            } else {
                map.insert(k, Value::Null);
            }
        }
        else if let Ok(b) = value.extract::<bool>() {
            map.insert(k, Value::Bool(b));
        }
        else if value.is_none() {
            map.insert(k, Value::Null);
        }
        else if let Ok(nested) = value.downcast::<PyDict>() {
            map.insert(k, py_dict_to_json(nested));
        }
        else {
            map.insert(k, Value::String(format!("{:?}", value)));
        }
    }
    Value::Object(map)
}