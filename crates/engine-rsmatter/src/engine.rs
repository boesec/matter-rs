//! The facade's [`Engine`] and [`Running`] on rs-matter: the product-side handles.
//!
//! [`RsMatterEngine::start`] spawns the engine thread and waits until the node is on the
//! network; the [`Running`] handle it returns turns every call into a request for that thread
//! and completes when the thread has applied it. All futures are `Send` and hold nothing but a
//! reference to the shared queues.

use std::future::{poll_fn, ready};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, BoxFuture, Capabilities, ClusterId,
    CommissioningState, Correlation, EndpointDescription, EndpointId, Engine, Error, Event,
    EventId, FabricIndex, FabricInfo, NetworkConfig, NodeIdentity, NodeRole, PairingInfo, Response,
    Running, Storage, Value, Window,
};
use rs_matter::fabric::MAX_FABRICS;
use rs_matter::im::subscriptions::DEFAULT_MAX_SUBSCRIPTIONS;

use crate::channel::{Reply, Request, Shared};
use crate::node::{self, NodeConfig};
use crate::pairing;

/// The default time the product has to answer a command or a write before the engine answers
/// `Failure` to the client.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// The default number of endpoints the product may add.
pub const DEFAULT_ENDPOINT_CAPACITY: usize = 64;

/// The engine facade on rs-matter.
///
/// One engine runs one node at a time on a dedicated thread. The engine is `Send + Sync` and
/// is usually held as a `Box<dyn Engine>` by the service binary.
pub struct RsMatterEngine {
    response_timeout: Duration,
    endpoint_capacity: usize,
    inner: Mutex<Inner>,
}

struct Inner {
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl Default for RsMatterEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RsMatterEngine {
    /// An engine with the default response timeout and endpoint capacity.
    #[must_use]
    pub fn new() -> Self {
        Self {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            endpoint_capacity: DEFAULT_ENDPOINT_CAPACITY,
            inner: Mutex::new(Inner {
                shared: None,
                thread: None,
            }),
        }
    }

    /// Sets how long the product may take to answer a command or a write; after that the
    /// client gets `Failure` and a late response is [`Error::UnknownCorrelation`].
    #[must_use]
    pub fn with_response_timeout(mut self, timeout: Duration) -> Self {
        self.response_timeout = timeout;
        self
    }

    /// Sets how many endpoints the product may add.
    #[must_use]
    pub fn with_endpoint_capacity(mut self, capacity: usize) -> Self {
        self.endpoint_capacity = capacity;
        self
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            max_endpoints: Some(self.endpoint_capacity),
            max_fabrics: Some(MAX_FABRICS),
            max_subscriptions: Some(DEFAULT_MAX_SUBSCRIPTIONS),
            enhanced_window: false,
        }
    }
}

