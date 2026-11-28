//! MQTT adapter of matter-rs (layer 4).
//!
//! The first adapter and the universal route for external programs. Devices of other
//! [mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) adapters (and of any
//! MQTT source with configurable topics) become bridged devices: their status items feed the
//! typed attributes of the bridge, device commands of Matter controllers leave as `set`
//! messages, and the bridge itself appears on the broker as an mqtt-smarthome 2.0 instance
//! (`<name>/connected`, `<name>/info`, `<name>/status/...`). The public contract is
//! `doc/mqtt.md` in the repository.
//!
//! - [`Config`] is the `[adapters.<id>]` section of kind `mqtt`; [`Mapping`] the `mapping` of a
//!   `[devices.<id>]` table on such an adapter. Both carry serde and schemars derives, so the
//!   configuration crate composes them into its schema.
//! - [`MqttFactory`] implements the adapter factory of the contract; [`MqttAdapter::build`] is
//!   the constructor the service uses, with the devices, the node's identity and the channel
//!   that tells the adapter whether the node runs.
//! - [`mapping::resolve`] turns a device definition into topics and [`Transform`]s and reports
//!   every problem with its key path; the configuration loader runs it for its diagnostics.
//! - [`payload`] parses whatever a broker delivers without a panic; the `fuzz/` crate feeds it.
//!
//! The crate needs the standard library (network I/O on tokio). Everything that touches devices
//! exists with the `role-bridge` feature.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

#[cfg(feature = "role-bridge")]
mod adapter;
#[cfg(feature = "role-bridge")]
pub mod attributes;
mod config;
mod error;
pub mod instance;
#[cfg(feature = "role-bridge")]
pub mod mapping;
pub mod payload;
pub mod transform;

#[cfg(feature = "role-bridge")]
pub use adapter::{KIND, MqttAdapter, MqttFactory, NodeState, NodeStateReceiver, no_node_state};
pub use config::{
    BrokerUrl, Config, ConfigIssue, DEFAULT_KEEP_ALIVE, DEFAULT_NAME, DEFAULT_PORT,
    DEFAULT_RECONNECT_INITIAL, DEFAULT_RECONNECT_MAX, DEFAULT_TLS_PORT, Reconnect, SecretText,
    UrlProblem,
};
pub use error::Error;
pub use instance::BridgeIdentity;
#[cfg(feature = "role-bridge")]
pub use mapping::{
    DeviceDefinition, ItemMapping, Mapping, MappingError, PartDefinition, PartMapping,
};
pub use transform::{Rejected, Transform, ValueType};
