//! The adapter: one broker connection, the bridge's own mqtt-smarthome items, and the mapped
//! devices in both directions.
//!
//! The run has two tasks. The network task owns the rumqttc event loop: it connects, hands
//! every event over a channel and, after a lost connection, waits with exponential backoff
//! before the next attempt. The adapter task does everything else: it adds the devices to the
//! bridge, routes incoming messages to attributes and reachability, turns device commands into
//! `set` messages, publishes the bridge's own state and answers the host's shutdown.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use matter_rs_adapter_api::{
    Adapter, AdapterContext, AdapterFactory, AdapterId, BoxFuture, Error, Health,
};
use matter_rs_bridge::{
    Attribute, CommandEvent, DeviceCommand, DeviceHandle, InitialValues, PartIndex,
};
use matter_rs_engine::{Status, Value};
use rumqttc::{AsyncClient, Event, EventLoop, Incoming, LastWill, MqttOptions, Outgoing, QoS};
use serde_json::Value as Json;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::attributes;
use crate::config::{BrokerUrl, Config, Reconnect, SecretText};
use crate::instance::{self, BridgeIdentity};
use crate::mapping::{self, DeviceDefinition, ResolvedDevice};
use crate::payload;

/// The kind the configuration selects this adapter with.
pub const KIND: &str = "mqtt";

/// The state of the node that the bridge instance publishes under
/// `<name>/status/commissioning/...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeState {
    /// Whether the node is part of at least one fabric.
    pub commissioned: bool,
    /// Whether a commissioning window is open.
    pub window_open: bool,
    /// How many fabrics the node is part of.
    pub fabric_count: u32,
}

/// Where the adapter learns the node's state from: a watch channel the host feeds. `None` means
/// the node is not running yet (the instance then publishes `connected` as `1`); `Some` means it
/// runs (`2`).
pub type NodeStateReceiver = watch::Receiver<Option<NodeState>>;

/// A node state channel that never says the node runs, for hosts without a node.
#[must_use]
pub fn no_node_state() -> NodeStateReceiver {
    let (sender, receiver) = watch::channel(None);
    // Keeping the sender alive keeps `changed()` pending instead of erroring.
    std::mem::forget(sender);
    receiver
}

/// The adapter instance before it runs.
pub struct MqttAdapter {
    instance: String,
    config: Config,
    url: BrokerUrl,
    devices: Vec<ResolvedDevice>,
    identity: Option<BridgeIdentity>,
    node_state: NodeStateReceiver,
}

impl core::fmt::Debug for MqttAdapter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MqttAdapter")
            .field("instance", &self.instance)
            .field("url", &self.url)
            .field("devices", &self.devices.len())
            .finish_non_exhaustive()
    }
}

impl MqttAdapter {
    /// Builds an instance with its devices, the node's identity for `info` and the channel it
    /// learns the node's state from. Cheap and side-effect free: nothing connects before
    /// [`Adapter::run`].
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the configuration or a mapping is not usable: a bad URL, an
    /// unresolved password, a device whose mapping does not fit its kind, two devices with one
    /// key, or a device whose source is the bridge's own instance name.
    pub fn build(
        id: AdapterId,
        config: Config,
        devices: Vec<DeviceDefinition>,
        identity: Option<BridgeIdentity>,
        node_state: NodeStateReceiver,
    ) -> Result<Box<dyn Adapter>, Error> {
        let issues = config.check();
        if let Some(issue) = issues.first() {
            return Err(Error::Config(issue.to_string()));
        }
        let url =
            BrokerUrl::parse(&config.url).map_err(|error| Error::Config(error.to_string()))?;
        if matches!(
            config.password,
            Some(SecretText::Env(_) | SecretText::File(_))
        ) {
            return Err(Error::Config(String::from(
                "`password` must be resolved before the adapter is built; the configuration \
                 loader resolves the `env` and `file` forms",
            )));
        }
        let mut resolved = Vec::with_capacity(devices.len());
        for definition in &devices {
            let device = mapping::resolve(definition).map_err(|errors| {
                let texts: Vec<String> = errors.iter().map(ToString::to_string).collect();
                Error::Config(format!("device `{}`: {}", definition.key, texts.join("; ")))
            })?;
            if device.source.split('/').next() == Some(config.name.as_str()) {
                return Err(Error::Config(format!(
                    "device `{}`: its source is the bridge's own instance name `{}`",
                    definition.key, config.name
                )));
            }
            if resolved
                .iter()
                .any(|other: &ResolvedDevice| other.key() == device.key())
            {
                return Err(Error::Config(format!(
                    "device `{}` is defined twice",
                    definition.key
                )));
            }
            resolved.push(device);
        }
        Ok(Box::new(Self {
            instance: id.instance,
            config,
            url,
            devices: resolved,
            identity,
            node_state,
        }))
    }
}

