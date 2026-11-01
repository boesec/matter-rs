//! The configuration file as written: one type per section. These types define the file format;
//! the JSON schema and the configuration reference are generated from them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::adapters::AdapterConfig;
use crate::secret::SecretValue;

/// The configuration file of matter-rs.
///
/// One TOML file configures the service. Configuration and state are kept apart: the service
/// never writes to this file, and everything it learns at run time lives in the state directory.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "matter-rs configuration")]
pub struct ConfigFile {
    /// Identity and commissioning data of the Matter node.
    pub node: NodeSection,
    /// How the node is reached on the network.
    #[serde(default)]
    pub network: NetworkSection,
    /// Where the service keeps its state (fabrics, keys, counters).
    #[serde(default)]
    pub state: StateSection,
    /// Logging.
    #[serde(default)]
    pub log: LogSection,
    /// The bridge role. The section's presence enables the role. It needs a build with the Cargo
    /// feature `role-bridge`.
    #[serde(default)]
    pub bridge: Option<BridgeSection>,
    /// Adapter instances, one table `[adapters.<id>]` per instance. `kind` selects the adapter;
    /// the other keys depend on the kind. A kind needs a build with the Cargo feature
    /// `adapter-<kind>`.
    #[serde(default)]
    pub adapters: BTreeMap<String, AdapterConfig>,
}

/// Identity and commissioning data of the Matter node.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeSection {
    /// Vendor id announced by the node. The default is the test vendor id `0xFFF1`.
    #[serde(default = "default_vendor_id")]
    #[schemars(range(min = 1))]
    pub vendor_id: u16,
    /// Product id announced by the node.
    #[serde(default = "default_product_id")]
    #[schemars(range(min = 1))]
    pub product_id: u16,
    /// Vendor name shown by controllers.
    #[serde(default = "default_vendor_name")]
    #[schemars(length(min = 1, max = 32))]
    pub vendor_name: String,
    /// Product name shown by controllers.
    #[serde(default = "default_product_name")]
    #[schemars(length(min = 1, max = 32))]
    pub product_name: String,
    /// User-visible label of the node.
    #[serde(default = "default_node_label")]
    #[schemars(length(max = 32))]
    pub node_label: String,
    /// Serial number announced by the node, if any.
    #[serde(default)]
    #[schemars(length(min = 1, max = 32))]
    pub serial_number: Option<String>,
    /// Setup passcode for commissioning: 1 to 99999998, without the trivial values the Matter
    /// specification forbids. A secret: write it here, or read it with `{ env = "NAME" }` or
    /// `{ file = "PATH" }`.
    pub passcode: SecretValue<u32>,
    /// Discriminator that commissioners use to find the node, 0 to 4095.
    #[schemars(range(max = 4095))]
    pub discriminator: u16,
}

/// How the node is reached on the network.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// Network interface to use, by name. All interfaces when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub interface: Option<String>,
    /// Use IPv4.
    #[serde(default = "default_true")]
    pub ipv4: bool,
    /// Use IPv6.
    #[serde(default = "default_true")]
    pub ipv6: bool,
    /// UDP port of the Matter protocol.
    #[serde(default = "default_port")]
    #[schemars(range(min = 1))]
    pub port: u16,
}

impl Default for NetworkSection {
    fn default() -> Self {
        Self {
            interface: None,
            ipv4: true,
            ipv6: true,
            port: default_port(),
        }
    }
}

/// Where the service keeps its state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StateSection {
    /// State directory. A relative path is relative to the directory of the configuration file.
    /// When absent: `/var/lib/matter-rs` for root and `$XDG_STATE_HOME/matter-rs` (or
    /// `~/.local/state/matter-rs`) otherwise on Linux; `%ProgramData%\matter-rs` when elevated and
    /// `%LOCALAPPDATA%\matter-rs` otherwise on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// Logging.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    /// Lowest level that is logged.
    #[serde(default)]
    pub level: LogLevel,
    /// Format of log lines.
    #[serde(default)]
    pub format: LogFormat,
    /// Log to the systemd journal. Linux only; ignored with a warning elsewhere.
    #[serde(default)]
    pub journald: bool,
}

/// Lowest level that is logged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Errors only.
    Error,
    /// Errors and warnings.
    Warn,
    /// Normal operation.
    #[default]
    Info,
    /// Details for troubleshooting.
    Debug,
    /// Everything.
    Trace,
}

/// Format of log lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable text.
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

/// The bridge role.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BridgeSection {
    /// Run the bridge role. Set to `false` to switch the role off without removing the section.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for BridgeSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

const fn default_true() -> bool {
    true
}

/// The test vendor id of the Matter specification.
pub const DEFAULT_VENDOR_ID: u16 = 0xFFF1;

/// The default product id, from the range the test vendor id may use freely.
pub const DEFAULT_PRODUCT_ID: u16 = 0x8001;

/// The default UDP port of the Matter protocol.
pub const DEFAULT_PORT: u16 = 5540;

const fn default_vendor_id() -> u16 {
    DEFAULT_VENDOR_ID
}

const fn default_product_id() -> u16 {
    DEFAULT_PRODUCT_ID
}

const fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_vendor_name() -> String {
    String::from("matter-rs")
}

fn default_product_name() -> String {
    String::from("matter-rs bridge")
}

fn default_node_label() -> String {
    String::from("matter-rs")
}
