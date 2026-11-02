//! A controllable bridge node, driven through the engine facade only.
//!
//! This is the device under test of the interop harness (`tests/interop/`) and of the local
//! scenario (`tests/interop-local/`), and a reference for how a product talks to the facade:
//! it starts the engine, adds endpoints with their initial values, answers the commands and
//! writes the engine reports as events, and takes orders on standard input to change the
//! node while it runs. The cluster semantics (what `Toggle` does, which labels are
//! acceptable) live here, not in the engine.
//!
//! # Usage
//!
//! ```text
//! onoff_node [--state-dir DIR] [--interface NAME] [--port N] [--discriminator N]
//!            [--passcode N] [--ipv4-only] [--print-pairing] [--no-initial-device]
//! ```
//!
//! Unless `--no-initial-device` is given, the node starts with one on/off light at endpoint 2,
//! whose on/off state and label are kept in the state directory across restarts. The state
//! directory also keeps the engine's fabrics. `--print-pairing` prints the manual pairing
//! code and the QR text on standard output at start; both are secrets and are printed only on
//! this explicit request (or on the `pairing` command below).
//!
//! # Line protocol on standard input
//!
//! One command per line; the answer is one line `ok` or `err <reason>` on standard output.
//!
//! | Command | Effect |
//! |---|---|
//! | `add <endpoint> <device-type> [parent]` | Adds an endpoint of the named device type, composed under `parent` when given |
//! | `remove <endpoint>` | Removes the endpoint (and the endpoints composed under it) |
//! | `set <endpoint> <cluster> <attribute> <value>` | Sets an attribute the endpoint lists; ids in hex (`0x0006`) or decimal |
//! | `reachable <endpoint> true\|false` | Marks a bridged endpoint reachable or not |
//! | `pairing` | Prints the manual pairing code and the QR text (the only command that shows a secret) |
//! | `quit` | Stops the node; the process exits after the stop event |
//!
//! A value is written in the type of the attribute: `true`/`false` for booleans, an integer
//! for the integer kinds, the rest of the line for strings, `null` where the attribute is
//! nullable. End of file on standard input does not stop the node, so that it runs detached.
//!
//! Every event the engine reports is printed as a line `event <kind> ...`, for example
//! `event CommandInvoked endpoint=2 cluster=0x0006 command=0x02 status=Success`. The log goes
//! to standard error. The line `onoff_node: ready ...` marks the node as reachable on the
//! network.
//!
//! # Device types
//!
//! The ids and revisions are those of the Matter 1.6 device library and cluster library, read
//! from the `data_model/1.6` directory of
//! <https://github.com/project-chip/connectedhomeip> (`device_types/*.xml`, `clusters/*.xml`).
//! Every bridged endpoint also carries the Bridged Node device type and the Bridged Device
//! Basic Information cluster.
//!
//! | Name | Device type | Clusters |
//! |---|---|---|
//! | `on-off-light` | On/Off Light `0x0100` rev 3 | On/Off `0x0006` rev 6 |
//! | `dimmable-light` | Dimmable Light `0x0101` rev 3 | On/Off, Level Control `0x0008` rev 7 |
//! | `contact-sensor` | Contact Sensor `0x0015` rev 2 | Boolean State `0x0045` rev 3 |
//! | `temperature-sensor` | Temperature Sensor `0x0302` rev 3 | Temperature Measurement `0x0402` rev 6 |
//!
//! Bridged Node is `0x0013` rev 3, Bridged Device Basic Information `0x0039` rev 6.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use matter_rs_engine::testing::block_on;
use matter_rs_engine::{
    Attestation, AttributeId, ClusterDescription, ClusterId, CommandId, DeviceType, DeviceTypeId,
    DirectoryStorage, EndpointDescription, EndpointId, Engine, Event, EventId, Kind, NetworkConfig,
    NodeIdentity, NodeRole, ProductId, Response, Running, Secret, Status, Storage, Value, VendorId,
    bridged_device,
};
use matter_rs_engine_rsmatter::RsMatterEngine;

const AGGREGATOR: EndpointId = EndpointId::new(1);
/// The endpoint of the initial light.
const LIGHT: EndpointId = EndpointId::new(2);

const ON_OFF: ClusterId = ClusterId::new(0x0006);
const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0x0000);
const OFF: CommandId = CommandId::new(0x00);
const ON: CommandId = CommandId::new(0x01);
const TOGGLE: CommandId = CommandId::new(0x02);

const LEVEL_CONTROL: ClusterId = ClusterId::new(0x0008);
const CURRENT_LEVEL: AttributeId = AttributeId::new(0x0000);
const OPTIONS: AttributeId = AttributeId::new(0x000F);
const ON_LEVEL: AttributeId = AttributeId::new(0x0011);
const MOVE_TO_LEVEL: CommandId = CommandId::new(0x00);
const MOVE_TO_LEVEL_WITH_ON_OFF: CommandId = CommandId::new(0x04);

const BOOLEAN_STATE: ClusterId = ClusterId::new(0x0045);
const STATE_VALUE: AttributeId = AttributeId::new(0x0000);
const STATE_CHANGE: EventId = EventId::new(0x00);