/// The factory the configuration crate composes: an instance without devices and without a
/// node, which publishes the broker side of the bridge instance only. The service uses
/// [`MqttAdapter::build`] to hand over the devices and the node's state.
#[derive(Debug, Clone, Copy, Default)]
pub struct MqttFactory;

impl AdapterFactory for MqttFactory {
    const KIND: &'static str = KIND;
    type Config = Config;

    fn build(id: AdapterId, config: Config) -> Result<Box<dyn Adapter>, Error> {
        MqttAdapter::build(id, config, Vec::new(), None, no_node_state())
    }
}

impl Adapter for MqttAdapter {
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let mut runner = Runner::start(*self, ctx).await?;
            runner.serve().await
        })
    }
}

/// What the network task reports.
enum Net {
    /// An event of the MQTT event loop.
    Event(Event),
    /// The connection failed or was lost; the task retries after `retry_in`.
    Lost { reason: String, retry_in: Duration },
}

/// Where a subscribed topic leads.
#[derive(Debug, Clone, Copy)]
enum Route {
    /// The `connected` item of a source adapter.
    Connected,
    /// The online item of a device.
    Online(usize),
    /// An attribute of a device: the device and the index of its item.
    Status(usize, usize),
}

/// One device while the adapter runs.
struct DeviceState {
    resolved: ResolvedDevice,
    handle: DeviceHandle,
    /// Per item: whether a value has arrived (fixed values count as arrived).
    seen: Vec<bool>,
    /// The online item's last value, when the mapping has one.
    online: Option<bool>,
    reachable: bool,
}

impl DeviceState {
    /// Whether the device should be reachable: its source adapter is connected to its hardware,
    /// its online item (when configured) says so, and every required attribute has a value.
    fn wants_reachable(&self, source_connected: Option<u8>) -> bool {
        source_connected.is_some_and(|level| level >= 2)
            && (self.resolved.online_topic.is_none() || self.online == Some(true))
            && self
                .resolved
                .items
                .iter()
                .zip(&self.seen)
                .all(|(item, seen)| *seen || !item.required)
    }
}

struct Runner {
    ctx: AdapterContext,
    config: Config,
    url: BrokerUrl,
    identity: Option<BridgeIdentity>,
    node_state: NodeStateReceiver,
    node_state_live: bool,
    client: AsyncClient,
    events: mpsc::Receiver<Net>,
    stop_network: watch::Sender<bool>,
    network: Option<JoinHandle<()>>,
    devices: Vec<DeviceState>,
    routes: HashMap<String, Vec<Route>>,
    /// The `connected` level of every source adapter, by its `connected` topic.
    sources: HashMap<String, Option<u8>>,
    /// The last value and `lc` of every status item the instance published.
    published: HashMap<String, (Json, u64)>,
    connected: bool,
    losses: u32,
    started: SystemTime,
}

