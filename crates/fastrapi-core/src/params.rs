use regex::Regex;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterSource {
    Path,
    Query,
    Body,
    Header,
    Cookie,
    BackgroundTasks,
}

#[derive(Clone, Debug, Default)]
pub struct ParameterConstraints {
    pub gt: Option<f64>,
    pub ge: Option<f64>,
    pub lt: Option<f64>,
    pub le: Option<f64>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub pattern: Option<Arc<Regex>>,
}

#[derive(Clone, Copy, Debug)]
pub struct PathParamRange {
    pub name_index: usize,
    pub start: usize,
    pub end: usize,
}
