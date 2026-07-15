// here lies all the decoraters signatures for get, put, patch, post, delete, options, head
// once upon a time, here lied all the methods for the above, but they were later abstracted away by heros called macros

use super::PyAPIRouter;
use pyo3::prelude::Python;

impl PyAPIRouter {
    #[inline]
    fn _router<'a>(&'a self, _py: Python<'_>) -> &'a PyAPIRouter {
        self
    }
}
crate::generate_http_methods!(PyAPIRouter, _router);
