//! The bridge: one aggregator with bridged devices on the engine facade.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Poll, Waker};
use core::time::Duration;

use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, BoxFuture, Capabilities, ClusterId,
    CommissioningState, Correlation, EndpointId, Engine, Event, FabricIndex, FabricInfo,
    NetworkConfig, NodeIdentity, NodeRole, PairingInfo, Response, Running, Status, Storage, Value,
    Window, bridged_device,
};

use crate::command::{BridgeEvent, CommandEvent, DeviceCommand};
use crate::encoding;
use crate::error::Error;
use crate::events::translate_event;
use crate::ids::{
    boolean_state, bridged_device_basic_information as bdbi, color_control, level_control, on_off,
};
use crate::model::{Attribute, DeviceEvent, DeviceKey, DeviceSpec, InitialValues, PartIndex};
use crate::numbering::{AGGREGATOR, MAP_KEY, NEXT_KEY, Numbering, Slot};
use crate::plan::{EndpointPlan, Numbers, PlannedEndpoint, canonical, plan_device};
use crate::semantics::{CommandOutcome, WriteOutcome, translate_command, translate_write};
use crate::sync::Lock;
use crate::timer::Timer;

/// The prefix of the storage keys under which the bridge persists written attribute values, one
/// entry per device: `bridge/attributes/<device key>`.
pub const ATTRIBUTES_KEY_PREFIX: &str = "bridge/attributes/";

/// The default time an adapter has to answer a raw command or write before the bridge answers
/// `Failure`; shorter than the engines' own timeouts, so that the bridge's answer wins.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The version byte of a persisted attribute entry.
const ATTRIBUTES_VERSION: u8 = 1;

/// Settings of a bridge beyond what [`Bridge::start`] takes.
pub struct Options {
    /// How long an adapter has to answer a raw command or write.
    pub response_timeout: Duration,
    /// The timer that measures the response timeout; without one the bridge never times out
    /// on its own and the engine's timeout is the only one. Under `std` the default is
    /// [`ThreadTimer`](crate::ThreadTimer).
    pub timer: Option<Arc<dyn Timer>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            #[cfg(feature = "std")]
            timer: Some(Arc::new(crate::timer::ThreadTimer)),
            #[cfg(not(feature = "std"))]
            timer: None,
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("response_timeout", &self.response_timeout)
            .field("timer", &self.timer.is_some())
            .finish()
    }
}

/// A running bridge.
///
/// Created by [`Bridge::start`]; adapters add devices with [`Bridge::add_device`] and receive
/// commands from [`Bridge::next_command`]. The bridge is shared between tasks through its
/// device handles and may itself be wrapped in an `Arc`; its futures are `Send`.
pub struct Bridge {
    inner: Arc<Inner>,
}

impl fmt::Debug for Bridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bridge")
            .field("devices", &self.devices())
            .finish_non_exhaustive()
    }
}

struct Inner {
    node: Arc<dyn Running>,
    storage: Arc<dyn Storage>,
    state: Lock<State>,
    response_timeout: Duration,
    timer: Option<Arc<dyn Timer>>,
}

struct State {
    devices: BTreeMap<DeviceKey, Device>,
    by_endpoint: BTreeMap<EndpointId, DeviceKey>,
    numbering: Numbering,
    pending: Vec<Pending>,
    queued: VecDeque<CommandEvent>,
    waker: Option<Waker>,
}

struct Device {
    spec: DeviceSpec,
    endpoints: Vec<PlannedEndpoint>,
    /// The written values the device keeps across restarts, by slot part, cluster and attribute.
    persisted: BTreeMap<(u16, ClusterId, AttributeId), Value>,
}

impl Device {
    fn endpoint(&self, part: Option<PartIndex>) -> Option<&PlannedEndpoint> {
        self.endpoints.iter().find(|e| e.plan.part == part)
    }

    fn endpoint_mut(&mut self, id: EndpointId) -> Option<&mut PlannedEndpoint> {
        self.endpoints.iter_mut().find(|e| e.plan.endpoint == id)
    }

    /// The endpoint that carries the Bridged Device Basic Information cluster.
    fn top(&self) -> &PlannedEndpoint {
        self.endpoints
            .iter()
            .find(|e| e.plan.part.is_none())
            .unwrap_or(&self.endpoints[0])
    }
}

/// A raw command or write that waits for the adapter's answer.
struct Pending {
    correlation: Correlation,
    shared: Arc<PendingShared>,
    deadline: Option<BoxFuture<'static, ()>>,
    timed_out: bool,
}

struct PendingShared {
    answered: AtomicBool,
    dropped: AtomicBool,
}

impl State {
    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }

    fn device_of(&self, endpoint: EndpointId) -> Option<(&DeviceKey, &Device, &PlannedEndpoint)> {
        let key = self.by_endpoint.get(&endpoint)?;
        let device = self.devices.get(key)?;
        let planned = device
            .endpoints
            .iter()
            .find(|e| e.plan.endpoint == endpoint)?;
        Some((key, device, planned))
    }

    fn store_value(
        &mut self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) {
        if let Some(key) = self.by_endpoint.get(&endpoint).cloned()
            && let Some(device) = self.devices.get_mut(&key)
            && let Some(planned) = device.endpoint_mut(endpoint)
        {
            planned.values.insert((cluster, attribute), value);
        }
    }
}

