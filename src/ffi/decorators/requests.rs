// here lies all the decoraters signatures for get, put, patch, post, delete, options, head
// again, wish these could be abstracted away by impls, but sadly pyo3 doesn't support it, atleast for now

use super::PyAPIRouter;
use pyo3::prelude::Python;

impl PyAPIRouter {
    #[inline]
    fn _router<'a>(&'a self, _py: Python<'_>) -> &'a PyAPIRouter {
        self
    }
}
crate::generate_http_methods!(PyAPIRouter, _router);