impl Runner {
    async fn start(adapter: MqttAdapter, ctx: AdapterContext) -> Result<Self, Error> {
        let MqttAdapter {
            instance,
            config,
            url,
            devices,
            identity,
            node_state,
        } = adapter;

        let mut states = Vec::with_capacity(devices.len());
        let mut routes: HashMap<String, Vec<Route>> = HashMap::new();
        let mut sources = HashMap::new();
        for (index, resolved) in devices.into_iter().enumerate() {
            let mut initial = InitialValues::new().reachable(false);
            let mut seen = Vec::with_capacity(resolved.items.len());
            for (item_index, item) in resolved.items.iter().enumerate() {
                let value = item
                    .constant
                    .clone()
                    .unwrap_or_else(|| attributes::placeholder(item.attribute));
                initial = initial.with(item.part, item.attribute.attribute, value);
                seen.push(item.constant.is_some());
                if let Some(topic) = &item.status_topic {
                    routes
                        .entry(topic.clone())
                        .or_default()
                        .push(Route::Status(index, item_index));
                }
            }
            routes
                .entry(resolved.connected_topic.clone())
                .or_default()
                .push(Route::Connected);
            sources.insert(resolved.connected_topic.clone(), None);
            if let Some(topic) = &resolved.online_topic {
                routes
                    .entry(topic.clone())
                    .or_default()
                    .push(Route::Online(index));
            }
            let handle = ctx.devices().add(resolved.spec.clone(), initial).await?;
            log::info!(
                "{}: device {} ({} part(s)) added from {}",
                ctx.instance(),
                resolved.key(),
                resolved.spec.parts.len(),
                resolved.source
            );
            states.push(DeviceState {
                resolved,
                handle,
                seen,
                online: None,
                reachable: false,
            });
        }

        let mut options = MqttOptions::new(config.client_id(&instance), url.host.clone(), url.port);
        options.set_keep_alive(Duration::from_secs(u64::from(config.keep_alive)));
        options.set_clean_session(true);
        options.set_max_packet_size(256 * 1024, 256 * 1024);
        options.set_last_will(LastWill::new(
            format!("{}/connected", config.name),
            "0",
            QoS::AtMostOnce,
            true,
        ));
        let username = config.username.clone().or_else(|| url.username.clone());
        let password = config
            .password
            .as_ref()
            .and_then(SecretText::plain)
            .or(url.password.as_ref());
        if let Some(username) = username {
            options.set_credentials(
                username,
                password
                    .map(|secret| secret.expose().clone())
                    .unwrap_or_default(),
            );
        }
        let capacity = 128 + 4 * states.len() + routes.len();
        let (client, eventloop) = AsyncClient::new(options, capacity);
        let (events_tx, events) = mpsc::channel(256);
        let (stop_network, stop) = watch::channel(false);
        let network = tokio::spawn(network_task(
            eventloop,
            events_tx,
            stop,
            config.reconnect.clone(),
        ));
        log::info!(
            "{}: connecting to {url} as `{}`, instance name `{}`",
            ctx.instance(),
            config.client_id(&instance),
            config.name
        );
        Ok(Self {
            ctx,
            config,
            url,
            identity,
            node_state,
            node_state_live: true,
            client,
            events,
            stop_network,
            network: Some(network),
            devices: states,
            routes,
            sources,
            published: HashMap::new(),
            connected: false,
            losses: 0,
            started: SystemTime::now(),
        })
    }

    async fn serve(&mut self) -> Result<(), Error> {
        let mut shutdown = self.ctx.shutdown();
        let mut commands = self.ctx.commands();
        loop {
            tokio::select! {
                () = &mut shutdown => {
                    self.stop().await;
                    return Ok(());
                }
                net = self.events.recv() => match net {
                    None => {
                        return Err(Error::Internal(String::from("the MQTT network task ended")));
                    }
                    Some(Net::Event(event)) => self.on_event(event).await,
                    Some(Net::Lost { reason, retry_in }) => self.on_lost(&reason, retry_in),
                },
                command = commands.next() => match command {
                    None => {
                        self.stop().await;
                        return Ok(());
                    }
                    Some(command) => self.on_command(command).await,
                },
                changed = self.node_state.changed(), if self.node_state_live => {
                    if changed.is_err() {
                        // The host dropped its sender; the last state stands.
                        self.node_state_live = false;
                    } else {
                        self.publish_node_state();
                        self.publish_connected();
                    }
                }
            }
        }
    }