impl Bridge {
    /// Starts a bridge node on `engine` with default [`Options`].
    ///
    /// The node has the root endpoint and the aggregator endpoint; devices are added afterwards.
    /// The engine's state and the bridge's own (endpoint numbers under `bridge/endpoints/`,
    /// written attribute values under `bridge/attributes/`) live in `storage`.
    ///
    /// # Errors
    ///
    /// The errors of [`Engine::start`], wrapped; [`Error::CorruptState`] when the persisted
    /// numbering cannot be read; [`Error::Storage`] when the storage fails.
    pub async fn start(
        engine: Box<dyn Engine>,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        storage: Arc<dyn Storage>,
    ) -> Result<Self, Error> {
        Self::start_with(
            engine,
            identity,
            network,
            attestation,
            storage,
            Options::default(),
        )
        .await
    }

    /// Starts a bridge node with the given options.
    ///
    /// # Errors
    ///
    /// As [`Bridge::start`].
    pub async fn start_with(
        engine: Box<dyn Engine>,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        storage: Arc<dyn Storage>,
        options: Options,
    ) -> Result<Self, Error> {
        let map = storage.load(MAP_KEY)?;
        let next = storage.load(NEXT_KEY)?;
        let numbering = Numbering::decode(map.as_deref(), next.as_deref())?;
        let node = engine
            .start(
                identity,
                network,
                attestation,
                NodeRole::Bridge {
                    aggregator: AGGREGATOR,
                },
                Arc::clone(&storage),
            )
            .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                node: Arc::from(node),
                storage,
                state: Lock::new(State {
                    devices: BTreeMap::new(),
                    by_endpoint: BTreeMap::new(),
                    numbering,
                    pending: Vec::new(),
                    queued: VecDeque::new(),
                    waker: None,
                }),
                response_timeout: options.response_timeout,
                timer: options.timer,
            }),
        })
    }

    /// Adds a device: plans its endpoints, gives them their stable numbers, applies persisted
    /// start-up values and adds the endpoints to the node.
    ///
    /// The returned handle is how the adapter reports values and events for the device. A
    /// device whose key was numbered before gets its old endpoint numbers back.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateDevice`] when the key is added already, [`Error::InvalidSpec`] for an
    /// inconsistent specification, [`Error::MissingInitialValue`], [`Error::InvalidValue`],
    /// [`Error::UnknownPart`] and [`Error::UnknownAttribute`] for the initial values,
    /// [`Error::EndpointsExhausted`] when no number is free, [`Error::Storage`] when the
    /// numbering cannot be persisted, and the engine's errors for the endpoints, wrapped.
    /// Nothing of the device is left on the node after an error.
    pub async fn add_device(
        &self,
        spec: DeviceSpec,
        initial: InitialValues,
    ) -> Result<DeviceHandle, Error> {
        let inner = &self.inner;
        let key = spec.key.clone();
        let (numbers, map, next) = inner.state.with(|state| {
            if state.devices.contains_key(&key) {
                return Err(Error::DuplicateDevice(key.clone()));
            }
            let parent = if spec.is_composed() {
                Some(state.numbering.assign(Slot::parent(&key))?)
            } else {
                None
            };
            let mut parts = Vec::with_capacity(spec.parts.len());
            for index in 0..spec.parts.len() {
                let part = PartIndex::try_from(index).map_err(|_| Error::InvalidSpec {
                    key: key.clone(),
                    reason: String::from("too many parts"),
                })?;
                parts.push(state.numbering.assign(Slot::part(&key, part))?);
            }
            Ok((
                Numbers { parent, parts },
                state.numbering.encode_map(),
                state.numbering.encode_next(),
            ))
        })?;
        inner.storage.store(MAP_KEY, &map)?;
        inner.storage.store(NEXT_KEY, &next)?;

        let persisted = load_persisted(&*inner.storage, &key)?;
        let lookup = |slot: u16, attribute: Attribute| {
            let (cluster, id) = attribute.path();
            persisted.get(&(slot, cluster, id)).cloned()
        };
        let endpoints = plan_device(&spec, &initial, &numbers, &lookup)?;
        let (start_up_changes, start_up_commands) = start_up(&key, &endpoints);

        let mut added = Vec::new();
        for endpoint in &endpoints {
            if let Err(error) = inner
                .node
                .add_endpoint(endpoint.plan.description.clone())
                .await
            {
                // Roll back: removing a parent removes its children; remove whatever is left.
                for id in added.iter().rev() {
                    let _ = inner.node.remove_endpoint(*id).await;
                }
                return Err(error.into());
            }
            added.push(endpoint.plan.endpoint);
        }
        if !start_up_changes.is_empty() {
            inner.node.set_attributes(start_up_changes.clone()).await?;
        }

        inner.state.with(|state| {
            let mut device = Device {
                spec,
                endpoints,
                persisted,
            };
            for change in start_up_changes {
                if let Some(planned) = device.endpoint_mut(change.endpoint) {
                    planned
                        .values
                        .insert((change.cluster, change.attribute), change.value);
                }
            }
            for endpoint in &device.endpoints {
                state
                    .by_endpoint
                    .insert(endpoint.plan.endpoint, key.clone());
            }
            state.devices.insert(key.clone(), device);
            let had_commands = !start_up_commands.is_empty();
            state.queued.extend(start_up_commands);
            if had_commands {
                state.wake();
            }
        });
        Ok(DeviceHandle {
            inner: Arc::clone(inner),
            key,
        })
    }

    /// Removes a device from the node. Its endpoint numbers stay reserved, so that the same key
    /// gets them back when it is added again; its persisted values stay as well.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`] when no device has this key; the engine's errors, wrapped.
    pub async fn remove_device(&self, key: &DeviceKey) -> Result<(), Error> {
        let device = self.inner.state.with(|state| {
            let device = state
                .devices
                .remove(key)
                .ok_or_else(|| Error::UnknownDevice(key.clone()))?;
            for endpoint in &device.endpoints {
                state.by_endpoint.remove(&endpoint.plan.endpoint);
            }
            Ok::<_, Error>(device)
        })?;
        // The top endpoint takes its children with it.
        self.inner
            .node
            .remove_endpoint(device.top().plan.endpoint)
            .await?;
        Ok(())
    }

    /// Forgets a device: removes it if it is added, releases its endpoint numbers and deletes
    /// its persisted values. A device added later under the same key is a new device.
    ///
    /// # Errors
    ///
    /// The engine's errors when the device was added and cannot be removed, wrapped;
    /// [`Error::Storage`] when the storage fails.
    pub async fn forget_device(&self, key: &DeviceKey) -> Result<(), Error> {
        match self.remove_device(key).await {
            Ok(()) | Err(Error::UnknownDevice(_)) => {}
            Err(error) => return Err(error),
        }
        let (map, next) = self.inner.state.with(|state| {
            state.numbering.forget(key);
            (state.numbering.encode_map(), state.numbering.encode_next())
        });
        self.inner.storage.store(MAP_KEY, &map)?;
        self.inner.storage.store(NEXT_KEY, &next)?;
        self.inner.storage.remove(&attributes_key(key))?;
        Ok(())
    }

    /// A handle for an added device.
    #[must_use]
    pub fn device(&self, key: &DeviceKey) -> Option<DeviceHandle> {
        self.inner.state.with(|state| {
            state.devices.contains_key(key).then(|| DeviceHandle {
                inner: Arc::clone(&self.inner),
                key: key.clone(),
            })
        })
    }

    /// The keys of the added devices, in order.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceKey> {
        self.inner
            .state
            .with(|state| state.devices.keys().cloned().collect())
    }

    /// The endpoint numbers currently reserved, added devices and removed ones alike.
    #[must_use]
    pub fn reserved_endpoints(&self) -> Vec<EndpointId> {
        self.inner
            .state
            .with(|state| state.numbering.reserved().collect())
    }

    /// Waits for the next command for a device, skipping the node's lifecycle events.
    ///
    /// # Errors
    ///
    /// [`Error::Engine`] with [`matter_rs_engine::Error::NotRunning`] once the node has stopped.
    pub async fn next_command(&self) -> Result<CommandEvent, Error> {
        loop {
            if let BridgeEvent::Command(command) = self.next_event().await? {
                return Ok(command);
            }
        }
    }

    /// Waits for the next event: a device command or a change of the node.
    ///
    /// One task pulls events; it is the task that applies the cluster semantics, answers the
    /// Matter clients and sweeps the raw responses that timed out.
    ///
    /// # Errors
    ///
    /// [`Error::Engine`] with [`matter_rs_engine::Error::NotRunning`] once the node has stopped.
    pub async fn next_event(&self) -> Result<BridgeEvent, Error> {
        loop {
            if let Some(command) = self.inner.state.with(|state| state.queued.pop_front()) {
                return Ok(BridgeEvent::Command(command));
            }
            for correlation in self.inner.expired() {
                let _ = self
                    .inner
                    .node
                    .respond(correlation, Response::status(Status::Failure))
                    .await;
            }
            let Some(event) = self.wait_engine().await? else {
                continue;
            };
            let outcome = match event {
                Event::CommandInvoked {
                    correlation,
                    endpoint,
                    cluster,
                    command,
                    payload,
                    ..
                } => {
                    self.inner
                        .handle_command(correlation, endpoint, cluster, command, &payload)
                        .await
                }
                Event::WriteRequested {
                    correlation,
                    endpoint,
                    cluster,
                    attribute,
                    value,
                    ..
                } => {
                    self.inner
                        .handle_write(correlation, endpoint, cluster, attribute, value)
                        .await
                }
                Event::CommissioningChanged(state) => {
                    return Ok(BridgeEvent::CommissioningChanged(state));
                }
                Event::FabricAdded(fabric) => return Ok(BridgeEvent::FabricAdded(fabric)),
                Event::FabricRemoved(index) => return Ok(BridgeEvent::FabricRemoved(index)),
                Event::Stopped(reason) => return Ok(BridgeEvent::Stopped(reason)),
                _ => continue,
            };
            if let Some(command) = outcome {
                return Ok(BridgeEvent::Command(command));
            }
        }
    }

    /// Waits for the engine's next event, or returns `None` when the bridge has something to
    /// do of its own (a queued command, a dropped or timed-out response).
    fn wait_engine(&self) -> impl Future<Output = Result<Option<Event>, Error>> + Send + '_ {
        let inner = Arc::clone(&self.inner);
        let mut engine_event = self.inner.node.next_event();
        core::future::poll_fn(move |cx| {
            let own_work = inner.state.with(|state| {
                state.waker = Some(cx.waker().clone());
                let mut own_work = !state.queued.is_empty();
                for pending in &mut state.pending {
                    if let Some(deadline) = &mut pending.deadline
                        && deadline.as_mut().poll(cx).is_ready()
                    {
                        pending.deadline = None;
                        pending.timed_out = true;
                    }
                    if pending.timed_out || pending.shared.dropped.load(Ordering::Acquire) {
                        own_work = true;
                    }
                }
                own_work
            });
            if own_work {
                return Poll::Ready(Ok(None));
            }
            engine_event
                .as_mut()
                .poll(cx)
                .map(|result| result.map(Some).map_err(Error::from))
        })
    }

    /// The current commissioning state of the node.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn commissioning_state(&self) -> Result<CommissioningState, Error> {
        Ok(self.inner.node.commissioning_state().await?)
    }

    /// The pairing information a commissioner needs; both codes are secrets.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn pairing_info(&self) -> Result<PairingInfo, Error> {
        Ok(self.inner.node.pairing_info().await?)
    }

    /// Opens a commissioning window.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn open_commissioning_window(
        &self,
        window: Window,
        timeout: Duration,
    ) -> Result<(), Error> {
        Ok(self
            .inner
            .node
            .open_commissioning_window(window, timeout)
            .await?)
    }

    /// Closes the commissioning window.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn close_commissioning_window(&self) -> Result<(), Error> {
        Ok(self.inner.node.close_commissioning_window().await?)
    }

    /// The fabrics the node is part of.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn fabrics(&self) -> Result<Vec<FabricInfo>, Error> {
        Ok(self.inner.node.fabrics().await?)
    }

    /// Removes the node from a fabric.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn remove_fabric(&self, fabric: FabricIndex) -> Result<(), Error> {
        Ok(self.inner.node.remove_fabric(fabric).await?)
    }

    /// Resets the node to factory state: the engine forgets its fabrics, the bridge forgets the
    /// endpoint numbers of every device that is not added right now and every persisted
    /// attribute value. The added devices keep their endpoints and numbers.
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped; [`Error::Storage`] when the storage fails.
    pub async fn factory_reset(&self) -> Result<(), Error> {
        self.inner.node.factory_reset().await?;
        let (keys, map, next) = self.inner.state.with(|state| {
            let live: Vec<DeviceKey> = state.devices.keys().cloned().collect();
            let forgotten = state.numbering.retain(&live);
            for device in state.devices.values_mut() {
                device.persisted.clear();
            }
            let mut keys = live;
            keys.extend(forgotten);
            (
                keys,
                state.numbering.encode_map(),
                state.numbering.encode_next(),
            )
        });
        self.inner.storage.store(MAP_KEY, &map)?;
        self.inner.storage.store(NEXT_KEY, &next)?;
        for key in keys {
            self.inner.storage.remove(&attributes_key(&key))?;
        }
        Ok(())
    }

    /// The limits of the engine.
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        self.inner.node.capabilities()
    }

    /// Stops the node. The last event is [`BridgeEvent::Stopped`].
    ///
    /// # Errors
    ///
    /// The engine's errors, wrapped.
    pub async fn stop(&self) -> Result<(), Error> {
        Ok(self.inner.node.stop().await?)
    }
}

