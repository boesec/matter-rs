//! What crosses between the product side and the engine thread: requests with their replies,
//! the event queue and the responses the product gives to commands and writes.
//!
//! Everything here is plain standard-library synchronisation: a mutex around the queues and
//! `Waker`s for the two sides. The product side's futures therefore hold nothing but an
//! `Arc<Shared>` and are `Send`; the engine thread registers the waker of its single-threaded
//! executor and is woken from the product side through it.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use matter_rs_engine::{
    AttributeValue, ClusterId, CommissioningState, Correlation, EndpointDescription, EndpointId,
    Error, Event, EventId, FabricIndex, FabricInfo, PairingInfo, Response, StopReason, Value,
    Window,
};

/// The reply slot of one request: the engine thread completes it, the product side awaits it.
pub(crate) struct Reply<T> {
    inner: Arc<Mutex<ReplyInner<T>>>,
    completed: bool,
}

struct ReplyInner<T> {
    value: Option<T>,
    closed: bool,
    waker: Option<Waker>,
}

impl<T> Reply<T> {
    /// A new slot and the future that waits for it.
    pub(crate) fn new() -> (Self, ReplyFuture<T>) {
        let inner = Arc::new(Mutex::new(ReplyInner {
            value: None,
            closed: false,
            waker: None,
        }));
        (
            Self {
                inner: Arc::clone(&inner),
                completed: false,
            },
            ReplyFuture(inner),
        )
    }

    /// Completes the slot with `value` and wakes the waiting side.
    pub(crate) fn complete(mut self, value: T) {
        self.completed = true;
        let mut inner = lock(&self.inner);
        inner.value = Some(value);
        inner.closed = true;
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }
}

impl<T> Drop for Reply<T> {
    /// A slot dropped without a value closes the future with `None`, so that the waiting side
    /// never hangs when the engine thread goes away.
    fn drop(&mut self) {
        if !self.completed {
            let mut inner = lock(&self.inner);
            inner.closed = true;
            if let Some(waker) = inner.waker.take() {
                waker.wake();
            }
        }
    }
}

/// The future side of a [`Reply`]: `None` when the slot was dropped without a value.
pub(crate) struct ReplyFuture<T>(Arc<Mutex<ReplyInner<T>>>);

impl<T> Future for ReplyFuture<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = lock(&self.0);
        if let Some(value) = inner.value.take() {
            return Poll::Ready(Some(value));
        }
        if inner.closed {
            return Poll::Ready(None);
        }
        inner.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// A request from the product side to the engine thread, with the slot for its reply.
pub(crate) enum Request {
    AddEndpoint(EndpointDescription, Reply<Result<(), Error>>),
    RemoveEndpoint(EndpointId, Reply<Result<(), Error>>),
    SetAttributes(Vec<AttributeValue>, Reply<Result<(), Error>>),
    EmitEvent {
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
        payload: Value,
        reply: Reply<Result<(), Error>>,
    },
    SetReachable(EndpointId, bool, Reply<Result<(), Error>>),
    CommissioningState(Reply<Result<CommissioningState, Error>>),
    OpenWindow(Window, Duration, Reply<Result<(), Error>>),
    CloseWindow(Reply<Result<(), Error>>),
    Fabrics(Reply<Result<Vec<FabricInfo>, Error>>),
    RemoveFabric(FabricIndex, Reply<Result<(), Error>>),
    PairingInfo(Reply<Result<PairingInfo, Error>>),
    FactoryReset(Reply<Result<(), Error>>),
    Stop(Reply<Result<(), Error>>),
}

impl Request {
    /// Fails the request with `error`, for requests that never reach the engine thread.
    fn fail(self, error: Error) {
        match self {
            Self::AddEndpoint(_, reply)
            | Self::RemoveEndpoint(_, reply)
            | Self::SetAttributes(_, reply)
            | Self::EmitEvent { reply, .. }
            | Self::SetReachable(_, _, reply)
            | Self::OpenWindow(_, _, reply)
            | Self::CloseWindow(reply)
            | Self::RemoveFabric(_, reply)
            | Self::FactoryReset(reply)
            | Self::Stop(reply) => reply.complete(Err(error)),
            Self::CommissioningState(reply) => reply.complete(Err(error)),
            Self::Fabrics(reply) => reply.complete(Err(error)),
            Self::PairingInfo(reply) => reply.complete(Err(error)),
        }
    }
}

