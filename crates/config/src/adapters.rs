//! Composition of the adapter sections.
//!
//! Every `[adapters.<id>]` table names its adapter with `kind`; the other keys belong to the
//! adapter. Each adapter crate exports its config type (with serde and schemars derives), and
//! this crate adds it behind the adapter's Cargo feature in two places:
//!
//! 1. a variant of [`AdapterConfig`], whose serde tag is the kind, for example
//!    `#[cfg(feature = "adapter-mqtt")] Mqtt(matter_rs_adapter_mqtt::Config)`;
//! 2. an entry in [`adapter_kinds`] that names the kind and its feature. The entry exists in
//!    every build, so that a kind whose feature is missing is reported as "not compiled in",
//!    naming the feature, instead of as an unknown kind.
//!
//! An adapter that feeds devices also exports the type of a device's `mapping` table; it is
//! added to [`mapping_schema`], which the device check and the reference use.
//!
//! The schema of the whole file therefore contains exactly the adapter kinds the build has.
//! Builds of this crate's own unit tests add a test adapter to prove the mechanism.

use schemars::JsonSchema;
use serde::Deserialize;

/// One adapter kind the configuration knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterKind {
    /// The value of `kind` that selects the adapter.
    pub kind: &'static str,
    /// The Cargo feature that compiles the adapter in.
    pub feature: &'static str,
    /// Whether this build contains the adapter.
    pub compiled_in: bool,
}

/// Every adapter kind the configuration knows about, compiled in or not.
#[must_use]
pub fn adapter_kinds() -> &'static [AdapterKind] {
    KINDS
}

const KINDS: &[AdapterKind] = &[
    AdapterKind {
        kind: "mqtt",
        feature: "adapter-mqtt",
        compiled_in: cfg!(feature = "adapter-mqtt"),
    },
    #[cfg(test)]
    AdapterKind {
        kind: "test",
        feature: "adapter-test",
        compiled_in: true,
    },
    #[cfg(test)]
    AdapterKind {
        kind: "absent",
        feature: "adapter-absent",
        compiled_in: false,
    },
];

/// The configuration of one adapter instance, selected by its `kind`.
///
/// The enum has one variant per adapter compiled into this build.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[schemars(
    description = "One adapter instance: `kind` selects the adapter, the other keys belong to it."
)]
#[non_exhaustive]
pub enum AdapterConfig {
    /// An MQTT broker; see the MQTT contract in `doc/mqtt.md`.
    #[cfg(feature = "adapter-mqtt")]
    Mqtt(matter_rs_adapter_mqtt::Config),
    /// An adapter that exists only in this crate's unit tests.
    #[cfg(test)]
    Test(TestAdapterConfig),
}

impl AdapterConfig {
    /// The `kind` that selected this adapter.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match *self {
            #[cfg(feature = "adapter-mqtt")]
            Self::Mqtt(_) => "mqtt",
            #[cfg(test)]
            Self::Test(_) => "test",
        }
    }
}

/// The JSON schema of the `mapping` table of a device on an adapter of `kind`, or `None` when
/// the kind is not compiled in or takes no devices.
#[must_use]
pub(crate) fn mapping_schema(kind: &str) -> Option<serde_json::Value> {
    match kind {
        #[cfg(feature = "adapter-mqtt")]
        "mqtt" => Some(crate::schema::schema_of::<matter_rs_adapter_mqtt::Mapping>()),
        _ => None,
    }
}

/// The kinds of this build whose adapters take devices, for the reference.
#[must_use]
pub(crate) fn kinds_with_devices() -> Vec<&'static str> {
    KINDS
        .iter()
        .filter(|kind| kind.compiled_in && mapping_schema(kind.kind).is_some())
        .map(|kind| kind.kind)
        .collect()
}

/// Configuration of the test adapter.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestAdapterConfig {
    /// A required setting.
    pub topic: String,
    /// An optional setting with a range.
    #[serde(default)]
    #[schemars(range(max = 10))]
    pub level: u8,
}
