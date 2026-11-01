//! An in-memory engine for testing product code without a Matter stack.
//!
//! [`FakeEngine`] implements [`Engine`] and [`Running`] completely in memory. Besides the
//! contract it offers test hooks that play the part of a Matter client and of the network:
//! [`FakeEngine::invoke`] and [`FakeEngine::write`] inject a command or a write and wait for the
//! product's response, [`FakeEngine::read`] looks at the value the engine would report,
//! [`FakeEngine::add_fabric`] and [`FakeEngine::set_commissioning_state`] simulate
//! commissioning, and [`FakeEngine::attribute_log`] and [`FakeEngine::event_log`] record every
//! attribute change and every emitted event. Capacity limits are configurable so that the error
//! paths of a product can be tested.
//!
//! This is how product crates are tested: the product gets a `Box<dyn Engine>` (a clone of the
//! fake), the test keeps the fake and drives it. [`block_on`] runs a future to completion on the
//! current thread, so a test needs no runtime.
//!
//! The fake keeps the contract described in the [`engine`](crate::engine) module with these
//! simplifications: it has no timeout for responses (an unanswered request waits until the
//! product answers or the node stops), commissioning window timeouts are ignored, the pairing
//! information is a placeholder that encodes the discriminator and the passcode but is not a
//! valid Matter payload, and fabrics live in memory of the fake instead of in the storage.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use crate::description::EndpointDescription;
use crate::engine::{
    AttributeValue, BoxFuture, Capabilities, CommissioningState, Correlation, Engine, Event,
    FabricInfo, PairingInfo, Response, Running, StopReason, Window, bridged_device,
};
use crate::error::Error;
use crate::id::{
    AttributeId, ClusterId, CommandId, EndpointId, EventId, FabricIndex, ROOT_ENDPOINT,
};
use crate::identity::{
    Attestation, MAX_DISCRIMINATOR, NetworkConfig, NodeIdentity, NodeRole, is_valid_passcode,
};
use crate::secret::Secret;
use crate::status::Status;
use crate::storage::Storage;
use crate::value::Value;

/// An event the product emitted, as recorded by [`FakeEngine::event_log`].
#[derive(Debug, Clone, PartialEq)]
pub struct EmittedEvent {
    /// The endpoint.
    pub endpoint: EndpointId,
    /// The cluster on the endpoint.
    pub cluster: ClusterId,
    /// The event.
    pub event: EventId,
    /// The event fields.
    pub payload: Value,
}

/// Runs a future to completion on the current thread.
///
/// A minimal executor for tests: the thread parks until the future's waker is called. It drives
/// one future at a time; concurrency between the product and the test comes from threads.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut future = core::pin::pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

/// The in-memory engine. Cloning it gives another handle on the same engine.
#[derive(Clone, Debug)]
pub struct FakeEngine {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    capabilities: Capabilities,
    running: bool,
    identity: Option<NodeIdentity>,
    role: Option<NodeRole>,
    endpoints: BTreeMap<EndpointId, Endpoint>,
    fabrics: BTreeMap<FabricIndex, FabricInfo>,
    commissioned: bool,
    window: Option<Window>,
    events: VecDeque<Event>,
    event_waker: Option<Waker>,
    pending: BTreeMap<Correlation, Pending>,
    next_correlation: u64,
    attribute_log: Vec<AttributeValue>,
    event_log: Vec<EmittedEvent>,
}

#[derive(Debug)]
struct Endpoint {
    description: EndpointDescription,
    values: BTreeMap<(ClusterId, AttributeId), Value>,
}

#[derive(Debug)]
struct Pending {
    kind: PendingKind,
    response: Option<Result<Response, Error>>,
    waker: Option<Waker>,
}

#[derive(Debug)]
enum PendingKind {
    Command,
    Write {
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    },
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngine {
    /// An engine without limits.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities::unbounded())
    }

