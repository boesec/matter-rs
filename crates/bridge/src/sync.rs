//! The lock of the bridge's shared state.
//!
//! The bridge is driven from two sides at once: the task that pulls events from the engine and
//! the adapter tasks that set values through device handles. Both need the device model, so it
//! sits behind a lock that is held only for the duration of a closure and never across an await.
//! Under `std` the lock is the standard mutex; on the `no_std` tier it is a critical-section
//! mutex, whose implementation the embedded target provides.

use core::fmt;

#[cfg(feature = "std")]
pub(crate) struct Lock<T>(std::sync::Mutex<T>);

#[cfg(not(feature = "std"))]
pub(crate) struct Lock<T>(critical_section::Mutex<core::cell::RefCell<T>>);

impl<T> Lock<T> {
    /// A lock around `value`.
    pub(crate) const fn new(value: T) -> Self {
        #[cfg(feature = "std")]
        {
            Self(std::sync::Mutex::new(value))
        }
        #[cfg(not(feature = "std"))]
        {
            Self(critical_section::Mutex::new(core::cell::RefCell::new(
                value,
            )))
        }
    }

    /// Runs `f` with exclusive access to the value.
    ///
    /// A poisoned standard mutex is used anyway: the state is a plain data structure that stays
    /// consistent whatever panicked while holding the lock.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        #[cfg(feature = "std")]
        {
            let mut guard = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            f(&mut guard)
        }
        #[cfg(not(feature = "std"))]
        {
            critical_section::with(|cs| f(&mut self.0.borrow_ref_mut(cs)))
        }
    }
}

impl<T> fmt::Debug for Lock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Lock")
    }
}

#[cfg(test)]
mod tests {
    use super::Lock;

    #[test]
    fn lock_gives_exclusive_access() {
        let lock = Lock::new(1);
        lock.with(|v| *v += 1);
        assert_eq!(lock.with(|v| *v), 2);
        assert_eq!(alloc::format!("{lock:?}"), "Lock");
    }

    #[test]
    fn lock_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Lock<alloc::vec::Vec<u8>>>();
    }
}
