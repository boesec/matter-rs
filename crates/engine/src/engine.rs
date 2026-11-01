//! The engine contract: how the product starts a node and drives it.
//!
//! [`Engine`] starts a node and hands back a [`Running`] handle. Through the handle the product
//! adds and removes endpoints, sets attribute values, emits events and pulls the [`Event`]s the
//! engine produces: commands and writes from Matter clients, commissioning changes, fabric
//! changes and the stop of the node. Commands and writes are answered with
//! [`Running::respond`].
//!
//! # What an engine implementation guarantees
//!
//! Every implementation of this contract, the fake engine included, keeps these rules; product
//! code may rely on them and the engine-agnostic test suite checks them:
//!
//! - **Reads never reach the product.** The engine answers attribute reads and subscription
//!   reports from the values the product set through [`EndpointDescription::initial_values`],
//!   [`Running::set_attribute`], [`Running::set_attributes`] and accepted writes. There is no
//!   read callback and no default the product did not provide.
//! - **Every `set_attribute` is a report.** A changed value bumps the data version of its
//!   cluster and reaches every subscriber; [`Running::set_attributes`] does so once for the whole
//!   batch.
//! - **Events are delivered in order.** [`Running::next_event`] yields events in the order the
//!   engine produced them. Two requests to the same endpoint are delivered in the order the engine
//!   received them. The queue has one consumer: the product runs one task that pulls events and
//!   answers them.
//! - **Every command and write is answered exactly once.** The product calls
//!   [`Running::respond`] with the [`Correlation`] of the event. An engine answers a client on
//!   the product's behalf with [`Status::Failure`] when the product takes longer than the engine's
//!   timeout (the fake engine has none and waits); a response that arrives after that, or twice,
//!   is [`Error::UnknownCorrelation`].
//! - **Paths the engine does not know are answered by the engine.** A command or write to an
//!   endpoint, cluster, attribute or command that is not described reaches the client as
//!   `UnsupportedEndpoint`, `UnsupportedCluster`, `UnsupportedAttribute` or
//!   `UnsupportedCommand` without an event; the product only sees what it described. A write to a
//!   global attribute is answered `UnsupportedWrite`.
//! - **An accepted write is stored.** When the product answers a [`Event::WriteRequested`] with
//!   [`Status::Success`], the engine stores the written value and reports it like a
//!   `set_attribute`; any other status leaves the value unchanged.
//! - **Endpoints are not persisted.** After every [`Engine::start`] the node has only the
//!   endpoints the engine creates itself; the product adds its endpoints again. Fabrics and the
//!   rest of the engine's state are persisted through the [`Storage`] the node was started with.
//! - **Removing an endpoint removes its parts.** [`Running::remove_endpoint`] removes the
//!   endpoint and every endpoint whose `parent` chain leads to it.
//! - **No cluster semantics.** The engine implements no cluster logic of its own on the
//!   endpoints the product adds: it does not toggle an On/Off attribute on an `On` command, it
//!   delivers the command. The utility clusters of the root and aggregator endpoints are the
//!   engine's.
//! - **Capacity is reported, not hidden.** [`Engine::capabilities`] states the limits of the
//!   implementation; an operation that would exceed one fails with [`Error::CapacityExceeded`]
//!   and changes nothing.
//!
//! [`EndpointDescription::initial_values`]: crate::EndpointDescription::initial_values
//! [`Storage`]: crate::Storage

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use crate::description::EndpointDescription;
use crate::error::Error;
use crate::id::{AttributeId, ClusterId, CommandId, EndpointId, EventId, FabricIndex, VendorId};
use crate::identity::{Attestation, NetworkConfig, NodeIdentity, NodeRole};
use crate::secret::Secret;
use crate::status::Status;
use crate::storage::Storage;
use crate::value::Value;

