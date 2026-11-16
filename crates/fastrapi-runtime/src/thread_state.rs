//! A Python thread state held by a runtime thread from its first Python call for the rest of its life.

#[cfg(Py_3_13)]
mod imp {
    use pyo3::ffi;
    use pyo3::prelude::*;
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
        // SAFETY: this runs on a runtime thread that holds no thread state yet (`HELD` is null)
        // and is not attached (`PyGILState_Check` is 0), while the interpreter is not finalizing.
        // `PyGILState_Ensure` creates the thread state and attaches; it is never released with
        // `PyGILState_Release`, so the state lives as long as the thread. `PyEval_SaveThread`
        // detaches and returns that state, which `HELD` keeps for `flush`.
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
        // SAFETY: `tstate` is the detached thread state `hold` saved on this same thread, and the
        // thread is not attached (`PyGILState_Check` is 0), so restoring it is the only attach.
        // `noop` is a live callable kept by `NOOP`; the call runs attached and its result or
        // error is released before `PyEval_SaveThread` detaches again and saves the state back.
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