const TEMPERATURE_MEASUREMENT: ClusterId = ClusterId::new(0x0402);
const MEASURED_VALUE: AttributeId = AttributeId::new(0x0000);
const MIN_MEASURED_VALUE: AttributeId = AttributeId::new(0x0001);
const MAX_MEASURED_VALUE: AttributeId = AttributeId::new(0x0002);

const NODE_LABEL: AttributeId = AttributeId::new(0x0005);
const UNIQUE_ID: AttributeId = AttributeId::new(0x0012);

const DEVICE_TYPE_ON_OFF_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0100);
const DEVICE_TYPE_DIMMABLE_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0101);
const DEVICE_TYPE_CONTACT_SENSOR: DeviceTypeId = DeviceTypeId::new(0x0015);
const DEVICE_TYPE_TEMPERATURE_SENSOR: DeviceTypeId = DeviceTypeId::new(0x0302);
const DEVICE_TYPE_BRIDGED_NODE: DeviceTypeId = DeviceTypeId::new(0x0013);

/// The longest label a device accepts; shorter than the cluster's 32 so that a client can
/// send a label the device refuses with `ConstraintError`.
const MAX_LABEL_LEN: usize = 16;

/// The product's own storage keys, for the initial light.
const KEY_ON: &str = "bridge/onoff-node/on";
const KEY_LABEL: &str = "bridge/onoff-node/label";

struct Options {
    state_dir: PathBuf,
    interface: Option<String>,
    port: u16,
    discriminator: u16,
    passcode: u32,
    ipv4_only: bool,
    print_pairing: bool,
    initial_device: bool,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        state_dir: std::env::temp_dir().join("matter-rs-onoff-node"),
        interface: None,
        port: 5540,
        discriminator: 3840,
        passcode: 20_202_021,
        ipv4_only: false,
        print_pairing: false,
        initial_device: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--state-dir" => options.state_dir = PathBuf::from(value("--state-dir")?),
            "--interface" => options.interface = Some(value("--interface")?),
            "--port" => {
                options.port = value("--port")?
                    .parse()
                    .map_err(|e| format!("--port: {e}"))?;
            }
            "--discriminator" => {
                options.discriminator = value("--discriminator")?
                    .parse()
                    .map_err(|e| format!("--discriminator: {e}"))?;
            }
            "--passcode" => {
                options.passcode = value("--passcode")?
                    .parse()
                    .map_err(|e| format!("--passcode: {e}"))?;
            }
            "--ipv4-only" => options.ipv4_only = true,
            "--print-pairing" => options.print_pairing = true,
            "--no-initial-device" => options.initial_device = false,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// The device types the node can add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceKind {
    OnOffLight,
    DimmableLight,
    ContactSensor,
    TemperatureSensor,
}

impl DeviceKind {
    const NAMES: [(&'static str, Self); 4] = [
        ("on-off-light", Self::OnOffLight),
        ("dimmable-light", Self::DimmableLight),
        ("contact-sensor", Self::ContactSensor),
        ("temperature-sensor", Self::TemperatureSensor),
    ];

    fn parse(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, kind)| *kind)
    }

    fn device_type(self) -> DeviceType {
        let (id, revision) = match self {
            Self::OnOffLight => (DEVICE_TYPE_ON_OFF_LIGHT, 3),
            Self::DimmableLight => (DEVICE_TYPE_DIMMABLE_LIGHT, 3),
            Self::ContactSensor => (DEVICE_TYPE_CONTACT_SENSOR, 2),
            Self::TemperatureSensor => (DEVICE_TYPE_TEMPERATURE_SENSOR, 3),
        };
        DeviceType { id, revision }
    }

    fn default_label(self) -> &'static str {
        match self {
            Self::OnOffLight => "Light",
            Self::DimmableLight => "Dimmer",
            Self::ContactSensor => "Contact",
            Self::TemperatureSensor => "Thermometer",
        }
    }
}

/// One attribute of a device: where it lives, what kind of value it takes and its initial
/// value.
#[derive(Debug, Clone)]
struct AttributeSpec {
    cluster: ClusterId,
    attribute: AttributeId,
    kind: Kind,
    nullable: bool,
    initial: Value,
}

/// One cluster of a device: its metadata and attributes.
#[derive(Debug, Clone)]
struct ClusterSpec {
    id: ClusterId,
    revision: u16,
    accepted_commands: Vec<CommandId>,
    events: Vec<EventId>,
    attributes: Vec<AttributeSpec>,
}

fn attribute(
    cluster: ClusterId,
    attribute: AttributeId,
    kind: Kind,
    nullable: bool,
    initial: Value,
) -> AttributeSpec {
    AttributeSpec {
        cluster,
        attribute,
        kind,
        nullable,
        initial,
    }
}

fn on_off_spec(on: bool) -> ClusterSpec {
    ClusterSpec {
        id: ON_OFF,
        revision: 6,
        accepted_commands: vec![OFF, ON, TOGGLE],
        events: vec![],
        attributes: vec![attribute(
            ON_OFF,
            ON_OFF_ATTRIBUTE,
            Kind::Bool,
            false,
            Value::Bool(on),
        )],
    }
}

