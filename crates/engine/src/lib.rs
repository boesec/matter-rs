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
//! What lives here today is the error type and the [`Secret`] wrapper. The ids, dynamic values,
//! endpoint descriptions, events and the storage contract follow when the facade is designed.
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

mod error;
mod secret;

pub use error::Error;
pub use secret::Secret;
