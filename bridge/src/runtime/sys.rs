//! Small Win32 helpers shared by the runtime modules.

use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// A kernel handle that is closed when dropped.
#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

impl OwnedHandle {
    /// Takes ownership of `handle`.
    ///
    /// # Safety
    ///
    /// `handle` must be a valid kernel handle that nothing else will close.
    pub unsafe fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    /// The raw handle, still owned by `self`.
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `new`'s contract makes us the sole owner of a valid handle, and it is closed
        // exactly once, here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Encodes a string as a null-terminated UTF-16 buffer for Win32 `W` functions.
pub fn to_wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Locks `mutex`, recovering the data if a panic poisoned it.
///
/// Panics are caught at every entry point from the game, so poisoning is possible; the data
/// guarded here is always left consistent, so carrying on is the right call.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
