//! Types that describe the parameters of a route handler.
//! They tell where a value comes from and which constraints the value must satisfy.

use regex::Regex;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

impl ParameterConstraints {
    #[inline]
    #[must_use]
    pub const fn has_numeric_bounds(&self) -> bool {
        self.gt.is_some() || self.ge.is_some() || self.lt.is_some() || self.le.is_some()
    }

    #[inline]
    #[must_use]
    pub const fn has_text_bounds(&self) -> bool {
        self.min_length.is_some() || self.max_length.is_some() || self.pattern.is_some()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PathParamRange {
    pub name_index: usize,
    pub start: usize,
    pub end: usize,
}