fn level_control_spec() -> ClusterSpec {
    ClusterSpec {
        id: LEVEL_CONTROL,
        revision: 7,
        accepted_commands: vec![MOVE_TO_LEVEL, MOVE_TO_LEVEL_WITH_ON_OFF],
        events: vec![],
        attributes: vec![
            attribute(LEVEL_CONTROL, CURRENT_LEVEL, Kind::U8, true, Value::U8(1)),
            attribute(LEVEL_CONTROL, OPTIONS, Kind::U8, false, Value::U8(0)),
            attribute(LEVEL_CONTROL, ON_LEVEL, Kind::U8, true, Value::Null),
        ],
    }
}

fn boolean_state_spec() -> ClusterSpec {
    ClusterSpec {
        id: BOOLEAN_STATE,
        revision: 3,
        accepted_commands: vec![],
        events: vec![STATE_CHANGE],
        attributes: vec![attribute(
            BOOLEAN_STATE,
            STATE_VALUE,
            Kind::Bool,
            false,
            Value::Bool(false),
        )],
    }
}

fn temperature_measurement_spec() -> ClusterSpec {
    ClusterSpec {
        id: TEMPERATURE_MEASUREMENT,
        revision: 6,
        accepted_commands: vec![],
        events: vec![],
        attributes: vec![
            attribute(
                TEMPERATURE_MEASUREMENT,
                MEASURED_VALUE,
                Kind::I16,
                true,
                Value::I16(2150),
            ),
            attribute(
                TEMPERATURE_MEASUREMENT,
                MIN_MEASURED_VALUE,
                Kind::I16,
                true,
                Value::I16(-4000),
            ),
            attribute(
                TEMPERATURE_MEASUREMENT,
                MAX_MEASURED_VALUE,
                Kind::I16,
                true,
                Value::I16(12500),
            ),
        ],
    }
}

fn bridged_device_spec(label: &str, unique_id: &str) -> ClusterSpec {
    ClusterSpec {
        id: bridged_device::CLUSTER,
        revision: 6,
        accepted_commands: vec![],
        events: vec![bridged_device::REACHABLE_CHANGED],
        attributes: vec![
            attribute(
                bridged_device::CLUSTER,
                NODE_LABEL,
                Kind::Utf8,
                false,
                Value::from(label),
            ),
            attribute(
                bridged_device::CLUSTER,
                bridged_device::REACHABLE,
                Kind::Bool,
                false,
                Value::Bool(true),
            ),
            attribute(
                bridged_device::CLUSTER,
                UNIQUE_ID,
                Kind::Utf8,
                false,
                Value::from(unique_id),
            ),
        ],
    }
}

/// A device the product serves: what it is and the values it currently reports.
#[derive(Debug, Clone)]
struct Device {
    kind: DeviceKind,
    parent: Option<EndpointId>,
    clusters: Vec<ClusterSpec>,
    values: BTreeMap<(ClusterId, AttributeId), Value>,
}

impl Device {
    fn new(
        id: EndpointId,
        kind: DeviceKind,
        parent: Option<EndpointId>,
        label: &str,
        on: bool,
    ) -> Self {
        let unique_id = format!("onoff-node-{}", id.raw());
        let mut clusters = match kind {
            DeviceKind::OnOffLight => vec![on_off_spec(on)],
            DeviceKind::DimmableLight => vec![on_off_spec(on), level_control_spec()],
            DeviceKind::ContactSensor => vec![boolean_state_spec()],
            DeviceKind::TemperatureSensor => vec![temperature_measurement_spec()],
        };
        clusters.push(bridged_device_spec(label, &unique_id));
        let values = clusters
            .iter()
            .flat_map(|cluster| cluster.attributes.iter())
            .map(|a| ((a.cluster, a.attribute), a.initial.clone()))
            .collect();
        Self {
            kind,
            parent,
            clusters,
            values,
        }
    }

    fn description(&self, id: EndpointId) -> EndpointDescription {
        EndpointDescription {
            id,
            device_types: vec![
                self.kind.device_type(),
                DeviceType {
                    id: DEVICE_TYPE_BRIDGED_NODE,
                    revision: 3,
                },
            ],
            clusters: self
                .clusters
                .iter()
                .map(|cluster| ClusterDescription {
                    id: cluster.id,
                    revision: cluster.revision,
                    feature_map: 0,
                    attributes: cluster.attributes.iter().map(|a| a.attribute).collect(),
                    accepted_commands: cluster.accepted_commands.clone(),
                    generated_commands: vec![],
                    events: cluster.events.clone(),
                })
                .collect(),
            parent: self.parent,
            initial_values: self
                .clusters
                .iter()
                .flat_map(|cluster| cluster.attributes.iter())
                .map(|a| (a.cluster, a.attribute, a.initial.clone()))
                .collect(),
        }
    }

    fn spec(&self, cluster: ClusterId, attribute: AttributeId) -> Option<&AttributeSpec> {
        self.clusters
            .iter()
            .find(|c| c.id == cluster)
            .and_then(|c| c.attributes.iter().find(|a| a.attribute == attribute))
    }

    fn is_on(&self) -> bool {
        matches!(
            self.values.get(&(ON_OFF, ON_OFF_ATTRIBUTE)),
            Some(Value::Bool(true))
        )
    }
}

/// The product's view of the node: the devices by endpoint.
#[derive(Debug, Default)]
struct Model {
    devices: BTreeMap<EndpointId, Device>,
}