/// A boxed, `Send` future: the shape every asynchronous method of the contract returns, so that
/// the traits can be used as `dyn Engine` and `dyn Running`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A Matter engine that can start a node.
///
/// The binary holds a `Box<dyn Engine>` chosen by feature. An engine starts one node at a time:
/// a second [`start`](Engine::start) while a node runs is [`Error::AlreadyRunning`]; after
/// [`Running::stop`] the engine may be started again.
pub trait Engine: Send + Sync {
    /// Starts a node with the given identity, network settings, attestation credentials and
    /// role, persisting the engine's state in `storage`.
    ///
    /// On success the node is reachable on the network, has its root endpoint (and, for a
    /// bridge, its aggregator endpoint), and the product may add endpoints. Whether a
    /// commissioning window is open depends on the persisted state: a node without fabrics
    /// opens a basic window, a node with fabrics does not. The product reads
    /// [`Running::commissioning_state`] instead of assuming either.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyRunning`] while a node runs, [`Error::InvalidIdentity`] for an identity
    /// or network configuration the engine cannot use, [`Error::Storage`] when the persisted
    /// state cannot be read, [`Error::Engine`] for an implementation-specific failure.
    fn start(
        &self,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        role: NodeRole,
        storage: Arc<dyn Storage>,
    ) -> BoxFuture<'_, Result<Box<dyn Running>, Error>>;

    /// The limits of this engine, so that a configuration can be validated before the node
    /// is started.
    fn capabilities(&self) -> Capabilities;
}

/// A running node.
///
/// The handle is `Send + Sync`; the product shares it (usually as an `Arc<dyn Running>`)
/// between the task that pulls events and the tasks that set values. Every method is a request
/// to the engine and completes when the engine has applied it. After [`stop`](Running::stop),
/// every method fails with [`Error::NotRunning`].
pub trait Running: Send + Sync {
    /// Adds an endpoint with its device types, clusters and initial attribute values.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateEndpoint`] when the id is in use (including the root and aggregator
    /// endpoints), [`Error::UnknownEndpoint`] when `parent` names an endpoint that does not
    /// exist, [`Error::InvalidDescription`] when the description is inconsistent (no device
    /// type, the root endpoint id, a listed attribute without an initial value, a value for an
    /// attribute that is not listed, a global attribute in the attribute list, a cluster listed
    /// twice), [`Error::CapacityExceeded`] when the engine has no room for another endpoint,
    /// [`Error::NotRunning`] after `stop`.
    fn add_endpoint(&self, description: EndpointDescription) -> BoxFuture<'_, Result<(), Error>>;

    /// Removes an endpoint and every endpoint composed under it.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownEndpoint`] when the endpoint was not added by the product,
    /// [`Error::NotRunning`] after `stop`.
    fn remove_endpoint(&self, endpoint: EndpointId) -> BoxFuture<'_, Result<(), Error>>;

    /// Sets the value of one attribute; subscribers are told.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownEndpoint`] when the endpoint does not exist, [`Error::UnknownAttribute`]
    /// when the endpoint does not list the cluster or the attribute, [`Error::NotRunning`]
    /// after `stop`.
    fn set_attribute(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> BoxFuture<'_, Result<(), Error>>;

    /// Sets several attributes at once; subscribers get one report for the batch.
    ///
    /// The batch is applied completely or not at all.
    ///
    /// # Errors
    ///
    /// As [`set_attribute`](Running::set_attribute), for the first value that is rejected.
    fn set_attributes(&self, values: Vec<AttributeValue>) -> BoxFuture<'_, Result<(), Error>>;

    /// Emits an event of a cluster on an endpoint with `payload` as its fields.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownEndpoint`] when the endpoint does not exist, [`Error::UnknownEvent`]
    /// when the endpoint does not list the cluster or the event, [`Error::NotRunning`] after
    /// `stop`.
    fn emit_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
        payload: Value,
    ) -> BoxFuture<'_, Result<(), Error>>;

