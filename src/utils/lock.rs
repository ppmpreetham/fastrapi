use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub trait LockExt<T> {
    fn lock_or_panic(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    #[inline(always)]
    fn lock_or_panic(&self) -> MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(|e| panic!("Mutex poisoned: {e}"))
    }
}

/// Extension trait for ergonomic RwLock acquisition with consistent error messaging.
pub trait RwLockExt<T> {
    /// Acquires a shared read lock, panicking if poisoned.
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T>;

    /// Acquires an exclusive write lock, panicking if poisoned.
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    #[inline(always)]
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T> {
        self.read()
            .unwrap_or_else(|e| panic!("RwLock read poisoned: {e}"))
    }

    #[inline(always)]
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T> {
        self.write()
            .unwrap_or_else(|e| panic!("RwLock write poisoned: {e}"))
    }
}