impl Model {
    /// Removes `endpoint` and every device composed under it, as the engine does.
    fn remove(&mut self, endpoint: EndpointId) {
        let mut doomed = vec![endpoint];
        loop {
            let more: Vec<EndpointId> = self
                .devices
                .iter()
                .filter(|(id, device)| {
                    device.parent.is_some_and(|p| doomed.contains(&p)) && !doomed.contains(id)
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                break;
            }
            doomed.extend(more);
        }
        for id in doomed {
            self.devices.remove(&id);
        }
    }
}

/// A command of the line protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Add {
        endpoint: EndpointId,
        kind: DeviceKind,
        parent: Option<EndpointId>,
    },
    Remove {
        endpoint: EndpointId,
    },
    Set {
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: String,
    },
    Reachable {
        endpoint: EndpointId,
        reachable: bool,
    },
    Pairing,
    Quit,
}

/// Parses an id in decimal or `0x` hex.
fn parse_id(text: &str, what: &str) -> Result<u32, String> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse::<u32>(),
    };
    parsed.map_err(|_| format!("{what}: {text:?} is not an id"))
}

fn parse_endpoint(text: &str) -> Result<EndpointId, String> {
    let raw = parse_id(text, "endpoint")?;
    u16::try_from(raw)
        .map(EndpointId::new)
        .map_err(|_| format!("endpoint: {text:?} exceeds 16 bits"))
}

fn parse_command(line: &str) -> Result<Command, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some(&verb) = words.first() else {
        return Err(String::from("empty line"));
    };
    let arg = |index: usize, name: &str| {
        words
            .get(index)
            .copied()
            .ok_or_else(|| format!("{verb}: missing {name}"))
    };
    let (command, used) = match verb {
        "add" => {
            let endpoint = parse_endpoint(arg(1, "endpoint")?)?;
            let name = arg(2, "device type")?;
            let kind = DeviceKind::parse(name).ok_or_else(|| {
                let known: Vec<&str> = DeviceKind::NAMES.iter().map(|(n, _)| *n).collect();
                format!(
                    "add: unknown device type {name:?}; known: {}",
                    known.join(", ")
                )
            })?;
            let parent = words.get(3).map(|text| parse_endpoint(text)).transpose()?;
            let used = if parent.is_some() { 4 } else { 3 };
            (
                Command::Add {
                    endpoint,
                    kind,
                    parent,
                },
                used,
            )
        }
        "remove" => (
            Command::Remove {
                endpoint: parse_endpoint(arg(1, "endpoint")?)?,
            },
            2,
        ),
        "set" => {
            let endpoint = parse_endpoint(arg(1, "endpoint")?)?;
            let cluster = ClusterId::new(parse_id(arg(2, "cluster")?, "cluster")?);
            let attribute = AttributeId::new(parse_id(arg(3, "attribute")?, "attribute")?);
            if words.len() < 5 {
                return Err(String::from("set: missing value"));
            }
            (
                Command::Set {
                    endpoint,
                    cluster,
                    attribute,
                    value: words[4..].join(" "),
                },
                words.len(),
            )
        }
        "reachable" => {
            let endpoint = parse_endpoint(arg(1, "endpoint")?)?;
            let reachable = match arg(2, "true|false")? {
                "true" => true,
                "false" => false,
                other => return Err(format!("reachable: {other:?} is not true or false")),
            };
            (
                Command::Reachable {
                    endpoint,
                    reachable,
                },
                3,
            )
        }
        "pairing" => (Command::Pairing, 1),
        "quit" | "stop" => (Command::Quit, 1),
        other => return Err(format!("unknown command {other:?}")),
    };
    if words.len() > used {
        return Err(format!("{verb}: too many arguments"));
    }
    Ok(command)
}

/// Parses a value in the kind of its attribute.
fn parse_value(spec: &AttributeSpec, text: &str) -> Result<Value, String> {
    if text == "null" {
        return if spec.nullable {
            Ok(Value::Null)
        } else {
            Err(format!("attribute {} is not nullable", spec.attribute))
        };
    }
    let bad = |kind: Kind| format!("{text:?} is not a {kind}");
    let value = match spec.kind {
        Kind::Bool => match text {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(bad(Kind::Bool)),
        },
        Kind::U8 => Value::U8(text.parse().map_err(|_| bad(Kind::U8))?),
        Kind::U16 => Value::U16(text.parse().map_err(|_| bad(Kind::U16))?),
        Kind::U32 => Value::U32(text.parse().map_err(|_| bad(Kind::U32))?),
        Kind::U64 => Value::U64(text.parse().map_err(|_| bad(Kind::U64))?),
        Kind::I8 => Value::I8(text.parse().map_err(|_| bad(Kind::I8))?),
        Kind::I16 => Value::I16(text.parse().map_err(|_| bad(Kind::I16))?),
        Kind::I32 => Value::I32(text.parse().map_err(|_| bad(Kind::I32))?),
        Kind::I64 => Value::I64(text.parse().map_err(|_| bad(Kind::I64))?),
        Kind::Utf8 => Value::from(text),
        other => {
            return Err(format!(
                "values of kind {other} cannot be set from the line"
            ));
        }
    };
    Ok(value)
}

/// Everything the command thread and the event loop share.
struct Product {
    node: Arc<dyn Running>,
    model: Mutex<Model>,
    storage: Arc<dyn Storage>,
    identity: NodeIdentity,
}

