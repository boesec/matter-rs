//! The time source of the bridge's response timeout.
//!
//! The bridge is runtime-neutral and has no clock of its own. To answer `Failure` for a raw
//! command or write the adapter did not answer in time, it needs a [`Timer`] from the product:
//! on tokio that is a wrapper around `tokio::time::sleep`, on an embedded executor its timer, in
//! tests a timer the test fires by hand. Under `std` the [`ThreadTimer`] is the default: it
//! sleeps on a thread per pending response, which is fine for the rare raw command.

use core::time::Duration;

use matter_rs_engine::BoxFuture;

/// A source of delays.
pub trait Timer: Send + Sync {
    /// A future that completes after `duration`.
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
}

#[cfg(feature = "std")]
pub use self::thread_timer::ThreadTimer;

#[cfg(feature = "std")]
mod thread_timer {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::task::{Poll, Waker};
    use std::thread;
    use std::time::Duration;

    use matter_rs_engine::BoxFuture;

    use super::Timer;

    /// A timer that sleeps on a thread. Each sleep spawns a thread when first polled; a sleep
    /// that is dropped early leaves its thread to run out on its own.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct ThreadTimer;

    #[derive(Default)]
    struct Shared {
        done: AtomicBool,
        waker: Mutex<Option<Waker>>,
    }

    impl Timer for ThreadTimer {
        fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
            let shared = Arc::new(Shared::default());
            let mut started = false;
            Box::pin(core::future::poll_fn(move |cx| {
                if shared.done.load(Ordering::Acquire) {
                    return Poll::Ready(());
                }
                *shared.waker.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(cx.waker().clone());
                if !started {
                    started = true;
                    let shared = Arc::clone(&shared);
                    thread::spawn(move || {
                        thread::sleep(duration);
                        shared.done.store(true, Ordering::Release);
                        if let Some(waker) = shared
                            .waker
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .take()
                        {
                            waker.wake();
                        }
                    });
                }
                Poll::Pending
            }))
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::{Duration, Instant};

        use matter_rs_engine::testing::block_on;

        use super::ThreadTimer;
        use crate::timer::Timer;

        #[test]
        fn thread_timer_completes_after_the_duration() {
            let start = Instant::now();
            block_on(ThreadTimer.sleep(Duration::from_millis(20)));
            assert!(start.elapsed() >= Duration::from_millis(20));
        }
    }
}
