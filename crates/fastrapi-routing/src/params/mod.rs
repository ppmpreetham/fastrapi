//! Python parameter classes and the helpers that read endpoint parameters.
//! This module contains the parameter markers, the dependency markers, and the sentinels.
//! It also converts each endpoint parameter into a `ParsedParameter`.

mod constraints;
mod depends;
mod module;
mod parse;
mod utils;

pub use depends::{PyDepends, PySecurity};
pub use module::{errors_with_loc, json_error, params_module};
pub use parse::parse_parameter_spec;
pub use utils::{
    base_annotation, find_annotated_dependency_marker, is_inspect_empty, list_element_annotation,
};