impl Product {
    fn model(&self) -> std::sync::MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Executes one protocol command; `Ok` is the `ok` answer, `Err` the reason of `err`.
    fn execute(&self, command: Command) -> Result<(), String> {
        match command {
            Command::Add {
                endpoint,
                kind,
                parent,
            } => {
                let device = Device::new(endpoint, kind, parent, kind.default_label(), false);
                block_on(self.node.add_endpoint(device.description(endpoint)))
                    .map_err(|e| e.to_string())?;
                self.model().devices.insert(endpoint, device);
                Ok(())
            }
            Command::Remove { endpoint } => {
                block_on(self.node.remove_endpoint(endpoint)).map_err(|e| e.to_string())?;
                self.model().remove(endpoint);
                Ok(())
            }
            Command::Set {
                endpoint,
                cluster,
                attribute,
                value,
            } => {
                let parsed = {
                    let model = self.model();
                    let device = model
                        .devices
                        .get(&endpoint)
                        .ok_or_else(|| format!("unknown endpoint {endpoint}"))?;
                    let spec = device.spec(cluster, attribute).ok_or_else(|| {
                        format!(
                            "endpoint {endpoint} has no attribute {attribute} in cluster {cluster}"
                        )
                    })?;
                    parse_value(spec, &value)?
                };
                block_on(
                    self.node
                        .set_attribute(endpoint, cluster, attribute, parsed.clone()),
                )
                .map_err(|e| e.to_string())?;
                self.remember(endpoint, cluster, attribute, parsed);
                Ok(())
            }
            Command::Reachable {
                endpoint,
                reachable,
            } => {
                block_on(self.node.set_reachable(endpoint, reachable))
                    .map_err(|e| e.to_string())?;
                self.remember(
                    endpoint,
                    bridged_device::CLUSTER,
                    bridged_device::REACHABLE,
                    Value::Bool(reachable),
                );
                Ok(())
            }
            Command::Pairing => {
                let info = block_on(self.node.pairing_info()).map_err(|e| e.to_string())?;
                print_pairing(info.manual_code.expose(), info.qr_text.expose());
                Ok(())
            }
            Command::Quit => block_on(self.node.stop()).map_err(|e| e.to_string()),
        }
    }

    /// Keeps the product's view of a value in step with the engine, and persists the
    /// initial light's state.
    fn remember(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) {
        if endpoint == LIGHT {
            match (&cluster, &attribute, &value) {
                (&ON_OFF, &ON_OFF_ATTRIBUTE, Value::Bool(on)) => {
                    let _ = self.storage.store(KEY_ON, if *on { b"1" } else { b"0" });
                }
                (&bridged_device::CLUSTER, &NODE_LABEL, Value::Utf8(label)) => {
                    let _ = self.storage.store(KEY_LABEL, label.as_bytes());
                }
                _ => {}
            }
        }
        if let Some(device) = self.model().devices.get_mut(&endpoint) {
            device.values.insert((cluster, attribute), value);
        }
    }

    /// Applies the cluster semantics of a command and reports the new values to the engine.
    fn handle_command(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        command: CommandId,
        payload: &Value,
    ) -> Response {
        let Some(device) = self.model().devices.get(&endpoint).cloned() else {
            return Response::status(Status::UnsupportedEndpoint);
        };
        let changes: Vec<(ClusterId, AttributeId, Value)> = match (cluster, command) {
            (ON_OFF, OFF) => vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))],
            (ON_OFF, ON) => vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))],
            (ON_OFF, TOGGLE) => vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(!device.is_on()))],
            (LEVEL_CONTROL, MOVE_TO_LEVEL | MOVE_TO_LEVEL_WITH_ON_OFF) => {
                let Some(level) = level_field(payload) else {
                    return Response::status(Status::InvalidCommand);
                };
                let mut changes = vec![(LEVEL_CONTROL, CURRENT_LEVEL, Value::U8(level))];
                if command == MOVE_TO_LEVEL_WITH_ON_OFF {
                    changes.push((ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(level > 0)));
                }
                changes
            }
            _ => return Response::status(Status::UnsupportedCommand),
        };
        for (cluster, attribute, value) in changes {
            if let Err(error) =
                block_on(
                    self.node
                        .set_attribute(endpoint, cluster, attribute, value.clone()),
                )
            {
                eprintln!("onoff_node: set attribute: {error}");
                return Response::status(Status::Failure);
            }
            self.remember(endpoint, cluster, attribute, value);
        }
        Response::success()
    }

    /// Decides on a write: labels up to the device's limit, Level Control settings as they
    /// come; everything else is not writable here.
    fn handle_write(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> Status {
        let spec = self
            .model()
            .devices
            .get(&endpoint)
            .and_then(|device| device.spec(cluster, attribute).cloned());
        let Some(spec) = spec else {
            return Status::UnsupportedAttribute;
        };
        match (cluster, attribute) {
            (bridged_device::CLUSTER, NODE_LABEL) => match String::try_from(value) {
                Ok(label) if label.chars().count() <= MAX_LABEL_LEN => {
                    self.remember(endpoint, cluster, attribute, Value::Utf8(label));
                    Status::Success
                }
                Ok(_) => Status::ConstraintError,
                Err(_) => Status::InvalidDataType,
            },
            (LEVEL_CONTROL, OPTIONS | ON_LEVEL) => {
                if value.kind() == spec.kind || (spec.nullable && value.is_null()) {
                    self.remember(endpoint, cluster, attribute, value);
                    Status::Success
                } else {
                    Status::InvalidDataType
                }
            }
            _ => Status::UnsupportedWrite,
        }
    }
}

