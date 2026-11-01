//! MQTT adapter of matter-rs (layer 4).
//!
//! The first adapter and the universal route for external programs: devices and values arrive
//! on MQTT topics laid out along the
//! [mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) conventions, and commands
//! leave the same way. The adapter implements the adapter contract and sees the roles only
//! through it and the role crates selected by the `role-*` features.
//!
//! The crate needs the standard library (network I/O). Today it holds only its error type; the
//! adapter follows in a later step.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

mod error;

pub use error::Error;