    /// An engine with the given limits; `None` in a limit means unlimited.
    #[must_use]
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    capabilities,
                    running: false,
                    identity: None,
                    role: None,
                    endpoints: BTreeMap::new(),
                    fabrics: BTreeMap::new(),
                    commissioned: false,
                    window: None,
                    events: VecDeque::new(),
                    event_waker: None,
                    pending: BTreeMap::new(),
                    next_correlation: 1,
                    attribute_log: Vec::new(),
                    event_log: Vec::new(),
                }),
            }),
        }
    }

    /// Whether a node is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.shared.lock().running
    }

    /// Injects a command as a Matter client would and waits for the product's response.
    ///
    /// A path the node does not describe is answered by the engine without an event, as the
    /// contract says. The returned future resolves when the product calls
    /// [`Running::respond`].
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when no node runs, or when the node stops before the product
    /// answers.
    pub fn invoke(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        command: CommandId,
        payload: Value,
    ) -> BoxFuture<'static, Result<Response, Error>> {
        let outcome = {
            let mut state = self.shared.lock();
            if !state.running {
                Err(Error::NotRunning)
            } else {
                match state.check_command_path(endpoint, cluster, command) {
                    Some(status) => Ok(Err(Response::status(status))),
                    None => {
                        let correlation = state.issue(PendingKind::Command);
                        state.push_event(Event::CommandInvoked {
                            correlation,
                            endpoint,
                            cluster,
                            command,
                            payload,
                            fabric: None,
                        });
                        Ok(Ok(correlation))
                    }
                }
            }
        };
        self.await_outcome(outcome)
    }

    /// Injects a write as a Matter client would and waits for the product's response.
    ///
    /// On [`Status::Success`] the value is stored and logged; any other status leaves it
    /// unchanged. A path the node does not describe is answered by the engine without an
    /// event; a global attribute is answered [`Status::UnsupportedWrite`].
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when no node runs, or when the node stops before the product
    /// answers.
    pub fn write(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> BoxFuture<'static, Result<Response, Error>> {
        let outcome = {
            let mut state = self.shared.lock();
            if !state.running {
                Err(Error::NotRunning)
            } else {
                match state.check_attribute_path(endpoint, cluster, attribute) {
                    Some(status) => Ok(Err(Response::status(status))),
                    None => {
                        let correlation = state.issue(PendingKind::Write {
                            endpoint,
                            cluster,
                            attribute,
                            value: value.clone(),
                        });
                        state.push_event(Event::WriteRequested {
                            correlation,
                            endpoint,
                            cluster,
                            attribute,
                            value,
                            fabric: None,
                        });
                        Ok(Ok(correlation))
                    }
                }
            }
        };
        self.await_outcome(outcome)
    }

    /// Turns the outcome of injecting a request into the future that yields its response:
    /// an error or an engine-made response immediately, a correlation when the product answers.
    fn await_outcome(
        &self,
        outcome: Result<Result<Correlation, Response>, Error>,
    ) -> BoxFuture<'static, Result<Response, Error>> {
        let shared = Arc::clone(&self.shared);
        Box::pin(core::future::poll_fn(move |cx| match &outcome {
            Err(error) => Poll::Ready(Err(error.clone())),
            Ok(Err(response)) => Poll::Ready(Ok(response.clone())),
            Ok(Ok(correlation)) => shared.lock().poll_response(*correlation, cx),
        }))
    }

    /// The value the engine reports for an attribute, or `None` when the endpoint does not
    /// exist or does not list the attribute.
    #[must_use]
    pub fn read(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
    ) -> Option<Value> {
        self.shared
            .lock()
            .endpoints
            .get(&endpoint)
            .and_then(|e| e.values.get(&(cluster, attribute)).cloned())
    }

    /// The endpoints the product has added, in id order.
    #[must_use]
    pub fn endpoints(&self) -> Vec<EndpointDescription> {
        self.shared
            .lock()
            .endpoints
            .values()
            .map(|e| e.description.clone())
            .collect()
    }

    /// Sets the commissioning state as if the network had changed it and reports it as an
    /// event.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when no node runs.
    pub fn set_commissioning_state(&self, state: CommissioningState) -> Result<(), Error> {
        let mut inner = self.shared.lock();
        inner.require_running()?;
        inner.commissioned = state.commissioned;
        inner.window = state.window;
        inner.push_commissioning_changed();
        Ok(())
    }

    /// Adds a fabric as if a commissioner had just finished. Reports [`Event::FabricAdded`];
    /// a window that was open closes, and the node counts as commissioned.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when no node runs, [`Error::CapacityExceeded`] when
    /// `max_fabrics` is reached, [`Error::Engine`] when the index is in use.
    pub fn add_fabric(&self, fabric: FabricInfo) -> Result<(), Error> {
        let mut state = self.shared.lock();
        state.require_running()?;
        if state.fabrics.contains_key(&fabric.index) {
            return Err(Error::Engine(format!(
                "fabric {} exists already",
                fabric.index
            )));
        }
        if let Some(max) = state.capabilities.max_fabrics
            && state.fabrics.len() >= max
        {
            return Err(Error::CapacityExceeded { what: "fabrics" });
        }
        state.fabrics.insert(fabric.index, fabric.clone());
        state.push_event(Event::FabricAdded(fabric));
        state.commissioned = true;
        state.window = None;
        state.push_commissioning_changed();
        Ok(())
    }

    /// Every attribute change so far, in order: initial values are not logged, values set by
    /// the product and accepted writes are.
    #[must_use]
    pub fn attribute_log(&self) -> Vec<AttributeValue> {
        self.shared.lock().attribute_log.clone()
    }

    /// Every event the product emitted so far, in order.
    #[must_use]
    pub fn event_log(&self) -> Vec<EmittedEvent> {
        self.shared.lock().event_log.clone()
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn require_running(&self) -> Result<(), Error> {
        if self.running {
            Ok(())
        } else {
            Err(Error::NotRunning)
        }
    }

    fn aggregator(&self) -> Option<EndpointId> {
        self.role.map(|NodeRole::Bridge { aggregator }| aggregator)
    }

    fn push_event(&mut self, event: Event) {
        self.events.push_back(event);
        if let Some(waker) = self.event_waker.take() {
            waker.wake();
        }
    }

    fn commissioning_state(&self) -> CommissioningState {
        CommissioningState {
            commissioned: self.commissioned,
            window: self.window,
        }
    }

    fn push_commissioning_changed(&mut self) {
        let state = self.commissioning_state();
        self.push_event(Event::CommissioningChanged(state));
    }

    fn issue(&mut self, kind: PendingKind) -> Correlation {
        let correlation = Correlation::new(self.next_correlation);
        self.next_correlation += 1;
        self.pending.insert(
            correlation,
            Pending {
                kind,
                response: None,
                waker: None,
            },
        );
        correlation
    }

    fn poll_response(
        &mut self,
        correlation: Correlation,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Response, Error>> {
        let Some(pending) = self.pending.get_mut(&correlation) else {
            // Taken by `stop` or answered and collected already.
            return Poll::Ready(Err(Error::NotRunning));
        };
        if pending.response.is_some() {
            let pending = self.pending.remove(&correlation);
            let response = pending.and_then(|p| p.response);
            Poll::Ready(response.unwrap_or(Err(Error::NotRunning)))
        } else {
            pending.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// The status the engine answers a command path with, or `None` when the path reaches
    /// the product.
    fn check_command_path(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        command: CommandId,
    ) -> Option<Status> {
        let Some(endpoint) = self.endpoints.get(&endpoint) else {
            return Some(Status::UnsupportedEndpoint);
        };
        let Some(cluster) = endpoint.description.cluster(cluster) else {
            return Some(Status::UnsupportedCluster);
        };
        if !cluster.accepts_command(command) {
            return Some(Status::UnsupportedCommand);
        }
        None
    }

    /// The status the engine answers a write path with, or `None` when the path reaches the
    /// product.
    fn check_attribute_path(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
    ) -> Option<Status> {
        let Some(endpoint) = self.endpoints.get(&endpoint) else {
            return Some(Status::UnsupportedEndpoint);
        };
        let Some(cluster) = endpoint.description.cluster(cluster) else {
            return Some(Status::UnsupportedCluster);
        };
        if attribute.is_global() {
            return Some(Status::UnsupportedWrite);
        }
        if !cluster.has_attribute(attribute) {
            return Some(Status::UnsupportedAttribute);
        }
        None
    }

    fn validate_description(&self, description: &EndpointDescription) -> Result<(), Error> {
        let id = description.id;
        let invalid = |reason: String| Error::InvalidDescription {
            endpoint: id,
            reason,
        };
        if id == ROOT_ENDPOINT {
            return Err(invalid(String::from(
                "endpoint 0 is the root endpoint and belongs to the engine",
            )));
        }
        if self.endpoints.contains_key(&id) || self.aggregator() == Some(id) {
            return Err(Error::DuplicateEndpoint(id));
        }
        if description.device_types.is_empty() {
            return Err(invalid(String::from("no device type")));
        }
        for (i, cluster) in description.clusters.iter().enumerate() {
            if description.clusters[..i].iter().any(|c| c.id == cluster.id) {
                return Err(invalid(format!("cluster {} is listed twice", cluster.id)));
            }
            for (j, attribute) in cluster.attributes.iter().enumerate() {
                if attribute.is_global() {
                    return Err(invalid(format!(
                        "global attribute {attribute} of cluster {} is derived by the engine and must not be listed",
                        cluster.id
                    )));
                }
                if cluster.attributes[..j].contains(attribute) {
                    return Err(invalid(format!(
                        "attribute {attribute} of cluster {} is listed twice",
                        cluster.id
                    )));
                }
            }
        }
        if let Some(parent) = description.parent
            && self.aggregator() != Some(parent)
            && !self.endpoints.contains_key(&parent)
        {
            return Err(Error::UnknownEndpoint(parent));
        }
        for (i, (cluster, attribute, _)) in description.initial_values.iter().enumerate() {
            let listed = description
                .cluster(*cluster)
                .is_some_and(|c| c.has_attribute(*attribute));
            if !listed {
                return Err(invalid(format!(
                    "initial value for attribute {attribute} of cluster {cluster}, which is not listed"
                )));
            }
            if description.initial_values[..i]
                .iter()
                .any(|(c, a, _)| c == cluster && a == attribute)
            {
                return Err(invalid(format!(
                    "attribute {attribute} of cluster {cluster} has two initial values"
                )));
            }
        }
        for cluster in &description.clusters {
            for attribute in &cluster.attributes {
                let present = description
                    .initial_values
                    .iter()
                    .any(|(c, a, _)| *c == cluster.id && a == attribute);
                if !present {
                    return Err(invalid(format!(
                        "attribute {attribute} of cluster {} has no initial value",
                        cluster.id
                    )));
                }
            }
        }
        if let Some(max) = self.capabilities.max_endpoints
            && self.endpoints.len() >= max
        {
            return Err(Error::CapacityExceeded { what: "endpoints" });
        }
        Ok(())
    }

    fn add_endpoint(&mut self, description: EndpointDescription) -> Result<(), Error> {
        self.require_running()?;
        self.validate_description(&description)?;
        let values = description
            .initial_values
            .iter()
            .map(|(c, a, v)| ((*c, *a), v.clone()))
            .collect();
        self.endpoints.insert(
            description.id,
            Endpoint {
                description,
                values,
            },
        );
        Ok(())
    }

    fn remove_endpoint(&mut self, endpoint: EndpointId) -> Result<(), Error> {
        self.require_running()?;
        if !self.endpoints.contains_key(&endpoint) {
            return Err(Error::UnknownEndpoint(endpoint));
        }
        let mut doomed = vec![endpoint];
        loop {
            let more: Vec<EndpointId> = self
                .endpoints
                .values()
                .filter(|e| {
                    e.description
                        .parent
                        .is_some_and(|parent| doomed.contains(&parent))
                        && !doomed.contains(&e.description.id)
                })
                .map(|e| e.description.id)
                .collect();
            if more.is_empty() {
                break;
            }
            doomed.extend(more);
        }
        for id in doomed {
            self.endpoints.remove(&id);
        }
        Ok(())
    }

    fn check_settable(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
    ) -> Result<(), Error> {
        let Some(e) = self.endpoints.get(&endpoint) else {
            return Err(Error::UnknownEndpoint(endpoint));
        };
        let listed = e
            .description
            .cluster(cluster)
            .is_some_and(|c| c.has_attribute(attribute));
        if listed {
            Ok(())
        } else {
            Err(Error::UnknownAttribute {
                endpoint,
                cluster,
                attribute,
            })
        }
    }

    fn store_value(&mut self, value: AttributeValue) {
        if let Some(e) = self.endpoints.get_mut(&value.endpoint) {
            e.values
                .insert((value.cluster, value.attribute), value.value.clone());
        }
        self.attribute_log.push(value);
    }

    fn set_attributes(&mut self, values: Vec<AttributeValue>) -> Result<(), Error> {
        self.require_running()?;
        for v in &values {
            self.check_settable(v.endpoint, v.cluster, v.attribute)?;
        }
        for v in values {
            self.store_value(v);
        }
        Ok(())
    }

    fn check_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
    ) -> Result<(), Error> {
        let Some(e) = self.endpoints.get(&endpoint) else {
            return Err(Error::UnknownEndpoint(endpoint));
        };
        let listed = e
            .description
            .cluster(cluster)
            .is_some_and(|c| c.has_event(event));
        if listed {
            Ok(())
        } else {
            Err(Error::UnknownEvent {
                endpoint,
                cluster,
                event,
            })
        }
    }

    fn emit_event(&mut self, emitted: EmittedEvent) -> Result<(), Error> {
        self.require_running()?;
        self.check_event(emitted.endpoint, emitted.cluster, emitted.event)?;
        self.event_log.push(emitted);
        Ok(())
    }

    fn set_reachable(&mut self, endpoint: EndpointId, reachable: bool) -> Result<(), Error> {
        self.require_running()?;
        self.check_settable(endpoint, bridged_device::CLUSTER, bridged_device::REACHABLE)?;
        self.check_event(
            endpoint,
            bridged_device::CLUSTER,
            bridged_device::REACHABLE_CHANGED,
        )?;
        self.store_value(AttributeValue {
            endpoint,
            cluster: bridged_device::CLUSTER,
            attribute: bridged_device::REACHABLE,
            value: Value::Bool(reachable),
        });
        self.event_log.push(EmittedEvent {
            endpoint,
            cluster: bridged_device::CLUSTER,
            event: bridged_device::REACHABLE_CHANGED,
            payload: Value::Struct(vec![(
                bridged_device::REACHABLE_NEW_VALUE_TAG,
                Value::Bool(reachable),
            )]),
        });
        Ok(())
    }

    fn respond(&mut self, correlation: Correlation, response: Response) -> Result<(), Error> {
        self.require_running()?;
        let Some(pending) = self.pending.get_mut(&correlation) else {
            return Err(Error::UnknownCorrelation(correlation));
        };
        if pending.response.is_some() {
            return Err(Error::UnknownCorrelation(correlation));
        }
        let waker = pending.waker.take();
        let accepted_write = match &pending.kind {
            PendingKind::Write {
                endpoint,
                cluster,
                attribute,
                value,
            } if response.status.is_success() => Some(AttributeValue {
                endpoint: *endpoint,
                cluster: *cluster,
                attribute: *attribute,
                value: value.clone(),
            }),
            PendingKind::Write { .. } | PendingKind::Command => None,
        };
        pending.response = Some(Ok(response));
        if let Some(value) = accepted_write {
            self.store_value(value);
        }
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    fn open_window(&mut self, window: Window) -> Result<(), Error> {
        self.require_running()?;
        if matches!(window, Window::Enhanced) && !self.capabilities.enhanced_window {
            return Err(Error::Unsupported {
                operation: "enhanced commissioning window",
            });
        }
        self.window = Some(window);
        self.push_commissioning_changed();
        Ok(())
    }

    fn close_window(&mut self) -> Result<(), Error> {
        self.require_running()?;
        if self.window.take().is_some() {
            self.push_commissioning_changed();
        }
        Ok(())
    }

    fn remove_fabric(&mut self, fabric: FabricIndex) -> Result<(), Error> {
        self.require_running()?;
        if self.fabrics.remove(&fabric).is_none() {
            return Err(Error::UnknownFabric(fabric));
        }
        self.push_event(Event::FabricRemoved(fabric));
        if self.fabrics.is_empty() && self.commissioned {
            self.commissioned = false;
            self.push_commissioning_changed();
        }
        Ok(())
    }

    fn pairing_info(&self) -> Result<PairingInfo, Error> {
        self.require_running()?;
        let identity = self.identity.as_ref().ok_or(Error::NotRunning)?;
        let passcode = match self.window {
            Some(Window::Enhanced) => String::from("TEMPORARY"),
            Some(Window::Basic) | None => identity.passcode.expose().to_string(),
        };
        let discriminator = identity.discriminator;
        Ok(PairingInfo {
            qr_text: Secret::new(format!("MT:FAKE-{discriminator}-{passcode}")),
            manual_code: Secret::new(format!("FAKE-{passcode}")),
            discriminator,
        })
    }

    fn factory_reset(&mut self) -> Result<(), Error> {
        self.require_running()?;
        let indexes: Vec<FabricIndex> = self.fabrics.keys().copied().collect();
        for index in indexes {
            self.fabrics.remove(&index);
            self.push_event(Event::FabricRemoved(index));
        }
        self.commissioned = false;
        self.window = None;
        self.push_commissioning_changed();
        Ok(())
    }

    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<Result<Event, Error>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(Ok(event));
        }
        if !self.running {
            return Poll::Ready(Err(Error::NotRunning));
        }
        self.event_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn stop(&mut self) -> Result<(), Error> {
        self.require_running()?;
        self.running = false;
        self.window = None;
        for pending in self.pending.values_mut() {
            pending.response = Some(Err(Error::NotRunning));
            if let Some(waker) = pending.waker.take() {
                waker.wake();
            }
        }
        self.push_event(Event::Stopped(StopReason::Requested));
        Ok(())
    }

    fn start(
        &mut self,
        identity: NodeIdentity,
        network: &NetworkConfig,
        role: NodeRole,
    ) -> Result<(), Error> {
        if self.running {
            return Err(Error::AlreadyRunning);
        }
        if identity.discriminator > MAX_DISCRIMINATOR {
            return Err(Error::InvalidIdentity {
                reason: format!(
                    "discriminator {} exceeds the 12-bit maximum {MAX_DISCRIMINATOR}",
                    identity.discriminator
                ),
            });
        }
        if !is_valid_passcode(*identity.passcode.expose()) {
            return Err(Error::InvalidIdentity {
                reason: String::from(
                    "the passcode is outside 1..=99999998 or one of the forbidden passcodes",
                ),
            });
        }
        if !network.ipv4 && !network.ipv6 {
            return Err(Error::InvalidIdentity {
                reason: String::from("neither IPv4 nor IPv6 is enabled"),
            });
        }
        self.running = true;
        self.identity = Some(identity);
        self.role = Some(role);
        self.endpoints.clear();
        self.events.clear();
        self.event_waker = None;
        self.pending.clear();
        self.commissioned = !self.fabrics.is_empty();
        self.window = if self.commissioned {
            None
        } else {
            Some(Window::Basic)
        };
        Ok(())
    }
}

impl Engine for FakeEngine {
    fn start(
        &self,
        identity: NodeIdentity,
        network: NetworkConfig,
        _attestation: Attestation,
        role: NodeRole,
        _storage: Arc<dyn Storage>,
    ) -> BoxFuture<'_, Result<Box<dyn Running>, Error>> {
        Box::pin(async move {
            self.shared.lock().start(identity, &network, role)?;
            let running: Box<dyn Running> = Box::new(FakeRunning {
                shared: Arc::clone(&self.shared),
            });
            Ok(running)
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.shared.lock().capabilities
    }
}

/// The running handle of a [`FakeEngine`].
#[derive(Debug)]
struct FakeRunning {
    shared: Arc<Shared>,
}

impl Running for FakeRunning {
    fn add_endpoint(&self, description: EndpointDescription) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().add_endpoint(description) })
    }

    fn remove_endpoint(&self, endpoint: EndpointId) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().remove_endpoint(endpoint) })
    }

    fn set_attribute(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.shared.lock().set_attributes(vec![AttributeValue {
                endpoint,
                cluster,
                attribute,
                value,
            }])
        })
    }

    fn set_attributes(&self, values: Vec<AttributeValue>) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().set_attributes(values) })
    }

    fn emit_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
        payload: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.shared.lock().emit_event(EmittedEvent {
                endpoint,
                cluster,
                event,
                payload,
            })
        })
    }

    fn set_reachable(
        &self,
        endpoint: EndpointId,
        reachable: bool,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().set_reachable(endpoint, reachable) })
    }

    fn respond(
        &self,
        correlation: Correlation,
        response: Response,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().respond(correlation, response) })
    }

    fn commissioning_state(&self) -> BoxFuture<'_, Result<CommissioningState, Error>> {
        Box::pin(async move {
            let state = self.shared.lock();
            state.require_running()?;
            Ok(state.commissioning_state())
        })
    }

    fn open_commissioning_window(
        &self,
        window: Window,
        _timeout: Duration,
    ) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().open_window(window) })
    }

    fn close_commissioning_window(&self) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().close_window() })
    }

    fn fabrics(&self) -> BoxFuture<'_, Result<Vec<FabricInfo>, Error>> {
        Box::pin(async move {
            let state = self.shared.lock();
            state.require_running()?;
            Ok(state.fabrics.values().cloned().collect())
        })
    }

    fn remove_fabric(&self, fabric: FabricIndex) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().remove_fabric(fabric) })
    }

    fn pairing_info(&self) -> BoxFuture<'_, Result<PairingInfo, Error>> {
        Box::pin(async move { self.shared.lock().pairing_info() })
    }

    fn factory_reset(&self) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().factory_reset() })
    }

    fn capabilities(&self) -> Capabilities {
        self.shared.lock().capabilities
    }

    fn next_event(&self) -> BoxFuture<'_, Result<Event, Error>> {
        Box::pin(core::future::poll_fn(move |cx| {
            self.shared.lock().poll_event(cx)
        }))
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move { self.shared.lock().stop() })
    }
}