/// The `Level` field (tag 0) of a `MoveToLevel` payload.
fn level_field(payload: &Value) -> Option<u8> {
    let Value::Struct(fields) = payload else {
        return None;
    };
    fields
        .iter()
        .find(|(tag, _)| *tag == 0)
        .and_then(|(_, value)| match value {
            Value::U8(level) => Some(*level),
            Value::U16(level) => u8::try_from(*level).ok(),
            Value::U32(level) => u8::try_from(*level).ok(),
            Value::U64(level) => u8::try_from(*level).ok(),
            _ => None,
        })
}

/// The one place this example shows the codes, on explicit request.
fn print_pairing(manual_code: &str, qr_text: &str) {
    println!("onoff_node: pairing code {manual_code}");
    println!("onoff_node: qr text {qr_text}");
}

/// The initial light's persisted state, or its defaults.
fn load_state(storage: &dyn Storage) -> (bool, String) {
    let on = storage
        .load(KEY_ON)
        .ok()
        .flatten()
        .is_some_and(|bytes| bytes == b"1");
    let label = storage
        .load(KEY_LABEL)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| String::from("Light"));
    (on, label)
}

/// The `event ...` line for an engine event, with the status the product answered where
/// the event was a request.
fn event_line(event: &Event, status: Option<Status>) -> String {
    let status = status.map_or_else(String::new, |status| format!(" status={status:?}"));
    match event {
        Event::CommandInvoked {
            endpoint,
            cluster,
            command,
            ..
        } => format!(
            "event CommandInvoked endpoint={endpoint} cluster={cluster} command={command}{status}"
        ),
        Event::WriteRequested {
            endpoint,
            cluster,
            attribute,
            ..
        } => format!(
            "event WriteRequested endpoint={endpoint} cluster={cluster} attribute={attribute}{status}"
        ),
        Event::CommissioningChanged(state) => format!(
            "event CommissioningChanged commissioned={} window={:?}",
            state.commissioned, state.window
        ),
        Event::FabricAdded(fabric) => format!(
            "event FabricAdded index={} label={:?}",
            fabric.index, fabric.label
        ),
        Event::FabricRemoved(index) => format!("event FabricRemoved index={index}"),
        Event::Stopped(reason) => format!("event Stopped {reason:?}"),
        other => format!("event {other:?}"),
    }
}