    /// The instance's own topic below `<name>/`.
    fn topic(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.config.name)
    }

    /// Publishes with `QoS` 0, if connected. A full request queue drops the message with a log
    /// line; the next connect republishes every retained item anyway.
    fn publish(&self, topic: &str, payload: String, retain: bool) {
        if !self.connected {
            log::debug!(
                "{}: not connected, dropped mqtt > {topic} {payload}",
                self.ctx.instance()
            );
            return;
        }
        log::debug!("{}: mqtt > {topic} {payload}", self.ctx.instance());
        if let Err(error) = self
            .client
            .try_publish(topic, QoS::AtMostOnce, retain, payload)
        {
            log::warn!(
                "{}: cannot queue a publish to {topic}: {error}",
                self.ctx.instance()
            );
        }
    }

    /// Publishes a retained status item of the instance in the configured form, with `lc` kept
    /// from the last publish when the value did not change.
    fn publish_status(&mut self, topic: String, value: Json) {
        let now = instance::now_millis();
        let lc = match self.published.get(&topic) {
            Some((previous, lc)) if *previous == value => *lc,
            _ => now,
        };
        let text = if self.config.json_payloads {
            payload::encode_status(value.clone(), now, lc)
        } else {
            payload::encode_plain(&value)
        };
        self.published.insert(topic.clone(), (value, lc));
        self.publish(&topic, text, true);
    }

    fn publish_connected(&self) {
        let level = if self.node_state.borrow().is_some() {
            "2"
        } else {
            "1"
        };
        self.publish(&self.topic("connected"), String::from(level), true);
    }

    fn publish_info(&self) {
        let info = instance::info_document(
            env!("CARGO_PKG_VERSION"),
            instance::host_name().as_deref(),
            std::process::id(),
            self.started,
            self.identity,
        );
        self.publish(&self.topic("info"), info, true);
    }

    fn publish_node_state(&mut self) {
        let state = self.node_state.borrow_and_update().unwrap_or_default();
        self.publish_status(
            self.topic("status/commissioning/commissioned"),
            Json::Bool(state.commissioned),
        );
        self.publish_status(
            self.topic("status/commissioning/window_open"),
            Json::Bool(state.window_open),
        );
        self.publish_status(
            self.topic("status/commissioning/fabric_count"),
            Json::from(state.fabric_count),
        );
    }

    fn publish_reachable(&mut self, device: usize) {
        let key = self.devices[device].resolved.key().to_string();
        let reachable = self.devices[device].reachable;
        self.publish_status(
            self.topic(&format!("status/devices/{key}/reachable")),
            Json::Bool(reachable),
        );
    }

    async fn on_event(&mut self, event: Event) {
        match event {
            Event::Incoming(Incoming::ConnAck(_)) => self.on_connected(),
            Event::Incoming(Incoming::Publish(publish)) => {
                self.on_message(&publish.topic, &publish.payload).await;
            }
            Event::Incoming(Incoming::Disconnect) => {
                log::info!("{}: the broker closed the connection", self.ctx.instance());
            }
            Event::Incoming(_) | Event::Outgoing(_) => {}
        }
    }

    fn on_connected(&mut self) {
        self.connected = true;
        self.losses = 0;
        self.published.clear();
        log::info!("{}: connected to {}", self.ctx.instance(), self.url);
        self.ctx.set_health(Health::Healthy);
        self.publish_connected();
        self.publish_info();
        self.publish_node_state();
        for device in 0..self.devices.len() {
            self.publish_reachable(device);
        }
        let topics: Vec<rumqttc::SubscribeFilter> = self
            .routes
            .keys()
            .map(|topic| rumqttc::SubscribeFilter::new(topic.clone(), QoS::AtMostOnce))
            .collect();
        if topics.is_empty() {
            return;
        }
        let count = topics.len();
        match self.client.try_subscribe_many(topics) {
            Ok(()) => log::info!("{}: subscribed to {count} topics", self.ctx.instance()),
            Err(error) => log::warn!(
                "{}: cannot queue the subscriptions: {error}",
                self.ctx.instance()
            ),
        }
    }

    fn on_lost(&mut self, reason: &str, retry_in: Duration) {
        self.connected = false;
        self.losses += 1;
        let message = format!(
            "{}: connection to {} failed: {reason}; next attempt in {retry_in:?}",
            self.ctx.instance(),
            self.url
        );
        if self.losses == 1 {
            log::warn!("{message}");
        } else {
            log::debug!("{message}");
        }
        self.ctx.set_health(Health::Degraded(format!(
            "broker connection lost: {reason}"
        )));
    }

    async fn on_message(&mut self, topic: &str, bytes: &[u8]) {
        log::debug!(
            "{}: mqtt < {topic} {}",
            self.ctx.instance(),
            String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
        );
        let Some(routes) = self.routes.get(topic).cloned() else {
            log::debug!("{}: no route for topic {topic}", self.ctx.instance());
            return;
        };
        let parsed = payload::parse(bytes);
        for route in routes {
            match route {
                Route::Connected => {
                    let level = parsed
                        .as_ref()
                        .cloned()
                        .map(payload::unwrap_val)
                        .as_ref()
                        .and_then(payload::to_f64)
                        .filter(|level| (0.0..=2.0).contains(level))
                        .map(|level| level.round().clamp(0.0, 2.0));
                    // Within 0..=2 after the clamp above.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let level = level.map(|level| level as u8);
                    self.sources.insert(topic.to_owned(), level);
                    let affected: Vec<usize> = self
                        .devices
                        .iter()
                        .enumerate()
                        .filter(|(_, device)| device.resolved.connected_topic == topic)
                        .map(|(index, _)| index)
                        .collect();
                    for device in affected {
                        self.update_reachable(device).await;
                    }
                }
                Route::Online(device) => {
                    let online = parsed
                        .as_ref()
                        .cloned()
                        .map(payload::unwrap_val)
                        .as_ref()
                        .and_then(payload::to_bool);
                    self.devices[device].online = online;
                    self.update_reachable(device).await;
                }
                Route::Status(device, item_index) => {
                    let state = &self.devices[device];
                    let item = &state.resolved.items[item_index];
                    let ty = item.attribute.value_type;
                    match item.transform.inbound(ty, parsed.clone()) {
                        Ok(value) => {
                            let (part, attribute) = (item.part, item.attribute.attribute);
                            if let Err(error) = state.handle.set(part, attribute, value).await {
                                log::warn!(
                                    "{}: {}: value from {topic} refused by the bridge: {error}",
                                    self.ctx.instance(),
                                    state.resolved.key()
                                );
                                continue;
                            }
                            self.devices[device].seen[item_index] = true;
                            self.update_reachable(device).await;
                        }
                        Err(rejected) => log::debug!(
                            "{}: {}: payload on {topic} ignored: {rejected}",
                            self.ctx.instance(),
                            state.resolved.key()
                        ),
                    }
                }
            }
        }
    }

    async fn update_reachable(&mut self, device: usize) {
        let state = &self.devices[device];
        let level = self
            .sources
            .get(&state.resolved.connected_topic)
            .copied()
            .flatten();
        let wanted = state.wants_reachable(level);
        if wanted == state.reachable {
            return;
        }
        if let Err(error) = state.handle.set_reachable(wanted).await {
            log::warn!(
                "{}: {}: cannot set reachable: {error}",
                self.ctx.instance(),
                state.resolved.key()
            );
            return;
        }
        log::info!(
            "{}: device {} is {}",
            self.ctx.instance(),
            state.resolved.key(),
            if wanted { "reachable" } else { "unreachable" }
        );
        self.devices[device].reachable = wanted;
        self.publish_reachable(device);
    }

    async fn on_command(&mut self, command: CommandEvent) {
        let Some(device) = self
            .devices
            .iter()
            .position(|device| device.resolved.key() == &command.key)
        else {
            log::debug!(
                "{}: command for unknown device {} dropped",
                self.ctx.instance(),
                command.key
            );
            return;
        };
        let part = command.part.unwrap_or(0);
        match command.command {
            DeviceCommand::SetOnOff { on } => {
                self.set_item(device, part, "on-off", Value::Bool(on));
            }
            DeviceCommand::SetLevel { level, on_off, .. } => {
                self.set_item(device, part, "level", Value::U8(level));
                if let Some(on) = on_off {
                    self.set_item(device, part, "on-off", Value::Bool(on));
                }
            }
            DeviceCommand::SetColorTemperature { mireds, .. } => {
                self.set_item(device, part, "color-temperature", Value::U16(mireds));
            }
            DeviceCommand::SetHue { hue, .. } => self.set_item(device, part, "hue", Value::U8(hue)),
            DeviceCommand::SetSaturation { saturation, .. } => {
                self.set_item(device, part, "saturation", Value::U8(saturation));
            }
            DeviceCommand::SetHueAndSaturation {
                hue, saturation, ..
            } => {
                self.set_item(device, part, "hue", Value::U8(hue));
                self.set_item(device, part, "saturation", Value::U8(saturation));
            }
            DeviceCommand::SetColorXy { x, y, .. } => {
                self.set_item(device, part, "color-x", Value::U16(x));
                self.set_item(device, part, "color-y", Value::U16(y));
            }
            DeviceCommand::Rename(label) => {
                let state = &self.devices[device];
                match state.resolved.rename_topic.clone() {
                    Some(topic) => {
                        let text = if self.config.set_json {
                            payload::encode_val(Json::String(label))
                        } else {
                            label
                        };
                        self.publish(&topic, text, false);
                    }
                    None => log::debug!(
                        "{}: {} renamed to {label}; the mapping names no topic for `name`",
                        self.ctx.instance(),
                        state.resolved.key()
                    ),
                }
            }
            DeviceCommand::Written { attribute, value } => {
                self.set_attribute(device, part, attribute, value);
            }
            DeviceCommand::Identify { .. } | DeviceCommand::TriggerEffect { .. } => {
                log::debug!(
                    "{}: {}: identify is not mapped to MQTT",
                    self.ctx.instance(),
                    self.devices[device].resolved.key()
                );
            }
            DeviceCommand::Raw { response, .. } => {
                let _ = response.fail(Status::UnsupportedCommand).await;
            }
            DeviceCommand::Write { response, .. } => {
                let _ = response.fail(Status::UnsupportedWrite).await;
            }
            _ => {}
        }
    }

    /// Publishes a `set` for the attribute `name` of a part.
    fn set_item(&self, device: usize, part: PartIndex, name: &str, value: Value) {
        let state = &self.devices[device];
        let Some(item) = state.resolved.item(part, name) else {
            log::debug!(
                "{}: {}: part {part} has no `{name}` item",
                self.ctx.instance(),
                state.resolved.key()
            );
            return;
        };
        self.publish_set(device, item, &value);
    }

    /// Publishes a `set` for a written attribute, when the mapping has an item for it.
    fn set_attribute(&self, device: usize, part: PartIndex, attribute: Attribute, value: Value) {
        let state = &self.devices[device];
        match state
            .resolved
            .items
            .iter()
            .find(|item| item.part == part && item.attribute.attribute == attribute)
        {
            Some(item) => self.publish_set(device, item, &value),
            None => log::debug!(
                "{}: {}: written attribute {attribute} is not mapped",
                self.ctx.instance(),
                state.resolved.key()
            ),
        }
    }

    fn publish_set(&self, device: usize, item: &mapping::ResolvedItem, value: &Value) {
        let key = self.devices[device].resolved.key();
        let Some(topic) = &item.set_topic else {
            log::debug!(
                "{}: {key}: `{}` has a fixed value, nothing to set",
                self.ctx.instance(),
                item.attribute.name
            );
            return;
        };
        let Some(wire) = item.transform.outbound(item.attribute.value_type, value) else {
            log::warn!(
                "{}: {key}: `{}` has no representation for {value:?} under its mapping",
                self.ctx.instance(),
                item.attribute.name
            );
            return;
        };
        let text = if self.config.set_json {
            payload::encode_val(wire)
        } else {
            payload::encode_plain(&wire)
        };
        self.publish(topic, text, false);
    }

    /// Says goodbye (`connected` = `0`), disconnects and stops the network task.
    async fn stop(&mut self) {
        log::info!("{}: stopping", self.ctx.instance());
        if self.connected {
            self.publish(&self.topic("connected"), String::from("0"), true);
            let _ = self.client.try_disconnect();
            let flushed = async {
                while let Some(net) = self.events.recv().await {
                    if matches!(net, Net::Event(Event::Outgoing(Outgoing::Disconnect)))
                        || matches!(net, Net::Lost { .. })
                    {
                        break;
                    }
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), flushed).await;
        }
        let _ = self.stop_network.send(true);
        if let Some(mut network) = self.network.take()
            && tokio::time::timeout(Duration::from_secs(1), &mut network)
                .await
                .is_err()
        {
            network.abort();
        }
        self.connected = false;
    }
}