    /// Marks a bridged endpoint reachable or unreachable.
    ///
    /// A convenience over the `Reachable` attribute and the `ReachableChanged` event of the
    /// Bridged Device Basic Information cluster (see [`bridged_device`]): it sets the attribute
    /// and emits the event in one request. The endpoint must list that cluster with that
    /// attribute.
    ///
    /// # Errors
    ///
    /// As [`set_attribute`](Running::set_attribute).
    fn set_reachable(
        &self,
        endpoint: EndpointId,
        reachable: bool,
    ) -> BoxFuture<'_, Result<(), Error>>;

    /// Answers a [`Event::CommandInvoked`] or [`Event::WriteRequested`].
    ///
    /// # Errors
    ///
    /// [`Error::UnknownCorrelation`] when nothing waits for this correlation (answered already,
    /// timed out, or never issued), [`Error::NotRunning`] after `stop`.
    fn respond(
        &self,
        correlation: Correlation,
        response: Response,
    ) -> BoxFuture<'_, Result<(), Error>>;

    /// The current commissioning state.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] after `stop`.
    fn commissioning_state(&self) -> BoxFuture<'_, Result<CommissioningState, Error>>;

    /// Opens a commissioning window for `timeout`.
    ///
    /// A basic window uses the node's own passcode and discriminator; an enhanced window uses
    /// a temporary passcode the engine generates, which [`pairing_info`](Running::pairing_info)
    /// reports while the window is open. What happens when a window is already open is the
    /// engine's choice (replace or [`Error::Engine`]); a product that needs a defined outcome
    /// closes the window first.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the engine cannot open an enhanced window locally
    /// ([`Capabilities::enhanced_window`]), [`Error::NotRunning`] after `stop`.
    fn open_commissioning_window(
        &self,
        window: Window,
        timeout: Duration,
    ) -> BoxFuture<'_, Result<(), Error>>;

    /// Closes the commissioning window; succeeds when none is open.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] after `stop`.
    fn close_commissioning_window(&self) -> BoxFuture<'_, Result<(), Error>>;

    /// The fabrics the node is part of.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] after `stop`.
    fn fabrics(&self) -> BoxFuture<'_, Result<Vec<FabricInfo>, Error>>;

    /// Removes the node from a fabric. A [`Event::FabricRemoved`] follows.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownFabric`] when no fabric has this index, [`Error::NotRunning`] after
    /// `stop`.
    fn remove_fabric(&self, fabric: FabricIndex) -> BoxFuture<'_, Result<(), Error>>;

    /// The pairing information a commissioner needs: QR code text and manual pairing code.
    ///
    /// Both are secrets (they encode the passcode). Where they are shown is a deliberate
    /// `expose()` at that one place.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] after `stop`.
    fn pairing_info(&self) -> BoxFuture<'_, Result<PairingInfo, Error>>;

    /// Removes every fabric and all persisted engine state; the node keeps running without
    /// fabrics. A [`Event::FabricRemoved`] follows for every fabric that was removed, then a
    /// [`Event::CommissioningChanged`]. Reopening a commissioning window is the product's
    /// decision.
    ///
    /// # Errors
    ///
    /// [`Error::Storage`] when the persisted state cannot be removed, [`Error::NotRunning`]
    /// after `stop`.
    fn factory_reset(&self) -> BoxFuture<'_, Result<(), Error>>;

    /// The limits of the engine; the same as [`Engine::capabilities`].
    fn capabilities(&self) -> Capabilities;

    /// Waits for the next event.
    ///
    /// The last event of a node is [`Event::Stopped`]; after it, and whenever the node is not
    /// running, the method fails with [`Error::NotRunning`] instead of waiting forever.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] once the stop event has been delivered.
    fn next_event(&self) -> BoxFuture<'_, Result<Event, Error>>;

    /// Stops the node: it leaves the network and answers nothing any more. A
    /// [`Event::Stopped`] with [`StopReason::Requested`] is the last event.
    ///
    /// # Errors
    ///
    /// [`Error::NotRunning`] when the node was stopped already.
    fn stop(&self) -> BoxFuture<'_, Result<(), Error>>;
}

