//! Cached Python modules and attributes that many crates use.
//! Each item is imported one time, on first use.

use crate::cached_py_import;

cached_py_import!(pub INSPECT, "inspect");
cached_py_import!(pub TYPING, "typing");
cached_py_import!(pub ENUM, "enum");
cached_py_import!(pub TYPES, "types");
cached_py_import!(pub STARLETTE_STATE, "starlette.datastructures", "State");
cached_py_import!(pub STARLETTE_HEADERS, "starlette.datastructures", "Headers");
cached_py_import!(pub SIMPLE_COOKIE, "http.cookies", "SimpleCookie");
