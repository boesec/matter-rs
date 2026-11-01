//! Engine facade of matter-rs (layer 0).
//!
//! This crate is the engine-neutral contract between the product crates (bridge, adapters,
//! configuration, service) and the Matter engine that implements it. It is phrased in the
//! product's own terms, with dynamic values, and knows nothing about any particular engine: the
//! only crate that depends on an engine is the engine implementation crate, which implements this
//! contract. Everything above it sees only this crate.
//!
//! The crate is `no_std` with `alloc` unless the default `std` feature is enabled, so the same
//! contract serves Linux and Windows services and, later, microcontrollers. Its asynchronous
//! parts are executor-neutral: they use `core::future` only and never name a runtime.
//!
//! # What is here
//!
//! - **Ids** ([`EndpointId`], [`ClusterId`], [`AttributeId`], ...) and the dynamic [`Value`]
//!   that maps one to one onto Matter TLV, with conversions from and to Rust types.
//! - **Descriptions** of what the product puts on the node: [`DeviceType`],
//!   [`ClusterDescription`], [`EndpointDescription`].
//! - **Identity**: [`NodeIdentity`], [`NetworkConfig`], [`Attestation`], [`NodeRole`].
//! - **The storage contract** [`Storage`], with [`DirectoryStorage`] and [`MemoryStorage`]
//!   behind the `std` feature.
//! - **The engine contract**: [`Engine`] starts a node, [`Running`] drives it, [`Event`]s come
//!   back, [`Response`]s with a [`Status`] go in. The guarantees every implementation keeps are
//!   listed in the [`engine`] module.
//! - **Errors** ([`Error`]) and the [`Secret`] wrapper for passcodes, keys and pairing codes.
//! - **The fake engine** [`testing::FakeEngine`] behind the `testing` feature, for testing
//!   product code without a Matter stack.
//!
//! # Where the semantics live
//!
//! The facade carries raw ids and dynamic values. The engine implements no cluster logic on the
//! endpoints the product adds: commands and writes reach the product as events, the product
//! answers them, and the engine reports the attribute values the product sets. Reads never reach
//! the product and the engine never invents a value: every attribute the product lists has an
//! initial value, or the endpoint is refused.
//!
//! # Walkthrough
//!
//! Start a node on the fake engine, add an endpoint with an initial value, change the value,
//! receive a command and answer it. The product side runs on its own thread and pulls events
//! with [`Running::next_event`]; the test side plays the Matter client with
//! [`testing::FakeEngine::invoke`]. The same code runs unchanged against a real engine, with the
//! product's own executor in place of [`testing::block_on`].
//!
#![cfg_attr(feature = "testing", doc = "```")]
#![cfg_attr(not(feature = "testing"), doc = "```ignore")]
//! use std::sync::Arc;
//! use std::thread;
//!
//! use matter_rs_engine::testing::{block_on, FakeEngine};
//! use matter_rs_engine::{
//!     Attestation, AttributeId, ClusterDescription, ClusterId, CommandId, DeviceType,
//!     DeviceTypeId, EndpointDescription, EndpointId, Engine, Event, MemoryStorage,
//!     NetworkConfig, NodeIdentity, NodeRole, ProductId, Response, Running, Secret, Status,
//!     Value, VendorId,
//! };
//!
//! // Ids of the On/Off cluster, its OnOff attribute and its On command. The facade does not
//! // know them; the product does.
//! const ON_OFF: ClusterId = ClusterId::new(0x0006);
//! const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0x0000);
//! const ON: CommandId = CommandId::new(0x01);
//! const ON_OFF_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0100);
//! const LIGHT: EndpointId = EndpointId::new(2);
//!
//! let fake = FakeEngine::new();
//! let engine: Box<dyn Engine> = Box::new(fake.clone());
//!
//! // 1. Start the node.
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
//! let node = block_on(engine.start(
//!     identity,
//!     NetworkConfig::default(),
//!     Attestation::Test,
//!     NodeRole::Bridge { aggregator: EndpointId::new(1) },
//!     Arc::new(MemoryStorage::new()),
//! ))
//! .expect("the node starts");
//! let node: Arc<dyn Running> = Arc::from(node);
//!
//! // 2. Add an endpoint: an On/Off light that is off.
//! block_on(node.add_endpoint(EndpointDescription {
//!     id: LIGHT,
//!     device_types: vec![DeviceType { id: ON_OFF_LIGHT, revision: 3 }],
//!     clusters: vec![ClusterDescription {
//!         id: ON_OFF,
//!         revision: 6,
//!         feature_map: 0,
//!         attributes: vec![ON_OFF_ATTRIBUTE],
//!         accepted_commands: vec![ON],
//!         generated_commands: vec![],
//!         events: vec![],
//!     }],
//!     parent: None,
//!     initial_values: vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))],
//! }))
//! .expect("the endpoint is added");
//! assert_eq!(fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE), Some(Value::Bool(false)));
//!
//! // 3. Set a value: the engine reports it to subscribers and answers reads from it.
//! block_on(node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))).unwrap();
//! assert_eq!(fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE), Some(Value::Bool(true)));
//!
//! // 4. The product task: pull events, apply the cluster semantics, respond.
//! let product = {
//!     let node = Arc::clone(&node);
//!     thread::spawn(move || {
//!         while let Ok(event) = block_on(node.next_event()) {
//!             match event {
//!                 Event::CommandInvoked { correlation, endpoint, cluster, command, .. }
//!                     if cluster == ON_OFF && command == ON =>
//!                 {
//!                     block_on(node.set_attribute(endpoint, cluster, ON_OFF_ATTRIBUTE, Value::Bool(true)))
//!                         .unwrap();
//!                     block_on(node.respond(correlation, Response::success())).unwrap();
//!                 }
//!                 Event::CommandInvoked { correlation, .. } | Event::WriteRequested { correlation, .. } => {
//!                     block_on(node.respond(correlation, Response::status(Status::UnsupportedCommand)))
//!                         .unwrap();
//!                 }
//!                 Event::Stopped(_) => break,
//!                 _ => {}
//!             }
//!         }
//!     })
//! };
//!
//! // 5. A Matter client turns the light on.
//! block_on(node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))).unwrap();
//! let response = block_on(fake.invoke(LIGHT, ON_OFF, ON, Value::Null)).unwrap();
//! assert_eq!(response.status, Status::Success);
//! assert_eq!(fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE), Some(Value::Bool(true)));
//!
//! // 6. Stop: the product task sees `Event::Stopped` and ends.
//! block_on(node.stop()).unwrap();
//! product.join().unwrap();
//! ```
//!
//! Unsafe code is forbidden in every crate of this workspace, twice over: by the workspace lint
//! `unsafe_code = "forbid"` and by the `#![forbid(unsafe_code)]` attribute at the crate root.
//! Doc tests are compiled as separate crates and do not inherit either, so every crate passes
//! the same rule on to them with `#![doc(test(attr(forbid(unsafe_code))))]`. The following block
//! is otherwise valid Rust and must not compile:
//!
//! ```compile_fail
//! let value = 1_u8;
//! let pointer = &raw const value;
//! // SAFETY: `pointer` points to `value`, which is alive and initialised.
//! let copy = unsafe { *pointer };
//! assert_eq!(copy, 1);
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

extern crate alloc;

mod description;
pub mod engine;
mod error;
mod id;
mod identity;
mod secret;
mod status;
mod storage;
#[cfg(feature = "testing")]
pub mod testing;
mod value;

pub use description::{ClusterDescription, DeviceType, EndpointDescription};
pub use engine::{
    AttributeValue, BoxFuture, Capabilities, CommissioningState, Correlation, Engine, Event,
    FabricInfo, PairingInfo, Response, Running, StopReason, Window, bridged_device,
};
pub use error::Error;
pub use id::{
    AttributeId, ClusterId, CommandId, DeviceTypeId, EndpointId, EventId, FabricIndex, ProductId,
    ROOT_ENDPOINT, VendorId,
};
pub use identity::{
    Attestation, INVALID_PASSCODES, MATTER_PORT, MAX_DISCRIMINATOR, MAX_PASSCODE, NetworkConfig,
    NodeIdentity, NodeRole, is_valid_passcode,
};
pub use secret::{REDACTED, Secret};
pub use status::Status;
#[cfg(feature = "std")]
pub use storage::{DirectoryStorage, MemoryStorage};
pub use storage::{Storage, StorageError};
pub use value::{Kind, Value, ValueError};
