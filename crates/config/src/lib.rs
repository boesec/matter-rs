//! Configuration of matter-rs (layer 5).
//!
//! The service is configured by one TOML file. This crate owns the config model, its validation
//! and its schema, and keeps configuration apart from runtime state. It assembles the sections of
//! the roles and adapters that the binary's features select, which is why the same `role-*` and
//! `adapter-*` features exist here and are forwarded to the crates they name.
//!
//! The crate needs the standard library (files). Today it holds only its error type; the model
//! follows in a later step.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

mod error;

pub use error::Error;