impl Drop for RsMatterEngine {
    /// Stops a node that still runs and waits for the engine thread, so that its sockets are
    /// released with the engine.
    fn drop(&mut self) {
        let mut inner = self.lock();
        if let Some(shared) = inner.shared.take()
            && shared.is_running()
        {
            let (reply, _) = Reply::new();
            let _ = shared.enqueue(Request::Stop(reply));
        }
        if let Some(thread) = inner.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Engine for RsMatterEngine {
    fn start(
        &self,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        role: NodeRole,
        storage: Arc<dyn Storage>,
    ) -> BoxFuture<'_, Result<Box<dyn Running>, Error>> {
        Box::pin(async move {
            pairing::validate_identity(&identity)?;
            if !network.ipv4 && !network.ipv6 {
                return Err(Error::InvalidIdentity {
                    reason: String::from("neither IPv4 nor IPv6 is enabled"),
                });
            }
            let (shared, started) = {
                let mut inner = self.lock();
                if let Some(shared) = &inner.shared
                    && shared.is_running()
                {
                    return Err(Error::AlreadyRunning);
                }
                if let Some(thread) = inner.thread.take() {
                    let _ = thread.join();
                }
                let shared = Arc::new(Shared::new());
                let (started, started_future) = Reply::new();
                let config = NodeConfig {
                    identity,
                    network,
                    attestation,
                    role,
                    storage,
                    response_timeout: self.response_timeout,
                    endpoint_capacity: self.endpoint_capacity,
                };
                let thread_shared = Arc::clone(&shared);
                let thread = thread::Builder::new()
                    .name(String::from("matter-rs-engine"))
                    .stack_size(node::THREAD_STACK_SIZE)
                    .spawn(move || node::run_thread(config, thread_shared, started))
                    .map_err(|error| {
                        Error::Engine(format!("cannot spawn the engine thread: {error}"))
                    })?;
                inner.shared = Some(Arc::clone(&shared));
                inner.thread = Some(thread);
                (shared, started_future)
            };
            match started.await {
                Some(Ok(())) => {
                    let running: Box<dyn Running> = Box::new(RunningNode {
                        shared,
                        capabilities: self.capabilities(),
                    });
                    Ok(running)
                }
                Some(Err(error)) => Err(error),
                None => Err(Error::Engine(String::from(
                    "the engine thread ended before the node started",
                ))),
            }
        })
    }

    fn capabilities(&self) -> Capabilities {
        Self::capabilities(self)
    }
}

/// The running handle: every method queues a request for the engine thread.
struct RunningNode {
    shared: Arc<Shared>,
    capabilities: Capabilities,
}

impl RunningNode {
    fn request<T>(
        &self,
        make: impl FnOnce(Reply<Result<T, Error>>) -> Request,
    ) -> BoxFuture<'_, Result<T, Error>>
    where
        T: Send + 'static,
    {
        let (reply, future) = Reply::new();
        let queued = self.shared.enqueue(make(reply));
        Box::pin(async move {
            queued?;
            future.await.unwrap_or(Err(Error::NotRunning))
        })
    }
}

impl Running for RunningNode {
    fn add_endpoint(&self, description: EndpointDescription) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::AddEndpoint(description, reply))
    }

    fn remove_endpoint(&self, endpoint: EndpointId) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::RemoveEndpoint(endpoint, reply))
    }

    fn set_attribute(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.set_attributes(vec![AttributeValue {
            endpoint,
            cluster,
            attribute,
            value,
        }])
    }

    fn set_attributes(&self, values: Vec<AttributeValue>) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::SetAttributes(values, reply))
    }

    fn emit_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
        payload: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::EmitEvent {
            endpoint,
            cluster,
            event,
            payload,
            reply,
        })
    }

    fn set_reachable(
        &self,
        endpoint: EndpointId,
        reachable: bool,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::SetReachable(endpoint, reachable, reply))
    }

    fn respond(
        &self,
        correlation: Correlation,
        response: Response,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(ready(self.shared.respond(correlation, response)))
    }

    fn commissioning_state(&self) -> BoxFuture<'_, Result<CommissioningState, Error>> {
        self.request(Request::CommissioningState)
    }

    fn open_commissioning_window(
        &self,
        window: Window,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::OpenWindow(window, timeout, reply))
    }

    fn close_commissioning_window(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.request(Request::CloseWindow)
    }

    fn fabrics(&self) -> BoxFuture<'_, Result<Vec<FabricInfo>, Error>> {
        self.request(Request::Fabrics)
    }

    fn remove_fabric(&self, fabric: FabricIndex) -> BoxFuture<'_, Result<(), Error>> {
        self.request(|reply| Request::RemoveFabric(fabric, reply))
    }

    fn pairing_info(&self) -> BoxFuture<'_, Result<PairingInfo, Error>> {
        self.request(Request::PairingInfo)
    }

    fn factory_reset(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.request(Request::FactoryReset)
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn next_event(&self) -> BoxFuture<'_, Result<Event, Error>> {
        Box::pin(poll_fn(move |cx| self.shared.poll_event(cx)))
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.request(Request::Stop)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DEFAULT_ENDPOINT_CAPACITY, DEFAULT_RESPONSE_TIMEOUT, RsMatterEngine};

    #[test]
    fn capabilities_report_the_compile_time_limits() {
        let engine = RsMatterEngine::new();
        let capabilities = engine.capabilities();
        assert_eq!(capabilities.max_endpoints, Some(DEFAULT_ENDPOINT_CAPACITY));
        assert_eq!(capabilities.max_fabrics, Some(5));
        assert_eq!(capabilities.max_subscriptions, Some(15));
        assert!(!capabilities.enhanced_window);
        assert_eq!(engine.response_timeout, DEFAULT_RESPONSE_TIMEOUT);

        let engine = RsMatterEngine::new()
            .with_endpoint_capacity(3)
            .with_response_timeout(Duration::from_millis(10));
        assert_eq!(engine.capabilities().max_endpoints, Some(3));
        assert_eq!(engine.response_timeout, Duration::from_millis(10));
    }

    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RsMatterEngine>();
    }
}