impl Inner {
    /// The pending responses that are dropped or timed out, marked answered so that nobody
    /// answers them twice, and removed from the list.
    fn expired(&self) -> Vec<Correlation> {
        self.state.with(|state| {
            let mut expired = Vec::new();
            state.pending.retain(|pending| {
                if pending.shared.answered.load(Ordering::Acquire) {
                    return false;
                }
                if pending.timed_out || pending.shared.dropped.load(Ordering::Acquire) {
                    pending.shared.answered.store(true, Ordering::Release);
                    expired.push(pending.correlation);
                    return false;
                }
                true
            });
            expired
        })
    }

    fn register_pending(self: &Arc<Self>, correlation: Correlation) -> ResponseHandle {
        let shared = Arc::new(PendingShared {
            answered: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        });
        let deadline = self
            .timer
            .as_ref()
            .map(|timer| timer.sleep(self.response_timeout));
        self.state.with(|state| {
            state.pending.push(Pending {
                correlation,
                shared: Arc::clone(&shared),
                deadline,
                timed_out: false,
            });
        });
        ResponseHandle {
            inner: Arc::clone(self),
            correlation,
            shared,
            write: None,
        }
    }

    async fn handle_command(
        self: &Arc<Self>,
        correlation: Correlation,
        endpoint: EndpointId,
        cluster: ClusterId,
        command: matter_rs_engine::CommandId,
        payload: &Value,
    ) -> Option<CommandEvent> {
        let translated = self.state.with(|state| {
            let (key, _, planned) = state.device_of(endpoint)?;
            Some((
                key.clone(),
                planned.plan.part,
                translate_command(planned, cluster, command, payload),
            ))
        });
        let Some((key, part, outcome)) = translated else {
            let _ = self
                .node
                .respond(correlation, Response::status(Status::UnsupportedEndpoint))
                .await;
            return None;
        };
        match outcome {
            CommandOutcome::Handled(translation) => {
                let status = self
                    .apply(endpoint, translation.changes, translation.status)
                    .await;
                let _ = self
                    .node
                    .respond(correlation, Response::status(status))
                    .await;
                if status.is_success() {
                    self.queue(key, part, translation.commands)
                } else {
                    None
                }
            }
            CommandOutcome::Forward => {
                let response = self.register_pending(correlation);
                Some(CommandEvent {
                    key,
                    part,
                    command: DeviceCommand::Raw {
                        cluster,
                        command,
                        payload: payload.clone(),
                        response,
                    },
                })
            }
        }
    }