/// One attribute value for [`Running::set_attributes`].
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeValue {
    /// The endpoint.
    pub endpoint: EndpointId,
    /// The cluster on the endpoint.
    pub cluster: ClusterId,
    /// The attribute in the cluster.
    pub attribute: AttributeId,
    /// The new value.
    pub value: Value,
}

/// Identifies a pending command or write so that the product's response finds it.
///
/// The engine issues it with the event; the product passes it back to [`Running::respond`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Correlation(u64);

impl Correlation {
    /// Wraps a raw correlation; for engine implementations.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw correlation.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Correlation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// The product's answer to a command or a write.
///
/// For a command, `payload` is the response command's fields, or `None` when the command has
/// no response other than the status. For a write, `payload` is ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The status reported to the client.
    pub status: Status,
    /// The response payload, for commands with a response.
    pub payload: Option<Value>,
}

impl Response {
    /// A successful response without payload.
    #[must_use]
    pub const fn success() -> Self {
        Self {
            status: Status::Success,
            payload: None,
        }
    }

    /// A successful response with a payload.
    #[must_use]
    pub const fn with_payload(payload: Value) -> Self {
        Self {
            status: Status::Success,
            payload: Some(payload),
        }
    }

    /// A response with a status and no payload.
    #[must_use]
    pub const fn status(status: Status) -> Self {
        Self {
            status,
            payload: None,
        }
    }
}

/// What the engine reports to the product.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A client invoked a command on an endpoint the product added. Answer with
    /// [`Running::respond`].
    CommandInvoked {
        /// The correlation for the response.
        correlation: Correlation,
        /// The endpoint.
        endpoint: EndpointId,
        /// The cluster on the endpoint.
        cluster: ClusterId,
        /// The command.
        command: CommandId,
        /// The command fields; [`Value::Null`] when the command has none.
        payload: Value,
        /// The fabric the client belongs to, when the request came over a fabric-scoped session.
        fabric: Option<FabricIndex>,
    },
    /// A client wants to write an attribute the product listed. Answer with
    /// [`Running::respond`]; [`Status::Success`] makes the engine store the value.
    WriteRequested {
        /// The correlation for the response.
        correlation: Correlation,
        /// The endpoint.
        endpoint: EndpointId,
        /// The cluster on the endpoint.
        cluster: ClusterId,
        /// The attribute.
        attribute: AttributeId,
        /// The value the client wants to write.
        value: Value,
        /// The fabric the client belongs to, when the request came over a fabric-scoped session.
        fabric: Option<FabricIndex>,
    },
    /// The commissioning state changed: a window opened or closed, or the node gained or lost
    /// its first or last fabric.
    CommissioningChanged(CommissioningState),
    /// The node joined a fabric.
    FabricAdded(FabricInfo),
    /// The node left a fabric.
    FabricRemoved(FabricIndex),
    /// The node stopped; no further event follows.
    Stopped(StopReason),
}

/// Why a node stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// The product called [`Running::stop`].
    Requested,
    /// The engine failed; the text is the engine's diagnosis.
    Failed(String),
}

/// The kind of commissioning window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Window {
    /// Basic commissioning: the node's own passcode and discriminator.
    Basic,
    /// Enhanced commissioning: a temporary passcode generated for this window.
    Enhanced,
}

/// Whether the node has fabrics and whether a commissioning window is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommissioningState {
    /// Whether the node is part of at least one fabric.
    pub commissioned: bool,
    /// The window that is open, if any.
    pub window: Option<Window>,
}

/// A fabric the node is part of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricInfo {
    /// The index of the fabric on this node.
    pub index: FabricIndex,
    /// The vendor id of the fabric's administrator.
    pub vendor_id: VendorId,
    /// The fabric id, unique to the administrator.
    pub fabric_id: u64,
    /// The node id of this node within the fabric.
    pub node_id: u64,
    /// The label the administrator gave the fabric.
    pub label: String,
}