fn main() {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("onoff_node: {message}");
            std::process::exit(2);
        }
    };
    if let Err(message) = run(options) {
        eprintln!("onoff_node: {message}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), String> {
    let identity = NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
        vendor_name: "matter-rs".into(),
        product_name: "On/Off node".into(),
        node_label: "On/Off node".into(),
        serial_number: None,
        software_version: 1,
        software_version_string: env!("CARGO_PKG_VERSION").into(),
        unique_id: "onoff-node".into(),
        passcode: Secret::new(options.passcode),
        discriminator: options.discriminator,
    };
    let network = NetworkConfig {
        interface: options.interface.clone(),
        ipv4: true,
        ipv6: !options.ipv4_only,
        port: options.port,
    };
    let storage: Arc<dyn Storage> = Arc::new(
        DirectoryStorage::open(&options.state_dir).map_err(|e| format!("state directory: {e}"))?,
    );
    let (on, label) = load_state(&*storage);

    if options.print_pairing {
        let info = matter_rs_engine_rsmatter::pairing_info_for(&identity)
            .map_err(|e| format!("pairing info: {e}"))?;
        print_pairing(info.manual_code.expose(), info.qr_text.expose());
    }

    let engine = RsMatterEngine::new();
    let node = block_on(engine.start(
        identity.clone(),
        network,
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::clone(&storage),
    ))
    .map_err(|e| format!("start: {e}"))?;
    let product = Arc::new(Product {
        node: Arc::from(node),
        model: Mutex::new(Model::default()),
        storage,
        identity,
    });
    if options.initial_device {
        let device = Device::new(LIGHT, DeviceKind::OnOffLight, None, &label, on);
        block_on(product.node.add_endpoint(device.description(LIGHT)))
            .map_err(|e| format!("add endpoint: {e}"))?;
        product.model().devices.insert(LIGHT, device);
    }
    let state = block_on(product.node.commissioning_state()).map_err(|e| format!("state: {e}"))?;
    println!(
        "onoff_node: ready on={on} label={label:?} commissioned={} window={:?} discriminator={}",
        state.commissioned, state.window, product.identity.discriminator
    );

    // The command thread reads standard input until `quit` or end of file; end of file does
    // not stop the node, so that it runs detached (without a standard input) until it is
    // killed. After a `quit` the main thread waits for the answer to be printed.
    let quit_requested = Arc::new(AtomicBool::new(false));
    let commands = {
        let product = Arc::clone(&product);
        let quit_requested = Arc::clone(&quit_requested);
        thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let outcome = parse_command(&line).and_then(|command| {
                    let quit = command == Command::Quit;
                    if quit {
                        quit_requested.store(true, Ordering::SeqCst);
                    }
                    product.execute(command).map(|()| quit)
                });
                match outcome {
                    Ok(quit) => {
                        println!("ok");
                        if quit {
                            break;
                        }
                    }
                    Err(reason) => println!("err {reason}"),
                }
            }
        })
    };

    loop {
        let event = match block_on(product.node.next_event()) {
            Ok(event) => event,
            Err(error) => {
                eprintln!("onoff_node: event stream ended: {error}");
                break;
            }
        };
        match &event {
            Event::CommandInvoked {
                correlation,
                endpoint,
                cluster,
                command,
                payload,
                ..
            } => {
                let response = product.handle_command(*endpoint, *cluster, *command, payload);
                println!("{}", event_line(&event, Some(response.status)));
                block_on(product.node.respond(*correlation, response))
                    .map_err(|e| format!("respond: {e}"))?;
            }
            Event::WriteRequested {
                correlation,
                endpoint,
                cluster,
                attribute,
                value,
                ..
            } => {
                let status = product.handle_write(*endpoint, *cluster, *attribute, value.clone());
                println!("{}", event_line(&event, Some(status)));
                block_on(product.node.respond(*correlation, Response::status(status)))
                    .map_err(|e| format!("respond: {e}"))?;
            }
            Event::Stopped(_) => {
                println!("{}", event_line(&event, None));
                break;
            }
            _ => println!("{}", event_line(&event, None)),
        }
    }
    if quit_requested.load(Ordering::SeqCst) {
        let _ = commands.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AttributeSpec, Command, Device, DeviceKind, Kind, Model, Value, attribute, event_line,
        level_field, parse_command, parse_value,
    };
    use matter_rs_engine::{
        AttributeId, ClusterId, CommandId, CommissioningState, Correlation, EndpointId, Event,
        FabricIndex, Status, StopReason, Window,
    };

    fn ep(id: u16) -> EndpointId {
        EndpointId::new(id)
    }

    #[test]
    fn every_protocol_command_parses() {
        assert_eq!(
            parse_command("add 3 contact-sensor"),
            Ok(Command::Add {
                endpoint: ep(3),
                kind: DeviceKind::ContactSensor,
                parent: None
            })
        );
        assert_eq!(
            parse_command("add 4 temperature-sensor 3"),
            Ok(Command::Add {
                endpoint: ep(4),
                kind: DeviceKind::TemperatureSensor,
                parent: Some(ep(3))
            })
        );
        assert_eq!(
            parse_command("remove 0x10"),
            Ok(Command::Remove { endpoint: ep(16) })
        );
        assert_eq!(
            parse_command("set 2 0x0006 0 true"),
            Ok(Command::Set {
                endpoint: ep(2),
                cluster: ClusterId::new(6),
                attribute: AttributeId::new(0),
                value: String::from("true")
            })
        );
        assert_eq!(
            parse_command("set 2 0x0039 0x0005 Living room"),
            Ok(Command::Set {
                endpoint: ep(2),
                cluster: ClusterId::new(0x39),
                attribute: AttributeId::new(5),
                value: String::from("Living room")
            })
        );
        assert_eq!(
            parse_command("reachable 2 false"),
            Ok(Command::Reachable {
                endpoint: ep(2),
                reachable: false
            })
        );
        assert_eq!(parse_command("pairing"), Ok(Command::Pairing));
        assert_eq!(parse_command("quit"), Ok(Command::Quit));
        assert_eq!(parse_command("stop"), Ok(Command::Quit));
    }

    #[test]
    fn malformed_commands_are_refused_with_a_reason() {
        let cases = [
            ("", "empty line"),
            ("fly 2", "unknown command"),
            ("add", "missing endpoint"),
            ("add 2", "missing device type"),
            ("add 2 toaster", "unknown device type"),
            ("add 70000 on-off-light", "exceeds 16 bits"),
            ("add x on-off-light", "not an id"),
            ("remove 2 3", "too many arguments"),
            ("set 2 0x0006 0", "missing value"),
            ("reachable 2 maybe", "not true or false"),
            ("pairing now", "too many arguments"),
        ];
        for (line, expected) in cases {
            let error = parse_command(line).expect_err(line);
            assert!(error.contains(expected), "{line:?}: {error}");
        }
    }

    #[test]
    fn values_are_parsed_in_the_attribute_kind() {
        let bool_spec = attribute(
            ClusterId::new(6),
            AttributeId::new(0),
            Kind::Bool,
            false,
            Value::Bool(false),
        );
        assert_eq!(parse_value(&bool_spec, "true"), Ok(Value::Bool(true)));
        assert!(
            parse_value(&bool_spec, "1")
                .unwrap_err()
                .contains("not a bool")
        );
        assert!(
            parse_value(&bool_spec, "null")
                .unwrap_err()
                .contains("not nullable")
        );

        let temperature = attribute(
            ClusterId::new(0x402),
            AttributeId::new(0),
            Kind::I16,
            true,
            Value::Null,
        );
        assert_eq!(parse_value(&temperature, "-1250"), Ok(Value::I16(-1250)));
        assert_eq!(parse_value(&temperature, "null"), Ok(Value::Null));
        assert!(
            parse_value(&temperature, "40000")
                .unwrap_err()
                .contains("not a i16")
        );

        let level = attribute(
            ClusterId::new(8),
            AttributeId::new(0),
            Kind::U8,
            true,
            Value::U8(0),
        );
        assert_eq!(parse_value(&level, "254"), Ok(Value::U8(254)));
        assert!(parse_value(&level, "-1").is_err());

        let label = attribute(
            ClusterId::new(0x39),
            AttributeId::new(5),
            Kind::Utf8,
            false,
            Value::from(""),
        );
        assert_eq!(
            parse_value(&label, "Living room"),
            Ok(Value::from("Living room"))
        );

        let list: AttributeSpec = attribute(
            ClusterId::new(1),
            AttributeId::new(1),
            Kind::List,
            false,
            Value::List(vec![]),
        );
        assert!(
            parse_value(&list, "x")
                .unwrap_err()
                .contains("cannot be set")
        );
    }

    #[test]
    fn device_kinds_have_names_types_and_consistent_descriptions() {
        for (name, kind) in DeviceKind::NAMES {
            assert_eq!(DeviceKind::parse(name), Some(kind));
            let device = Device::new(ep(5), kind, Some(ep(2)), "Label", true);
            let description = device.description(ep(5));
            assert_eq!(description.parent, Some(ep(2)));
            assert_eq!(description.device_types[0], kind.device_type());
            assert_eq!(
                description.device_types[1].id,
                super::DEVICE_TYPE_BRIDGED_NODE
            );
            // Every listed attribute has an initial value of its kind (or null when nullable).
            for cluster in &description.clusters {
                for attribute in &cluster.attributes {
                    let spec = device.spec(cluster.id, *attribute).expect("a spec");
                    let (_, _, initial) = description
                        .initial_values
                        .iter()
                        .find(|(c, a, _)| *c == cluster.id && a == attribute)
                        .expect("an initial value");
                    assert!(
                        initial.kind() == spec.kind || (spec.nullable && initial.is_null()),
                        "{name}: {cluster:?}/{attribute} initial {initial:?} is not a {}",
                        spec.kind
                    );
                }
            }
            assert!(
                description
                    .clusters
                    .iter()
                    .any(|c| c.id == matter_rs_engine::bridged_device::CLUSTER)
            );
        }
        assert_eq!(DeviceKind::parse("toaster"), None);
        let light = Device::new(ep(2), DeviceKind::OnOffLight, None, "Light", true);
        assert!(light.is_on());
        assert_eq!(
            light
                .values
                .get(&(super::bridged_device::CLUSTER, super::UNIQUE_ID)),
            Some(&Value::from("onoff-node-2"))
        );
    }

    #[test]
    fn model_removal_cascades_to_composed_parts() {
        let mut model = Model::default();
        model.devices.insert(
            ep(2),
            Device::new(ep(2), DeviceKind::OnOffLight, None, "a", false),
        );
        model.devices.insert(
            ep(3),
            Device::new(ep(3), DeviceKind::ContactSensor, Some(ep(2)), "b", false),
        );
        model.devices.insert(
            ep(4),
            Device::new(
                ep(4),
                DeviceKind::TemperatureSensor,
                Some(ep(3)),
                "c",
                false,
            ),
        );
        model.devices.insert(
            ep(5),
            Device::new(ep(5), DeviceKind::DimmableLight, None, "d", false),
        );
        model.remove(ep(2));
        let left: Vec<EndpointId> = model.devices.keys().copied().collect();
        assert_eq!(left, vec![ep(5)]);
    }

    #[test]
    fn level_field_reads_the_first_field() {
        assert_eq!(
            level_field(&Value::Struct(vec![(0, Value::U8(7)), (1, Value::Null)])),
            Some(7)
        );
        assert_eq!(
            level_field(&Value::Struct(vec![(0, Value::U16(300))])),
            None
        );
        assert_eq!(level_field(&Value::Null), None);
    }

    #[test]
    fn event_lines_name_kind_and_path() {
        let command = Event::CommandInvoked {
            correlation: Correlation::new(1),
            endpoint: ep(2),
            cluster: ClusterId::new(6),
            command: CommandId::new(2),
            payload: Value::Null,
            fabric: None,
        };
        assert_eq!(
            event_line(&command, Some(Status::Success)),
            "event CommandInvoked endpoint=2 cluster=0x0006 command=0x0002 status=Success"
        );
        let write = Event::WriteRequested {
            correlation: Correlation::new(1),
            endpoint: ep(2),
            cluster: ClusterId::new(0x39),
            attribute: AttributeId::new(5),
            value: Value::from("x"),
            fabric: None,
        };
        assert_eq!(
            event_line(&write, Some(Status::ConstraintError)),
            "event WriteRequested endpoint=2 cluster=0x0039 attribute=0x0005 status=ConstraintError"
        );
        let changed = Event::CommissioningChanged(CommissioningState {
            commissioned: true,
            window: Some(Window::Basic),
        });
        assert_eq!(
            event_line(&changed, None),
            "event CommissioningChanged commissioned=true window=Some(Basic)"
        );
        assert_eq!(
            event_line(&Event::FabricRemoved(FabricIndex::new(3)), None),
            "event FabricRemoved index=3"
        );
        assert_eq!(
            event_line(&Event::Stopped(StopReason::Requested), None),
            "event Stopped Requested"
        );
    }
}
