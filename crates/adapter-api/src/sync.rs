//! The lock of the context's shared state.
//!
//! The adapter and the host that runs it (the supervisor) touch the same small structures: the
//! health the adapter reports, the queue of routed commands, the shutdown signal, the ownership
//! of devices. Each sits behind a lock that is held only for the duration of a closure and
//! never across an await. Under `std` the lock is the standard mutex; on the `no_std` tier it
//! is a critical-section mutex, whose implementation the embedded target provides.
//!
//! Locks are created by the host side of the context, which only the supervisor (`std` and
//! `role-bridge`) uses; other builds compile the constructor unused.

#![cfg_attr(not(all(feature = "std", feature = "role-bridge")), allow(dead_code))]

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
    /// consistent whatever panicked while holding the lock, and a panicking adapter must not
    /// take its context down with it.
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
