//! Bridge role of matter-rs (layer 2).
//!
//! The bridge turns devices supplied by adapters into bridged Matter endpoints behind one
//! aggregator. It owns the endpoint numbering, the device kinds with their clusters, the Bridged
//! Device Basic Information of every device and the cluster semantics, so that adapters work
//! with typed device commands and attributes instead of Matter ids. It sees the Matter engine
//! only through the engine facade and never names an engine implementation.
//!
//! The crate is `no_std` with `alloc` unless the default `std` feature is enabled, and
//! runtime-neutral: its futures use `core::future` only.
//!
//! # Model
//!
//! - A [`DeviceSpec`] describes a device: a stable [`DeviceKey`] (the id the configuration gives
//!   it), a name and one or more [`PartSpec`]s, each a [`DeviceKind`]. A device with one part is
//!   one endpoint; a device with several parts is composed: a Bridged Node parent endpoint with
//!   one child endpoint per part, and semantic tags on children that share a kind.
//! - Endpoint numbers are stable across restarts: a persisted map from device key and part to
//!   endpoint number plus a counter, kept in the storage under `bridge/endpoints/` (see
//!   [`numbering`]). The aggregator is endpoint 1, the first bridged endpoint 2. A removed device
//!   keeps its numbers until it is forgotten.
//! - Every bridged device carries the Bridged Device Basic Information cluster on its top
//!   endpoint: `NodeLabel` (writable by controllers, persisted, reported as
//!   [`DeviceCommand::Rename`]), `Reachable` (with `ReachableChanged`) and a stable `UniqueID`
//!   derived from the key.
//! - Cluster semantics are pass-through with target values: a Matter command sets the attribute
//!   values it defines and becomes one typed [`DeviceCommand`] for the adapter; transitions are
//!   hints, nothing is animated here. Writable attributes such as `OnLevel`, `StartUpOnOff`,
//!   `StartUpCurrentLevel`, `Options` and `StartUpColorTemperatureMireds` are stored, persisted
//!   under `bridge/attributes/`, and the start-up attributes are applied when the device is added
//!   again, only if a controller wrote them: without a persisted value the adapter's first value
//!   stands. The bridge invents no value: every attribute has a default the specification names
//!   or is required from the adapter.
//! - Commands and writes on the extra clusters a part may list reach the adapter as
//!   [`DeviceCommand::Raw`] and [`DeviceCommand::Write`] with a [`ResponseHandle`]; an answer
//!   that does not come in time is `Failure`.
//!
//! The device kinds and their clusters are listed at [`DeviceKind`]; the Matter ids come from the
//! Matter 1.6 data model published in the `data_model/1.6` directory of
//! <https://github.com/project-chip/connectedhomeip> and are collected in [`ids`].
//!
//! # Walkthrough
//!
//! Start a bridge on the fake engine, add a composed device (a power strip with two sockets and
//! a temperature sensor), receive a command from a Matter client and report a value. The product
//! task pulls commands with [`Bridge::next_command`]; the test side plays the client with the fake
//! engine's hooks. The same code runs unchanged against a real engine, with the product's own
//! executor in place of `block_on`.
//!
//! ```
//! use std::sync::Arc;
//! use std::thread;
//!
//! use matter_rs_bridge::{
//!     Attribute, Bridge, DeviceCommand, DeviceKey, DeviceKind, DeviceSpec, InitialValues,
//!     PartSpec,
//! };
//! use matter_rs_engine::testing::{block_on, FakeEngine};
//! use matter_rs_engine::{
//!     Attestation, AttributeId, ClusterId, CommandId, EndpointId, MemoryStorage, NetworkConfig,
//!     NodeIdentity, ProductId, Secret, Status, Value, VendorId,
//! };
//!
//! let fake = FakeEngine::new();
//! let identity = NodeIdentity {
//!     vendor_id: VendorId::new(0xFFF1),
//!     product_id: ProductId::new(0x8001),
//!     vendor_name: "matter-rs".into(),
//!     product_name: "Bridge".into(),
//!     node_label: "Bridge".into(),
//!     serial_number: None,
//!     software_version: 1,
//!     software_version_string: "0.1.0".into(),
//!     unique_id: "bridge-1".into(),
//!     passcode: Secret::new(20_202_021),
//!     discriminator: 3840,
//! };
//!
//! // 1. Start the bridge: the node has its root and aggregator endpoints.
//! let bridge = Arc::new(
//!     block_on(Bridge::start(
//!         Box::new(fake.clone()),
//!         identity,
//!         NetworkConfig::default(),
//!         Attestation::Test,
//!         Arc::new(MemoryStorage::new()),
//!     ))
//!     .expect("the bridge starts"),
//! );
//!
//! // 2. Add a composed device. Every value the kinds require comes from the adapter.
//! let spec = DeviceSpec {
//!     key: DeviceKey::new("strip-1").unwrap(),
//!     name: "Power strip".into(),
//!     parts: vec![
//!         PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Left"),
//!         PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Right"),
//!         PartSpec::new(DeviceKind::TemperatureSensor),
//!     ],
//! };
//! let initial = InitialValues::new()
//!     .with(0, Attribute::OnOff, false)
//!     .with(1, Attribute::OnOff, true)
//!     .with(2, Attribute::Temperature, Value::I16(2150));
//! let strip = block_on(bridge.add_device(spec, initial)).expect("the device is added");
//!
//! // The parent is endpoint 2, the parts are 3, 4 and 5; the sockets carry number tags.
//! let endpoints = strip.endpoints().unwrap();
//! assert_eq!(endpoints[0].endpoint, EndpointId::new(2));
//! assert_eq!(endpoints[0].part, None);
//! assert_eq!(endpoints[1].endpoint, EndpointId::new(3));
//! assert_eq!(endpoints[1].tags[0].label.as_deref(), Some("Left"));
//! assert_eq!(fake.endpoints().len(), 4);
//!
//! // 3. The product task: pull commands and pass them to the adapter.
//! let (sender, receiver) = std::sync::mpsc::channel();
//! let product = {
//!     let bridge = Arc::clone(&bridge);
//!     thread::spawn(move || {
//!         while let Ok(command) = block_on(bridge.next_command()) {
//!             if sender.send(command).is_err() {
//!                 break;
//!             }
//!         }
//!     })
//! };
//!
//! // 4. A Matter client turns the left socket on: the bridge sets `OnOff`, answers the client
//! //    and hands the adapter a typed command.
//! const ON_OFF: ClusterId = ClusterId::new(0x0006);
//! const ON: CommandId = CommandId::new(0x01);
//! let response = block_on(fake.invoke(EndpointId::new(3), ON_OFF, ON, Value::Null)).unwrap();
//! assert_eq!(response.status, Status::Success);
//! assert_eq!(
//!     fake.read(EndpointId::new(3), ON_OFF, AttributeId::new(0)),
//!     Some(Value::Bool(true))
//! );
//! let command = receiver.recv().unwrap();
//! assert_eq!(command.key.as_str(), "strip-1");
//! assert_eq!(command.part, Some(0));
//! assert_eq!(command.command, DeviceCommand::SetOnOff { on: true });
//!
//! // 5. The adapter reports a new temperature and the device going unreachable.
//! block_on(strip.set(2, Attribute::Temperature, Value::I16(2200))).unwrap();
//! block_on(strip.set_reachable(false)).unwrap();
//! assert_eq!(
//!     fake.read(EndpointId::new(5), ClusterId::new(0x0402), AttributeId::new(0)),
//!     Some(Value::I16(2200))
//! );
//! assert!(!strip.is_reachable());
//!
//! // 6. Stop: the product task sees the end of the event stream.
//! block_on(bridge.stop()).unwrap();
//! product.join().unwrap();
//! ```
//!
//! Unsafe code is forbidden in every crate of this workspace, twice over: by the workspace lint
//! and by the attribute at the crate root; doc tests inherit the rule through
//! `#![doc(test(attr(forbid(unsafe_code))))]`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

extern crate alloc;

mod bridge;
mod command;
mod encoding;
mod error;
mod events;
pub mod ids;
mod kinds;
mod model;
pub mod numbering;
mod plan;
mod semantics;
mod sync;
mod timer;

pub use bridge::{
    ATTRIBUTES_KEY_PREFIX, Bridge, DEFAULT_RESPONSE_TIMEOUT, DeviceHandle, Options, ResponseHandle,
};
pub use command::{BridgeEvent, CommandEvent, DeviceCommand};
pub use error::Error;
pub use kinds::DeviceKind;
pub use model::{
    Attribute, DeviceEvent, DeviceKey, DeviceSpec, InitialValues, MAX_DEVICE_KEY_LENGTH, MAX_PARTS,
    PartIndex, PartSpec, SemanticTag,
};
pub use plan::{EndpointPlan, unique_id};
#[cfg(feature = "std")]
pub use timer::ThreadTimer;
pub use timer::Timer;
