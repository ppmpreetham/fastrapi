//! A Python thread state held by a runtime thread from its first Python call for the rest of its life.

#[cfg(Py_3_13)]
mod imp {
    use pyo3::prelude::*;
    use pyo3::ffi;
    use std::cell::Cell;
    use std::ptr;
    use std::sync::OnceLock;

    thread_local! {
        static WORKER: Cell<bool> = const { Cell::new(false) };
        static HELD: Cell<*mut ffi::PyThreadState> = const { Cell::new(ptr::null_mut()) };
    }

    static NOOP: OnceLock<Py<PyAny>> = OnceLock::new();

    pub fn mark_worker() {
        WORKER.set(true);
    }

    #[inline]
    pub fn hold() {
        if !WORKER.get() || !HELD.get().is_null() {
            return;
        }
        unsafe {
            if ffi::PyGILState_Check() != 0 || ffi::Py_IsFinalizing() != 0 {
                return;
            }
            ffi::PyGILState_Ensure();
            if NOOP.get().is_none() {
                Python::attach(|py| {
                    if let Ok(noop) = py.eval(c"lambda: None", None, None) {
                        _ = NOOP.set(noop.unbind());
                    }
                });
            }
            HELD.set(ffi::PyEval_SaveThread());
        }
    }

    pub fn flush() {
        let tstate = HELD.get();
        let Some(noop) = NOOP.get() else { return };
        unsafe {
            if tstate.is_null() || ffi::PyGILState_Check() != 0 || ffi::Py_IsFinalizing() != 0 {
                return;
            }
            ffi::PyEval_RestoreThread(tstate);
            let out = ffi::PyObject_CallNoArgs(noop.as_ptr());
            if out.is_null() {
                ffi::PyErr_Clear();
            } else {
                ffi::Py_DECREF(out);
            }
            HELD.set(ffi::PyEval_SaveThread());
        }
    }
}

#[cfg(not(Py_3_13))]
mod imp {
    pub fn mark_worker() {}
    #[inline]
    pub fn hold() {}
    pub fn flush() {}
}

pub use imp::{flush, hold, mark_worker};