    async fn handle_write(
        self: &Arc<Self>,
        correlation: Correlation,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> Option<CommandEvent> {
        let translated = self.state.with(|state| {
            let (key, _, planned) = state.device_of(endpoint)?;
            Some((
                key.clone(),
                planned.plan.part,
                planned.slot_part,
                translate_write(planned, cluster, attribute, &value),
            ))
        });
        let Some((key, part, slot_part, outcome)) = translated else {
            let _ = self
                .node
                .respond(correlation, Response::status(Status::UnsupportedEndpoint))
                .await;
            return None;
        };
        match outcome {
            WriteOutcome::Accepted {
                stored,
                persist,
                commands,
            } => {
                let mut status = Status::Success;
                if persist
                    && let Err(error) = self.persist(&key, slot_part, cluster, attribute, &stored)
                {
                    let _ = error;
                    status = Status::Failure;
                }
                let _ = self
                    .node
                    .respond(correlation, Response::status(status))
                    .await;
                if !status.is_success() {
                    return None;
                }
                self.state
                    .with(|state| state.store_value(endpoint, cluster, attribute, stored));
                // A rename belongs to the device, not to a part.
                let part = if (cluster, attribute) == (bdbi::CLUSTER, bdbi::NODE_LABEL) {
                    None
                } else {
                    part
                };
                self.queue(key, part, commands)
            }
            WriteOutcome::Rejected(status) => {
                let _ = self
                    .node
                    .respond(correlation, Response::status(status))
                    .await;
                None
            }
            WriteOutcome::Forward => {
                let mut response = self.register_pending(correlation);
                response.write = Some((endpoint, cluster, attribute, value.clone()));
                Some(CommandEvent {
                    key,
                    part,
                    command: DeviceCommand::Write {
                        attribute: Attribute::from_path(cluster, attribute),
                        value,
                        response,
                    },
                })
            }
        }
    }

    /// Sets the changes a translation produced and returns the status to answer: the
    /// translation's own, or `Failure` when the engine refused the values.
    async fn apply(
        &self,
        endpoint: EndpointId,
        changes: Vec<(ClusterId, AttributeId, Value)>,
        status: Status,
    ) -> Status {
        if !status.is_success() || changes.is_empty() {
            return status;
        }
        let values: Vec<AttributeValue> = changes
            .into_iter()
            .map(|(cluster, attribute, value)| AttributeValue {
                endpoint,
                cluster,
                attribute,
                value,
            })
            .collect();
        if self.node.set_attributes(values.clone()).await.is_err() {
            return Status::Failure;
        }
        self.state.with(|state| {
            for value in values {
                state.store_value(value.endpoint, value.cluster, value.attribute, value.value);
            }
        });
        Status::Success
    }

    /// Hands the first command back and queues the rest.
    fn queue(
        &self,
        key: DeviceKey,
        part: Option<PartIndex>,
        commands: Vec<DeviceCommand>,
    ) -> Option<CommandEvent> {
        let mut commands = commands.into_iter();
        let first = commands.next()?;
        self.state.with(|state| {
            for command in commands {
                state.queued.push_back(CommandEvent {
                    key: key.clone(),
                    part,
                    command,
                });
            }
        });
        Some(CommandEvent {
            key,
            part,
            command: first,
        })
    }

    /// Persists a written value in the device's entry.
    fn persist(
        &self,
        key: &DeviceKey,
        slot_part: u16,
        cluster: ClusterId,
        attribute: AttributeId,
        value: &Value,
    ) -> Result<(), Error> {
        let encoded = self.state.with(|state| {
            let device = state.devices.get_mut(key)?;
            device
                .persisted
                .insert((slot_part, cluster, attribute), value.clone());
            Some(encode_persisted(&device.persisted))
        });
        if let Some(encoded) = encoded {
            self.storage.store(&attributes_key(key), &encoded)?;
        }
        Ok(())
    }
}

/// The storage key of a device's persisted values.
fn attributes_key(key: &DeviceKey) -> String {
    alloc::format!("{ATTRIBUTES_KEY_PREFIX}{key}")
}

/// Encodes the persisted values of a device: a version byte `1`, then per value the slot part
/// (`u16`), the cluster (`u32`), the attribute (`u32`), the length of the encoded value (`u16`)
/// and the value in the encoding of the `encoding` module, all big-endian.
fn encode_persisted(values: &BTreeMap<(u16, ClusterId, AttributeId), Value>) -> Vec<u8> {
    let mut out = alloc::vec![ATTRIBUTES_VERSION];
    for ((slot, cluster, attribute), value) in values {
        let Some(encoded) = encoding::encode(value) else {
            continue;
        };
        out.extend(slot.to_be_bytes());
        out.extend(cluster.raw().to_be_bytes());
        out.extend(attribute.raw().to_be_bytes());
        out.extend(
            u16::try_from(encoded.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        out.extend(encoded);
    }
    out
}

fn decode_persisted(
    bytes: &[u8],
) -> Result<BTreeMap<(u16, ClusterId, AttributeId), Value>, String> {
    let Some((&version, mut rest)) = bytes.split_first() else {
        return Err(String::from("the entry is empty"));
    };
    if version != ATTRIBUTES_VERSION {
        return Err(alloc::format!("unknown entry version {version}"));
    }
    let mut values = BTreeMap::new();
    while !rest.is_empty() {
        if rest.len() < 12 {
            return Err(String::from("truncated entry"));
        }
        let slot = u16::from_be_bytes([rest[0], rest[1]]);
        let cluster = u32::from_be_bytes([rest[2], rest[3], rest[4], rest[5]]);
        let attribute = u32::from_be_bytes([rest[6], rest[7], rest[8], rest[9]]);
        let length = usize::from(u16::from_be_bytes([rest[10], rest[11]]));
        rest = &rest[12..];
        if rest.len() < length {
            return Err(String::from("truncated value"));
        }
        let value = encoding::decode(&rest[..length])?;
        rest = &rest[length..];
        values.insert(
            (slot, ClusterId::new(cluster), AttributeId::new(attribute)),
            value,
        );
    }
    Ok(values)
}

fn load_persisted(
    storage: &dyn Storage,
    key: &DeviceKey,
) -> Result<BTreeMap<(u16, ClusterId, AttributeId), Value>, Error> {
    let storage_key = attributes_key(key);
    match storage.load(&storage_key)? {
        None => Ok(BTreeMap::new()),
        Some(bytes) => decode_persisted(&bytes).map_err(|reason| Error::CorruptState {
            key: storage_key,
            reason,
        }),
    }
}

/// The start-up attributes of a freshly planned device: the values to set and the commands for
/// the adapter. Only persisted start-up attributes have an effect; without one the adapter's
/// initial value stands.
fn start_up(
    key: &DeviceKey,
    endpoints: &[PlannedEndpoint],
) -> (Vec<AttributeValue>, Vec<CommandEvent>) {
    let mut changes = Vec::new();
    let mut commands = Vec::new();
    for endpoint in endpoints {
        let id = endpoint.plan.endpoint;
        let get = |attribute: Attribute| endpoint.values.get(&attribute.path()).cloned();
        let mut change = |attribute: Attribute, value: Value| {
            let (cluster, attribute) = attribute.path();
            changes.push(AttributeValue {
                endpoint: id,
                cluster,
                attribute,
                value,
            });
        };
        let mut command = |command: DeviceCommand| {
            commands.push(CommandEvent {
                key: key.clone(),
                part: endpoint.plan.part,
                command,
            });
        };
        if let Some(Value::U8(start_up)) = get(Attribute::StartUpOnOff)
            && let Some(Value::Bool(on)) = get(Attribute::OnOff)
        {
            let target = match start_up {
                on_off::START_UP_OFF => Some(false),
                on_off::START_UP_ON => Some(true),
                on_off::START_UP_TOGGLE => Some(!on),
                _ => None,
            };
            if let Some(target) = target
                && target != on
            {
                change(Attribute::OnOff, Value::Bool(target));
                command(DeviceCommand::SetOnOff { on: target });
            }
        }
        if let Some(Value::U8(start_up)) = get(Attribute::StartUpCurrentLevel) {
            let min = get(Attribute::Raw(
                level_control::CLUSTER,
                level_control::MIN_LEVEL,
            ))
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(level_control::LIGHTING_MIN_LEVEL);
            let max = get(Attribute::Raw(
                level_control::CLUSTER,
                level_control::MAX_LEVEL,
            ))
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(level_control::LIGHTING_MAX_LEVEL);
            let target = match start_up {
                level_control::START_UP_LEVEL_MINIMUM => Some(min),
                level_control::START_UP_LEVEL_PREVIOUS => None,
                level => Some(level.clamp(min, max)),
            };
            if let Some(target) = target
                && get(Attribute::CurrentLevel) != Some(Value::U8(target))
            {
                change(Attribute::CurrentLevel, Value::U8(target));
                command(DeviceCommand::SetLevel {
                    level: target,
                    transition: None,
                    on_off: None,
                });
            }
        }
        if let Some(Value::U16(start_up)) = get(Attribute::StartUpColorTemperatureMireds) {
            let min = get(Attribute::ColorTempPhysicalMinMireds)
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(0);
            let max = get(Attribute::ColorTempPhysicalMaxMireds)
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(color_control::MAX_COLOR_COORDINATE);
            let target = start_up.clamp(min, max.max(min));
            if get(Attribute::ColorTemperatureMireds) != Some(Value::U16(target)) {
                change(Attribute::ColorTemperatureMireds, Value::U16(target));
                change(
                    Attribute::ColorMode,
                    Value::U8(color_control::MODE_COLOR_TEMPERATURE),
                );
                change(
                    Attribute::Raw(color_control::CLUSTER, color_control::ENHANCED_COLOR_MODE),
                    Value::U8(color_control::MODE_COLOR_TEMPERATURE),
                );
                command(DeviceCommand::SetColorTemperature {
                    mireds: target,
                    transition: None,
                });
            }
        }
    }
    (changes, commands)
}

/// The adapter's handle on an added device.
///
/// Cloning the handle gives another handle on the same device. A handle of a removed device
/// fails every operation with [`Error::UnknownDevice`].
#[derive(Clone)]
pub struct DeviceHandle {
    inner: Arc<Inner>,
    key: DeviceKey,
}

impl fmt::Debug for DeviceHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceHandle")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl DeviceHandle {
    /// The key of the device.
    #[must_use]
    pub fn key(&self) -> &DeviceKey {
        &self.key
    }

    /// The specification the device was added with.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`] when the device was removed.
    pub fn spec(&self) -> Result<DeviceSpec, Error> {
        self.inner.state.with(|state| {
            state
                .devices
                .get(&self.key)
                .map(|device| device.spec.clone())
                .ok_or_else(|| Error::UnknownDevice(self.key.clone()))
        })
    }

    /// The endpoints of the device as the bridge describes them to the engine, the parent of
    /// a composed device first.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`] when the device was removed.
    pub fn endpoints(&self) -> Result<Vec<EndpointPlan>, Error> {
        self.inner.state.with(|state| {
            let device = state
                .devices
                .get(&self.key)
                .ok_or_else(|| Error::UnknownDevice(self.key.clone()))?;
            Ok(device.endpoints.iter().map(|e| e.plan.clone()).collect())
        })
    }

    /// The value the device currently reports for an attribute of a part.
    #[must_use]
    pub fn value(&self, part: PartIndex, attribute: Attribute) -> Option<Value> {
        self.inner.state.with(|state| {
            state
                .devices
                .get(&self.key)?
                .endpoint(Some(part))?
                .values
                .get(&canonical(attribute).path())
                .cloned()
        })
    }

    /// Reports a new value of an attribute of a part; subscribers of the node are told.
    ///
    /// The value is checked against the attribute's type and range. A change of a contact
    /// sensor's `StateValue` also emits the Boolean State `StateChange` event.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`], [`Error::UnknownPart`], [`Error::UnknownAttribute`],
    /// [`Error::InvalidValue`]; the engine's errors, wrapped.
    pub async fn set(
        &self,
        part: PartIndex,
        attribute: Attribute,
        value: impl Into<Value>,
    ) -> Result<(), Error> {
        let value = value.into();
        let attribute = canonical(attribute);
        let (endpoint, cluster, id, value, state_change) = self.inner.state.with(|state| {
            let device = state
                .devices
                .get(&self.key)
                .ok_or_else(|| Error::UnknownDevice(self.key.clone()))?;
            let planned = device
                .endpoint(Some(part))
                .ok_or_else(|| Error::UnknownPart {
                    key: self.key.clone(),
                    part,
                })?;
            let spec = planned
                .attribute(attribute)
                .ok_or_else(|| Error::UnknownAttribute {
                    key: self.key.clone(),
                    part,
                    attribute,
                })?;
            let value = spec.check(&value).map_err(|reason| Error::InvalidValue {
                key: self.key.clone(),
                part,
                attribute,
                reason,
            })?;
            let (cluster, id) = attribute.path();
            let state_change = attribute == Attribute::StateValue
                && planned.values.get(&(cluster, id)) != Some(&value)
                && planned
                    .cluster(cluster)
                    .is_some_and(|c| c.events.contains(&boolean_state::STATE_CHANGE));
            Ok::<_, Error>((planned.plan.endpoint, cluster, id, value, state_change))
        })?;
        self.inner
            .node
            .set_attribute(endpoint, cluster, id, value.clone())
            .await?;
        self.inner
            .state
            .with(|state| state.store_value(endpoint, cluster, id, value.clone()));
        if state_change {
            self.inner
                .node
                .emit_event(
                    endpoint,
                    cluster,
                    boolean_state::STATE_CHANGE,
                    Value::Struct(alloc::vec![(0, value)]),
                )
                .await?;
        }
        Ok(())
    }

    /// Marks the device reachable or unreachable: sets `Reachable` and emits `ReachableChanged`
    /// on the device's top endpoint.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`]; the engine's errors, wrapped.
    pub async fn set_reachable(&self, reachable: bool) -> Result<(), Error> {
        let endpoint = self.top_endpoint()?;
        self.inner.node.set_reachable(endpoint, reachable).await?;
        self.inner.state.with(|state| {
            state.store_value(
                endpoint,
                bridged_device::CLUSTER,
                bridged_device::REACHABLE,
                Value::Bool(reachable),
            );
        });
        Ok(())
    }

    /// Whether the device is currently marked reachable.
    #[must_use]
    pub fn is_reachable(&self) -> bool {
        self.inner.state.with(|state| {
            state
                .devices
                .get(&self.key)
                .and_then(|device| {
                    device
                        .top()
                        .values
                        .get(&(bridged_device::CLUSTER, bridged_device::REACHABLE))
                        .cloned()
                })
                .is_some_and(|value| value == Value::Bool(true))
        })
    }

    /// Reports an event of a part: switch input for a generic switch, or a raw event of an
    /// extra cluster.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownDevice`], [`Error::UnknownPart`], [`Error::UnsupportedEvent`] when the
    /// part does not produce the event; the engine's errors, wrapped.
    pub async fn emit(&self, part: PartIndex, event: DeviceEvent) -> Result<(), Error> {
        let (endpoint, cluster, id, payload, position) = self.inner.state.with(|state| {
            let device = state
                .devices
                .get(&self.key)
                .ok_or_else(|| Error::UnknownDevice(self.key.clone()))?;
            let planned = device
                .endpoint(Some(part))
                .ok_or_else(|| Error::UnknownPart {
                    key: self.key.clone(),
                    part,
                })?;
            let unsupported = || Error::UnsupportedEvent {
                key: self.key.clone(),
                part,
            };
            let (cluster, id, payload, position) = match event {
                DeviceEvent::Raw {
                    cluster,
                    event,
                    payload,
                } => (cluster, event, payload, None),
                other => translate_event(&other).ok_or_else(unsupported)?,
            };
            let listed = planned
                .cluster(cluster)
                .is_some_and(|c| c.events.contains(&id));
            if !listed {
                return Err(unsupported());
            }
            Ok::<_, Error>((planned.plan.endpoint, cluster, id, payload, position))
        })?;
        if let Some(position) = position {
            let (cluster, attribute) = Attribute::CurrentPosition.path();
            self.inner
                .node
                .set_attribute(endpoint, cluster, attribute, Value::U8(position))
                .await?;
            self.inner.state.with(|state| {
                state.store_value(endpoint, cluster, attribute, Value::U8(position));
            });
        }
        self.inner
            .node
            .emit_event(endpoint, cluster, id, payload)
            .await?;
        Ok(())
    }

    fn top_endpoint(&self) -> Result<EndpointId, Error> {
        self.inner.state.with(|state| {
            state
                .devices
                .get(&self.key)
                .map(|device| device.top().plan.endpoint)
                .ok_or_else(|| Error::UnknownDevice(self.key.clone()))
        })
    }
}

/// The adapter's way to answer a raw command or write.
///
/// Exactly one answer reaches the Matter client: the adapter's through [`respond`](Self::respond),
/// or the bridge's `Failure` when the handle is dropped unanswered or the response timeout
/// passes first.
pub struct ResponseHandle {
    inner: Arc<Inner>,
    correlation: Correlation,
    shared: Arc<PendingShared>,
    write: Option<(EndpointId, ClusterId, AttributeId, Value)>,
}

impl fmt::Debug for ResponseHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseHandle")
            .field("correlation", &self.correlation)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ResponseHandle {
    fn eq(&self, other: &Self) -> bool {
        self.correlation == other.correlation
    }
}

impl ResponseHandle {
    /// The correlation of the request, for diagnostics.
    #[must_use]
    pub fn correlation(&self) -> Correlation {
        self.correlation
    }

    /// Answers the request. For a write, a successful answer makes the engine store the value.
    ///
    /// # Errors
    ///
    /// [`Error::Engine`] with [`matter_rs_engine::Error::UnknownCorrelation`] when the bridge
    /// answered `Failure` already because the timeout passed; the engine's other errors,
    /// wrapped.
    pub async fn respond(self, response: Response) -> Result<(), Error> {
        if self.shared.answered.swap(true, Ordering::AcqRel) {
            return Err(Error::Engine(matter_rs_engine::Error::UnknownCorrelation(
                self.correlation,
            )));
        }
        let result = self
            .inner
            .node
            .respond(self.correlation, response.clone())
            .await;
        self.inner.state.with(|state| {
            state.pending.retain(|p| p.correlation != self.correlation);
            if result.is_ok()
                && response.status.is_success()
                && let Some((endpoint, cluster, attribute, value)) = &self.write
            {
                state.store_value(*endpoint, *cluster, *attribute, value.clone());
            }
        });
        Ok(result?)
    }

    /// Answers with [`Status::Success`] and no payload.
    ///
    /// # Errors
    ///
    /// As [`respond`](Self::respond).
    pub async fn succeed(self) -> Result<(), Error> {
        self.respond(Response::success()).await
    }

    /// Answers with a status and no payload.
    ///
    /// # Errors
    ///
    /// As [`respond`](Self::respond).
    pub async fn fail(self, status: Status) -> Result<(), Error> {
        self.respond(Response::status(status)).await
    }
}

impl Drop for ResponseHandle {
    fn drop(&mut self) {
        if !self.shared.answered.load(Ordering::Acquire) {
            self.shared.dropped.store(true, Ordering::Release);
            self.inner.state.with(State::wake);
        }
    }
}
