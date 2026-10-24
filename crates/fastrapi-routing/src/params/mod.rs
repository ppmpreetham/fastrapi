//! Python parameter classes and the helpers that read endpoint parameters.
//! This module contains the parameter markers, the dependency markers, and the sentinels.
//! It also converts each endpoint parameter into a `ParsedParameter`.

mod classes;
mod constraints;
mod depends;
mod parse;
mod sentinels;
mod utils;

pub use classes::{PyBody, PyCookie, PyFile, PyForm, PyHeader, PyPath, PyQuery};
pub use depends::{PyDepends, PySecurity};
pub use parse::parse_parameter_spec;
pub use sentinels::{Undefined, Unset};
pub use utils::{
    base_annotation, find_annotated_dependency_marker, is_inspect_empty, list_element_annotation,
};
