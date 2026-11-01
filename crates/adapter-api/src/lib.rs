//! Adapter contract of matter-rs (layer 3).
//!
//! Adapters bring devices and their values into the product and carry commands back out. This
//! crate defines the trait and the types every adapter implements, so that adapters depend on
//! one stable contract and the roles never know which adapters exist. Adapters are Rust crates
//! selected at compile time by Cargo features; the binary enables the role-specific parts of
//! this contract with the same `role-*` features it uses for the roles themselves.
//!
//! The crate is `no_std` with `alloc` unless the default `std` feature is enabled. Today it holds
//! only its error type; the trait follows in a later step.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

extern crate alloc;

mod error;

pub use error::Error;
