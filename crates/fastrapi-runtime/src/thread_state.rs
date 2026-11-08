//! A Python thread state held by a tokio worker between its first Python call and its next park.

#[cfg(Py_3_13)]
mod imp {
    use pyo3::ffi;
    use std::cell::Cell;
    use std::ptr;

    thread_local! {
        static WORKER: Cell<bool> = const { Cell::new(false) };
        static HELD: Cell<*mut ffi::PyThreadState> = const { Cell::new(ptr::null_mut()) };
    }

    pub fn mark_worker() {
        WORKER.set(true);
    }

    #[inline]
    #[must_use]
    pub fn is_held() -> bool {
        !HELD.get().is_null()
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
            HELD.set(ffi::PyEval_SaveThread());
        }
    }

    pub fn release() {
        let tstate = HELD.get();
        unsafe {
            if tstate.is_null() || ffi::PyGILState_Check() != 0 || ffi::Py_IsFinalizing() != 0 {
                return;
            }
            HELD.set(ptr::null_mut());
            ffi::PyEval_RestoreThread(tstate);
            ffi::PyGILState_Release(ffi::PyGILState_STATE::PyGILState_UNLOCKED);
        }
    }
}

#[cfg(not(Py_3_13))]
mod imp {
    pub fn mark_worker() {}
    #[inline]
    #[must_use]
    pub fn is_held() -> bool {
        false
    }
    #[inline]
    pub fn hold() {}
    pub fn release() {}
}

pub use imp::{hold, is_held, mark_worker, release};
