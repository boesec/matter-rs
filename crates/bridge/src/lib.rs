//! Bridge role of matter-rs (layer 2).
//!
//! The bridge role turns devices supplied by adapters into bridged Matter endpoints behind one
//! aggregator: it owns the endpoint numbering, the device types and the mapping between adapter
//! values and cluster attributes. It sees the Matter engine only through the engine facade and
//! never names an engine implementation.
//!
//! The crate is `no_std` with `alloc` unless the default `std` feature is enabled. Today it holds
//! only its error type; the role itself follows in a later step.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

extern crate alloc;

mod error;

pub use error::Error;