/// What a commissioner needs to pair with the node.
///
/// `qr_text` and `manual_code` encode the passcode and are secrets; the `Debug` output of this
/// struct shows only the discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingInfo {
    /// The text of the QR code (`MT:...`).
    pub qr_text: Secret<String>,
    /// The 11- or 21-digit manual pairing code.
    pub manual_code: Secret<String>,
    /// The discriminator the window uses.
    pub discriminator: u16,
}

/// The limits of an engine. `None` means no limit beyond memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// How many endpoints the product may add.
    pub max_endpoints: Option<usize>,
    /// How many fabrics the node can join.
    pub max_fabrics: Option<usize>,
    /// How many subscriptions the node can serve at once.
    pub max_subscriptions: Option<usize>,
    /// Whether [`Running::open_commissioning_window`] supports [`Window::Enhanced`].
    pub enhanced_window: bool,
}

impl Capabilities {
    /// No limits, enhanced window supported.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_endpoints: None,
            max_fabrics: None,
            max_subscriptions: None,
            enhanced_window: true,
        }
    }
}

/// The ids of the Bridged Device Basic Information cluster that
/// [`Running::set_reachable`] uses.
///
/// They are the only cluster ids the facade knows; the convenience exists because every
/// bridged endpoint carries this cluster and the pair "set `Reachable`, emit
/// `ReachableChanged`" is the one cluster rule a bridge needs from every engine.
pub mod bridged_device {
    use crate::id::{AttributeId, ClusterId, EventId};

    /// The Bridged Device Basic Information cluster.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0039);
    /// The `Reachable` attribute.
    pub const REACHABLE: AttributeId = AttributeId::new(0x0011);
    /// The `ReachableChanged` event; its payload is a struct with the field
    /// [`REACHABLE_NEW_VALUE_TAG`] holding the new `Reachable` value.
    pub const REACHABLE_CHANGED: EventId = EventId::new(0x0003);
    /// The tag of the `ReachableNewValue` field of `ReachableChanged`.
    pub const REACHABLE_NEW_VALUE_TAG: u64 = 0;
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;

    use super::{Capabilities, Correlation, PairingInfo, Response};
    use crate::secret::{REDACTED, Secret};
    use crate::status::Status;
    use crate::value::Value;

    #[test]
    fn response_constructors() {
        assert_eq!(
            Response::success(),
            Response {
                status: Status::Success,
                payload: None
            }
        );
        assert_eq!(
            Response::with_payload(Value::U8(1)),
            Response {
                status: Status::Success,
                payload: Some(Value::U8(1))
            }
        );
        assert_eq!(
            Response::status(Status::Busy),
            Response {
                status: Status::Busy,
                payload: None
            }
        );
    }

    #[test]
    fn correlation_round_trips_and_displays() {
        let correlation = Correlation::new(7);
        assert_eq!(correlation.raw(), 7);
        assert_eq!(format!("{correlation}"), "#7");
    }

    #[test]
    fn capabilities_unbounded_has_no_limits() {
        let capabilities = Capabilities::unbounded();
        assert_eq!(capabilities.max_endpoints, None);
        assert_eq!(capabilities.max_fabrics, None);
        assert_eq!(capabilities.max_subscriptions, None);
        assert!(capabilities.enhanced_window);
    }

    #[test]
    fn pairing_info_debug_redacts_codes() {
        let info = PairingInfo {
            qr_text: Secret::new(String::from("MT:SECRETQR")),
            manual_code: Secret::new(String::from("34970112332")),
            discriminator: 3840,
        };
        let output = format!("{info:?}");
        assert!(output.contains(REDACTED));
        assert!(output.contains("3840"));
        assert!(!output.contains("SECRETQR"));
        assert!(!output.contains("34970112332"));
    }
}
