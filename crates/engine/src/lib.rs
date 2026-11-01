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
//!
//! # Where the semantics live
//!
//! The facade carries raw ids and dynamic values. The engine implements no cluster logic on the
//! endpoints the product adds: commands and writes reach the product as events, the product
//! answers them, and the engine reports the attribute values the product sets. Reads never reach
//! the product and the engine never invents a value: every attribute the product lists has an
//! initial value, or the endpoint is refused.
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