/// Drives the MQTT event loop: every event goes to the adapter task; a failed or lost
/// connection is reported and retried with exponential backoff.
async fn network_task(
    mut eventloop: EventLoop,
    events: mpsc::Sender<Net>,
    mut stop: watch::Receiver<bool>,
    reconnect: Reconnect,
) {
    let mut attempt: u32 = 0;
    loop {
        let polled = tokio::select! {
            _ = stop.changed() => return,
            polled = eventloop.poll() => polled,
        };
        match polled {
            Ok(event) => {
                if matches!(event, Event::Incoming(Incoming::ConnAck(_))) {
                    attempt = 0;
                }
                if events.send(Net::Event(event)).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                let retry_in = backoff(attempt, &reconnect);
                attempt = attempt.saturating_add(1);
                let lost = Net::Lost {
                    reason: error.to_string(),
                    retry_in,
                };
                if events.send(lost).await.is_err() {
                    return;
                }
                tokio::select! {
                    _ = stop.changed() => return,
                    () = sleep(retry_in) => {}
                }
            }
        }
    }
}

/// The delay before reconnect attempt `attempt` (0 for the first): `initial` doubled per attempt,
/// capped at `max`.
fn backoff(attempt: u32, reconnect: &Reconnect) -> Duration {
    let factor = 2_f64.powi(i32::try_from(attempt.min(60)).unwrap_or(60));
    let seconds = (reconnect.initial * factor).min(reconnect.max);
    Duration::from_secs_f64(seconds.max(0.0))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use matter_rs_adapter_api::{AdapterFactory, AdapterId, Error};

    use super::{MqttFactory, backoff};
    use crate::config::{Config, Reconnect};

    fn config(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn backoff_doubles_from_initial_to_max() {
        let reconnect = Reconnect {
            initial: 1.0,
            max: 60.0,
        };
        assert_eq!(backoff(0, &reconnect), Duration::from_secs(1));
        assert_eq!(backoff(1, &reconnect), Duration::from_secs(2));
        assert_eq!(backoff(5, &reconnect), Duration::from_secs(32));
        assert_eq!(backoff(6, &reconnect), Duration::from_secs(60));
        assert_eq!(backoff(u32::MAX, &reconnect), Duration::from_secs(60));
        let quick = Reconnect {
            initial: 0.05,
            max: 0.2,
        };
        assert_eq!(backoff(0, &quick), Duration::from_millis(50));
        assert_eq!(backoff(3, &quick), Duration::from_millis(200));
    }

    /// The error of a refused build; a build that succeeds is a test failure.
    fn refused(text: &str) -> Error {
        match MqttFactory::build(AdapterId::new(MqttFactory::KIND, "home"), config(text)) {
            Ok(_) => panic!("the configuration was accepted: {text}"),
            Err(error) => error,
        }
    }

    #[test]
    fn factory_builds_and_refuses_bad_configurations() {
        let id = AdapterId::new(MqttFactory::KIND, "home");
        assert_eq!(MqttFactory::KIND, "mqtt");
        assert!(MqttFactory::build(id, config("url = \"mqtt://broker\"")).is_ok());
        let error = refused("url = \"broker\"");
        assert!(
            matches!(error, Error::Config(ref m) if m.contains("`url`")),
            "{error}"
        );
        let error = refused(
            "url = \"mqtt://b\"
password = { env = \"P\" }",
        );
        assert!(
            matches!(error, Error::Config(ref m) if m.contains("resolved")),
            "{error}"
        );
        let error = refused("url = \"mqtts://b\"");
        assert!(
            matches!(error, Error::Config(ref m) if m.contains("TLS")),
            "{error}"
        );
    }
}
