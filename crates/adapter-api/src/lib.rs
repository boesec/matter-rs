//! Adapter contract of matter-rs (layer 3).
//!
//! Adapters bring devices and their values into the product and carry commands back out. This
//! crate defines the trait and the types every adapter implements, so that adapters depend on
//! one stable contract and the roles never know which adapters exist. Adapters are Rust crates
//! selected at compile time by Cargo features; the binary enables the role-specific parts of
//! this contract with the same `role-*` features it uses for the roles themselves.
//!
//! # The contract
//!
//! - An adapter is a task: [`Adapter::run`] consumes the adapter and returns the future of that
//!   task. The future ends when the adapter stops on its own or when the host asks it to;
//!   returning an error or panicking is a failure.
//! - The [`AdapterContext`] is the adapter's connection to the product: its [`AdapterId`], the
//!   [`Health`] it reports, the shutdown signal and, with the bridge role, the
//!   [`DeviceRegistrar`] (add, remove and forget devices through the typed device API of
//!   `matter-rs-bridge`) and the [`CommandStream`] of the commands for its own devices.
//! - The [`AdapterFactory`] (`std`) builds instances from a configuration section; it is the
//!   piece the configuration crate composes its adapter kinds from.
//! - The [`Supervisor`] (`std` and `role-bridge`) runs adapter instances on tokio in isolation:
//!   one task each, a failed adapter's devices unreachable at once, restart with exponential
//!   backoff, commands routed by device ownership, orderly shutdown.
//! - The [`testing`] module (`testing`) holds a mock adapter the test drives.
//!
//! The contract itself is `no_std` with `alloc` and runtime-neutral: it uses `core::future`
//! only. The supervisor is the one piece that names a runtime.
//!
//! # Walkthrough
//!
//! A minimal adapter that adds one light and logs its commands, run under the supervisor on the
//! fake engine. The test side plays the Matter client with the fake engine's hooks. The same
//! adapter runs unchanged in the service against a real engine.
//!
//! ```
//! # #[cfg(all(feature = "role-bridge", feature = "testing"))]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use std::sync::Arc;
//!
//! use matter_rs_adapter_api::{
//!     Adapter, AdapterContext, AdapterId, BoxFuture, Error, Health, Supervisor,
//!     SupervisorOptions, TokioTimer,
//! };
//! use matter_rs_bridge::{
//!     Attribute, Bridge, DeviceCommand, DeviceKey, DeviceKind, DeviceSpec, InitialValues,
//!     Options,
//! };
//! use matter_rs_engine::testing::FakeEngine;
//! use matter_rs_engine::{
//!     Attestation, AttributeId, ClusterId, CommandId, EndpointId, MemoryStorage, NetworkConfig,
//!     NodeIdentity, ProductId, Secret, Status, Value, VendorId,
//! };
//!
//! /// An adapter with one light that is always off until a controller switches it.
//! struct Lamp;
//!
//! impl Adapter for Lamp {
//!     fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
//!         Box::pin(async move {
//!             let spec = DeviceSpec::single(DeviceKey::new("lamp")?, "Lamp", DeviceKind::OnOffLight);
//!             let lamp = ctx
//!                 .devices()
//!                 .add(spec, InitialValues::new().value(Attribute::OnOff, false))
//!                 .await?;
//!             ctx.set_health(Health::Healthy);
//!             let mut commands = ctx.commands();
//!             let mut shutdown = ctx.shutdown();
//!             loop {
//!                 tokio::select! {
//!                     () = &mut shutdown => return Ok(()),
//!                     command = commands.next() => {
//!                         let Some(command) = command else { return Ok(()) };
//!                         if let DeviceCommand::SetOnOff { on } = command.command {
//!                             // Tell the device; here the light is the value itself.
//!                             lamp.set(0, Attribute::OnOff, on).await?;
//!                         }
//!                     }
//!                 }
//!             }
//!         })
//!     }
//! }
//!
//! // 1. A bridge on the fake engine, with the runtime's timer for raw responses.
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
//! let bridge = Bridge::start_with(
//!     Box::new(fake.clone()),
//!     identity,
//!     NetworkConfig::default(),
//!     Attestation::Test,
//!     Arc::new(MemoryStorage::new()),
//!     Options {
//!         timer: Some(Arc::new(TokioTimer)),
//!         ..Options::default()
//!     },
//! )
//! .await
//! .expect("the bridge starts");
//!
//! // 2. The supervisor runs the adapter; the factory builds a fresh one for every start.
//! let supervisor = Supervisor::new(bridge, SupervisorOptions::default());
//! supervisor
//!     .add(AdapterId::new("lamp", "hall"), || Ok(Box::new(Lamp)))
//!     .unwrap();
//! let running = tokio::spawn({
//!     let supervisor = supervisor.clone();
//!     async move { supervisor.run().await }
//! });
//!
//! // 3. The adapter adds its light (endpoint 2) and reports healthy.
//! while supervisor.bridge().device(&DeviceKey::new("lamp").unwrap()).is_none() {
//!     tokio::task::yield_now().await;
//! }
//! assert_eq!(supervisor.health()[0].1, Health::Healthy);
//!
//! // 4. A Matter client switches the light on: the bridge answers and the adapter is told.
//! const ON_OFF: ClusterId = ClusterId::new(0x0006);
//! const ON: CommandId = CommandId::new(0x01);
//! let response = fake.invoke(EndpointId::new(2), ON_OFF, ON, Value::Null).await.unwrap();
//! assert_eq!(response.status, Status::Success);
//! assert_eq!(
//!     fake.read(EndpointId::new(2), ON_OFF, AttributeId::new(0)),
//!     Some(Value::Bool(true))
//! );
//!
//! // 5. Shutdown stops the adapter, then the bridge.
//! supervisor.shutdown();
//! running.await.unwrap().unwrap();
//! assert_eq!(supervisor.health()[0].1, Health::Stopped);
//! assert!(!fake.is_running());
//! # }
//! # #[cfg(not(all(feature = "role-bridge", feature = "testing")))]
//! # fn main() {}
//! ```
//!
//! The author guide in `doc/adapters.md` of the repository walks through a complete adapter
//! crate: the factory, the configuration wiring, device kinds and commands, isolation rules and
//! testing.
//!
//! Unsafe code is forbidden in every crate of this workspace, twice over: by the workspace lint
//! and by the attribute at the crate root; doc tests inherit the rule through
//! `#![doc(test(attr(forbid(unsafe_code))))]`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

extern crate alloc;

mod adapter;
mod context;
mod error;
#[cfg(feature = "std")]
mod factory;
mod health;
mod id;
#[cfg(all(feature = "std", feature = "role-bridge"))]
mod supervisor;
mod sync;
#[cfg(feature = "testing")]
pub mod testing;

pub use adapter::Adapter;
pub use context::{AdapterContext, Shutdown};
#[cfg(feature = "role-bridge")]
pub use context::{CommandStream, DeviceRegistrar};
pub use error::Error;
#[cfg(feature = "std")]
pub use factory::AdapterFactory;
pub use health::Health;
pub use id::AdapterId;
pub use matter_rs_engine::BoxFuture;
#[cfg(all(feature = "std", feature = "role-bridge"))]
pub use supervisor::{EventStream, Factory, Supervisor, SupervisorOptions, TokioTimer};

/// The public adapter author guide, compiled as doc tests so that its examples stay current.
#[cfg(all(doctest, feature = "testing"))]
#[doc = include_str!("../../../doc/adapters.md")]
pub struct AuthorGuide;