/// A command or write that waits for the product's response.
struct Pending {
    response: Option<Response>,
    waker: Option<Waker>,
}

/// The state shared by the product side and the engine thread.
pub(crate) struct Shared {
    state: Mutex<SharedState>,
}

struct SharedState {
    /// Whether the node runs: set at start, cleared when the engine thread finishes.
    running: bool,
    /// Requests from the product side, in order.
    requests: VecDeque<Request>,
    /// The waker of the engine thread's executor, registered by the request receiver.
    engine_waker: Option<Waker>,
    /// Events for the product side, in order; the stop event is the last one.
    events: VecDeque<Event>,
    /// The waker of the product task that pulls events.
    product_waker: Option<Waker>,
    /// Commands and writes that wait for the product's response, by correlation.
    pending: BTreeMap<u64, Pending>,
    /// The next correlation to issue.
    next_correlation: u64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// Shared state of a node that is about to run.
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(SharedState {
                running: true,
                requests: VecDeque::new(),
                engine_waker: None,
                events: VecDeque::new(),
                product_waker: None,
                pending: BTreeMap::new(),
                next_correlation: 1,
            }),
        }
    }

    /// Whether the node runs.
    pub(crate) fn is_running(&self) -> bool {
        lock(&self.state).running
    }

    /// Queues a request for the engine thread.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when the node does not run; the request's reply is failed too.
    pub(crate) fn enqueue(&self, request: Request) -> Result<(), Error> {
        let mut state = lock(&self.state);
        if !state.running {
            drop(state);
            request.fail(Error::NotRunning);
            return Err(Error::NotRunning);
        }
        state.requests.push_back(request);
        if let Some(waker) = state.engine_waker.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Waits for the next request; for the engine thread.
    pub(crate) fn next_request(&self) -> impl Future<Output = Request> + '_ {
        std::future::poll_fn(move |cx| {
            let mut state = lock(&self.state);
            if let Some(request) = state.requests.pop_front() {
                Poll::Ready(request)
            } else {
                state.engine_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
    }

    /// Queues an event for the product side.
    pub(crate) fn push_event(&self, event: Event) {
        let mut state = lock(&self.state);
        state.events.push_back(event);
        if let Some(waker) = state.product_waker.take() {
            waker.wake();
        }
    }

    /// Polls for the next event; for the product side.
    pub(crate) fn poll_event(&self, cx: &mut Context<'_>) -> Poll<Result<Event, Error>> {
        let mut state = lock(&self.state);
        if let Some(event) = state.events.pop_front() {
            return Poll::Ready(Ok(event));
        }
        if !state.running {
            return Poll::Ready(Err(Error::NotRunning));
        }
        state.product_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    /// Issues a correlation for a command or write that the engine thread will wait on.
    pub(crate) fn issue(&self) -> Correlation {
        let mut state = lock(&self.state);
        let correlation = state.next_correlation;
        state.next_correlation += 1;
        state.pending.insert(
            correlation,
            Pending {
                response: None,
                waker: None,
            },
        );
        Correlation::new(correlation)
    }

    /// Waits for the product's response to `correlation`; for the engine thread. `None` when
    /// the node stopped before the product answered.
    pub(crate) fn await_response(
        &self,
        correlation: Correlation,
    ) -> impl Future<Output = Option<Response>> + '_ {
        std::future::poll_fn(move |cx| {
            let mut state = lock(&self.state);
            let running = state.running;
            let Some(pending) = state.pending.get_mut(&correlation.raw()) else {
                return Poll::Ready(None);
            };
            let response = pending.response.take();
            if response.is_none() && running {
                pending.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            state.pending.remove(&correlation.raw());
            Poll::Ready(response)
        })
    }

    /// Forgets a pending correlation whose wait timed out; a response that arrives later is
    /// then [`Error::UnknownCorrelation`].
    pub(crate) fn forget(&self, correlation: Correlation) {
        lock(&self.state).pending.remove(&correlation.raw());
    }

    /// Records the product's response; for the product side.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when the node does not run, [`Error::UnknownCorrelation`] when
    /// nothing waits for `correlation` or it was answered already.
    pub(crate) fn respond(
        &self,
        correlation: Correlation,
        response: Response,
    ) -> Result<(), Error> {
        let mut state = lock(&self.state);
        if !state.running {
            return Err(Error::NotRunning);
        }
        let Some(pending) = state.pending.get_mut(&correlation.raw()) else {
            return Err(Error::UnknownCorrelation(correlation));
        };
        if pending.response.is_some() {
            return Err(Error::UnknownCorrelation(correlation));
        }
        pending.response = Some(response);
        if let Some(waker) = pending.waker.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Ends the node: the stop event becomes the last event, queued requests fail with
    /// [`Error::NotRunning`], and waiting responders are released.
    pub(crate) fn finish(&self, reason: StopReason) {
        let (requests, wakers) = {
            let mut state = lock(&self.state);
            state.running = false;
            state.events.push_back(Event::Stopped(reason));
            let requests: Vec<Request> = state.requests.drain(..).collect();
            let mut wakers: Vec<Waker> = state
                .pending
                .values_mut()
                .filter_map(|pending| pending.waker.take())
                .collect();
            state.pending.clear();
            wakers.extend(state.product_waker.take());
            wakers.extend(state.engine_waker.take());
            (requests, wakers)
        };
        for request in requests {
            request.fail(Error::NotRunning);
        }
        for waker in wakers {
            waker.wake();
        }
    }

    /// Marks a node that never started as not running, without a stop event.
    pub(crate) fn abort(&self) {
        let requests: Vec<Request> = {
            let mut state = lock(&self.state);
            state.running = false;
            state.requests.drain(..).collect()
        };
        for request in requests {
            request.fail(Error::NotRunning);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use matter_rs_engine::{Correlation, Error, Event, Response, StopReason};

    use super::{Reply, Request, Shared};

    fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> Poll<F::Output> {
        let mut cx = Context::from_waker(Waker::noop());
        std::pin::Pin::new(future).poll(&mut cx)
    }

    #[test]
    fn reply_completes_and_closes() {
        let (reply, mut future) = Reply::new();
        assert!(poll_once(&mut future).is_pending());
        reply.complete(7);
        assert_eq!(poll_once(&mut future), Poll::Ready(Some(7)));

        let (reply, mut future) = Reply::<u8>::new();
        drop(reply);
        assert_eq!(poll_once(&mut future), Poll::Ready(None));
    }

    #[test]
    fn requests_fail_after_the_node_finished() {
        let shared = Shared::new();
        let (reply, mut future) = Reply::new();
        shared.enqueue(Request::CloseWindow(reply)).unwrap();
        shared.finish(StopReason::Requested);
        assert_eq!(
            poll_once(&mut future),
            Poll::Ready(Some(Err(Error::NotRunning)))
        );
        let (reply, mut future) = Reply::new();
        assert_eq!(
            shared.enqueue(Request::CloseWindow(reply)),
            Err(Error::NotRunning)
        );
        assert_eq!(
            poll_once(&mut future),
            Poll::Ready(Some(Err(Error::NotRunning)))
        );
    }

    #[test]
    fn events_end_with_stop() {
        let shared = Shared::new();
        let mut cx = Context::from_waker(Waker::noop());
        assert!(shared.poll_event(&mut cx).is_pending());
        shared.finish(StopReason::Requested);
        assert_eq!(
            shared.poll_event(&mut cx),
            Poll::Ready(Ok(Event::Stopped(StopReason::Requested)))
        );
        assert_eq!(
            shared.poll_event(&mut cx),
            Poll::Ready(Err(Error::NotRunning))
        );
    }

    #[test]
    fn responses_are_matched_once() {
        let shared = Shared::new();
        let correlation = shared.issue();
        assert_eq!(
            shared.respond(Correlation::new(99), Response::success()),
            Err(Error::UnknownCorrelation(Correlation::new(99)))
        );
        shared.respond(correlation, Response::success()).unwrap();
        assert_eq!(
            shared.respond(correlation, Response::success()),
            Err(Error::UnknownCorrelation(correlation))
        );
        let mut future = std::pin::pin!(shared.await_response(correlation));
        assert_eq!(
            poll_once(&mut future),
            Poll::Ready(Some(Response::success()))
        );
        // Collected: answering again is unknown.
        assert_eq!(
            shared.respond(correlation, Response::success()),
            Err(Error::UnknownCorrelation(correlation))
        );

        let forgotten = shared.issue();
        shared.forget(forgotten);
        assert_eq!(
            shared.respond(forgotten, Response::success()),
            Err(Error::UnknownCorrelation(forgotten))
        );
    }
}
