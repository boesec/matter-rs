//! The adapter trait.

use alloc::boxed::Box;

use matter_rs_engine::BoxFuture;

use crate::context::AdapterContext;
use crate::error::Error;

/// An adapter: the task that brings devices into the product and carries their commands back
/// out.
///
/// An adapter owns one task. [`run`](Self::run) consumes the adapter and returns the future of
/// that task; the host (the supervisor) drives it on its executor. The future ends when the
/// adapter stops on its own or when the context's [`shutdown`](AdapterContext::shutdown) asks it
/// to; returning an error or panicking means the adapter failed, and the host restarts it from a
/// fresh instance after a delay. The adapter keeps its state in `self`; a restarted adapter
/// starts from what its factory builds.
///
/// Rules every adapter follows:
///
/// - **Never block.** The future runs on a shared executor; blocking I/O or a long computation
///   stalls every other adapter on that thread. Use asynchronous I/O or move the work to a
///   thread of your own.
/// - **Honour shutdown.** When [`AdapterContext::shutdown`] resolves, stop within a few seconds
///   and return `Ok(())`. An adapter that does not is aborted at the stop timeout.
/// - **Report health.** Call [`AdapterContext::set_health`] when the adapter is up, degraded or
///   failed; the operator sees it.
/// - **Fail loudly.** A lost connection that the adapter cannot restore is an `Err`; the host
///   marks the devices unreachable and restarts the adapter with backoff.
pub trait Adapter: Send {
    /// Runs the adapter until it stops or fails.
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>>;
}
