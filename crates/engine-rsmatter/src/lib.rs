//! Engine implementation of matter-rs on rs-matter (layer 1).
//!
//! This is the only crate of the workspace that may depend on rs-matter; a workspace test
//! enforces that. It implements the engine facade over rs-matter and hides everything about it:
//! no rs-matter type appears in its public API, and rs-matter's errors are translated into the
//! facade's errors or into this crate's own [`Error`].
//!
//! The crate needs the standard library (threads, sockets, files). A microcontroller engine is a
//! separate engine implementation crate, not a feature of this one.
//!
//! # How it works
//!
//! [`RsMatterEngine::start`] spawns one OS thread per node. That thread owns everything
//! rs-matter needs (the `Matter` object, the interaction model, the responders, the sockets, the
//! endpoint table) and drives rs-matter's futures with a single-threaded executor, exactly as
//! rs-matter's own examples do. rs-matter's futures are not `Send`, so nothing of them ever
//! leaves the thread. The product reaches the node through the [`Running`] handle:
//!
//! - every method queues a request for the engine thread and returns a `Send` future that
//!   completes when the thread has applied it;
//! - [`Running::next_event`] pulls from an event queue the engine thread fills;
//! - [`Running::respond`] delivers the product's answer to a command or a write into a shared
//!   table the engine thread waits on.
//!
//! The engine thread runs a service loop next to rs-matter's jobs (transport, mDNS, exchange
//! responders, interaction model) that drains the requests and, four times a second, compares
//! the commissioning window and the fabric table with its last snapshot to report changes as
//! [`Event::CommissioningChanged`], [`Event::FabricAdded`] and [`Event::FabricRemoved`].
//! [`Running::stop`] emits the shut-down event, keeps the interaction model running for half a
//! second so that subscribers receive it and deferred persistence can flush, then ends the
//! thread; [`Event::Stopped`] is the last event.
//!
//! # The rs-matter pin and how to upgrade it
//!
//! The workspace manifest pins `rs-matter` and `rs-matter-macros` to one git revision of
//! <https://github.com/project-chip/rs-matter> (0.4.1 plus the unreleased fixes up to that
//! commit, among them the Windows UDP fix). The default features `os`, `rustcrypto` and `log`
//! are enabled and nothing else: in particular not `send-sync`, which upstream marks as work in
//! progress, and not `groups` or `persistent-subscriptions`. `cargo deny` allows that git
//! source.
//!
//! To upgrade:
//!
//! 1. Read upstream's `CHANGELOG.md` from the pinned revision to the new one and note every
//!    line marked breaking; the APIs this crate uses are listed under "What this crate uses"
//!    below.
//! 2. Change the `rev` of both crates in the workspace manifest and run `cargo update -p
//!    rs-matter -p rs-matter-macros` (or delete their lockfile entries and let cargo resolve).
//! 3. Run the quality gate (`scripts/check.ps1` or `scripts/check.sh`): the unit tests of this
//!    crate pin the TLV mapping, the metadata shapes and the pairing codes; the contract tests
//!    start real nodes.
//! 4. Run the local interop scenario (`tests/interop-local/`) against the matter.js shell on
//!    Windows and Linux.
//! 5. Switch to a crates.io release when one appears that contains what the pin was chosen
//!    for.
//!
//! What this crate uses of rs-matter: `Matter` (construction, startup, factory reset, the
//! commissioning window, `comm_window_state`, `has_fabrics`, `mdns_services`, `with_state` for
//! the fabric table), `InteractionModel` (construction, startup, factory reset, `run`,
//! `open_basic_comm_window`, `close_comm_window`, `bump_configuration_version`,
//! `emit_shut_down`, the `AttrChangeNotifier`, `EventEmitter` and `HandlerContext` traits),
//! `DefaultResponder`, the `AsyncHandler`, `Metadata`, `Node`, `Endpoint` and `Cluster` types
//! with the generated `FULL_CLUSTER` constants, `EthSysHandlerBuilder` and `root_endpoint!`,
//! `KvBlobStore`, `FabricPersist`, the TLV reader and writer, `BasicCommData`'s pairing code
//! and `QrPayload`, `DeviceAttestation` and the test credentials, `BuiltinMdns` and the
//! async-io UDP adapter.
//!
//! # Node bring-up
//!
//! [`NodeIdentity`] becomes rs-matter's basic information (vendor and product ids and names,
//! software version, serial number, unique id; the product name is also the mDNS device name)
//! and its commissioning data. The passcode is exposed at exactly one place, when the
//! commissioning data is built, and appears in no log line. The configured node label is put
//! into Basic Information at start unless an administrator wrote one that rs-matter persisted.
//!
//! [`Attestation::Test`] uses rs-matter's test credentials (vendor id `0xFFF1`, the test PAA,
//! which ecosystems report as untrusted but accept for development). [`Attestation::Files`]
//! takes the certification declaration, the PAI and the DAC as DER and the DAC's P-256 private
//! key as its raw 32 bytes or as a DER `ECPrivateKey` (also inside PKCS#8).
//!
//! [`NetworkConfig`] decides the sockets: an IPv6 socket with `IPV6_V6ONLY` off when both
//! families are enabled (Windows defaults it to on, which would drop every IPv4 datagram), an
//! IPv4 socket for IPv4 only, an IPv6-only socket for IPv6 only, bound to every address on the
//! configured port. The interface, named or chosen (first non-loopback interface with IPv4 and
//! a link-local IPv6 address, then first with IPv4, then first with any address), decides what
//! the built-in mDNS responder announces on port 5353: its IPv4 address and every IPv6 address,
//! or IPv4 only when the interface has no IPv6, without warnings. The responder runs on every
//! platform; Avahi or Bonjour are not used.
//!
//! # Storage
//!
//! rs-matter's blobs (fabrics, basic-information settings, counters, the events epoch) are
//! stored through the facade's [`Storage`] under `engine/rsmatter/<key>` with rs-matter's
//! numeric key in decimal, plus an index of the stored keys under `engine/rsmatter/index` that
//! the factory reset uses to remove them all. They therefore live in the same directory as the
//! product's keys, with the permissions the directory storage sets. The scratch buffer size is
//! rs-matter's default (4096 bytes per blob).
//!
//! # Endpoints and values
//!
//! The node has the root endpoint (rs-matter's system clusters), the aggregator and the
//! product's endpoints. Every endpoint description becomes rs-matter metadata: the device
//! types, and one cluster per described cluster plus the Descriptor cluster, which the engine
//! serves itself (`DeviceTypeList`, `ServerList`, an empty `ClientList`, `PartsList` with the
//! direct parts of a composed endpoint or, on the aggregator, every product endpoint). rs-matter
//! borrows the metadata for the node's lifetime, so it is leaked once per distinct shape (the
//! ids of a cluster, the cluster list of an endpoint, a device type list) and reused; the leak
//! is bounded by the number of shapes the product uses, not by the number of endpoints added
//! over time.
//!
//! For clusters rs-matter ships generated metadata for (the lighting, sensor, switch, label,
//! power, bridged-device and several other clusters), the described attributes, commands and
//! events carry rs-matter's access rules and qualities: a client's write to a read-only
//! attribute is refused by rs-matter before it reaches the product, and a command's response
//! command id is known. For other clusters every attribute is readable and writable by an
//! operator, a list-valued initial value marks an array attribute, commands need operate
//! privilege, and the product decides on every write.
//!
//! One handler serves every endpoint but the root. Reads are answered from the value map (the
//! global attributes from the cluster metadata); array attributes follow rs-matter's chunked
//! read protocol. A write becomes [`Event::WriteRequested`], a command [`Event::CommandInvoked`];
//! the handler waits for the product's [`Running::respond`] for at most the response timeout
//! ([`DEFAULT_RESPONSE_TIMEOUT`], configurable), then answers `Failure` to the client. A
//! [`Status::Success`] on a write stores the value and reports it to subscribers; every other
//! status leaves it unchanged. A command response with a payload is encoded as the response
//! command when rs-matter's metadata knows its id; otherwise the status alone is sent and a
//! warning logged. Statuses without an rs-matter error code (`UnsupportedWrite`,
//! `UnsupportedRead`, `Timeout`, `PathsExhausted` and a few more) reach the client as
//! `Failure`.
//!
//! [`Running::set_attribute`] and [`Running::set_attributes`] update the value map and tell the
//! interaction model, which bumps the cluster's data version and reports to subscribers;
//! [`Running::emit_event`] goes to rs-matter's event queue with `Info` priority;
//! [`Running::set_reachable`] does both for the Bridged Device Basic Information cluster.
//! Adding or removing an endpoint tells the interaction model that the endpoint, its ancestors
//! up to the aggregator and the root changed, and bumps `BasicInformation::ConfigurationVersion`,
//! which rs-matter persists.
//!
//! # Dynamic endpoints at this revision
//!
//! rs-matter's node metadata may change at run time: path expansion re-reads it at every step
//! and recovers by id when it changed in between, and `PartsList` is computed from it at read
//! time. What this crate has shown end to end:
//!
//! - **Adding endpoints before commissioning** and serving them afterwards: the local interop
//!   scenario commissions the example node with the matter.js shell, reads the aggregator's
//!   `PartsList`, reads and toggles the light and writes its label.
//! - **Restart with persisted state**: the fabric survives in the storage, the controller
//!   reconnects without re-commissioning, the product adds its endpoints again and restores its
//!   own values.
//! - **Adding and removing while running** (in process): the metadata, the Descriptor lists and
//!   the data versions follow, the interaction model is notified for the changed endpoints and
//!   the configuration version is bumped.
//!
//! What is not shown end to end and stays open for the dynamic-endpoint work on top of this
//! crate: whether a controller with a wildcard subscription receives reports for an endpoint
//! added while it is subscribed, and whether a concrete-path subscription to a removed endpoint
//! is answered `UnsupportedEndpoint` by the next report. Both are what rs-matter's
//! `notify_endpoint_changed` and its path expansion are designed for, but no controller has
//! verified them against this crate yet.
//!
//! # Limits of this implementation
//!
//! - An enhanced commissioning window cannot be opened locally; rs-matter offers no API for a
//!   locally generated verifier. [`Capabilities::enhanced_window`] is `false` and
//!   [`Running::open_commissioning_window`] with [`Window::Enhanced`] is
//!   [`matter_rs_engine::Error::Unsupported`]. A window opened by an administrator over the
//!   network is reported with its real kind.
//! - A basic window's timeout is clamped to rs-matter's range (3 to 15 minutes).
//! - Device type ids must fit 16 bits (rs-matter's `DeviceType`); the Descriptor cluster must
//!   not be described, the engine serves it.
//! - Writes of single list elements (chunked list writes) are refused with `InvalidAction`;
//!   whole-list writes reach the product.
//! - [`Running::remove_fabric`] removes the fabric, its sessions and its persisted record the
//!   way rs-matter's `RemoveFabric` command does, but cannot withdraw the fabric's mDNS record
//!   until the next mDNS change (the notification is not public in rs-matter).
//! - The capabilities are compile-time: 5 fabrics, 15 subscriptions, and the endpoint capacity
//!   of this engine ([`DEFAULT_ENDPOINT_CAPACITY`], configurable).
//!
//! # Secrets
//!
//! No log line of this crate contains the passcode, the pairing code or key material. rs-matter's
//! log statements were audited at the pinned revision: the only ones that print the pairing
//! code or the QR text are `Matter::print_standard_qr_text`, `print_standard_qr_code` and
//! `print_standard_nfc_ndef`, which this crate never calls; the commissioning data's `Debug`
//! output is rs-matter's redacting `CryptoSensitive` wrapper. [`Running::pairing_info`] and
//! [`pairing_info_for`] compute the codes with rs-matter's own payload and manual-code
//! functions and return them as [`Secret`]s; the `pairing-info` command of the service binary
//! is the one place that shows them.
//!
//! [`Running`]: matter_rs_engine::Running
//! [`Running::next_event`]: matter_rs_engine::Running::next_event
//! [`Running::respond`]: matter_rs_engine::Running::respond
//! [`Running::stop`]: matter_rs_engine::Running::stop
//! [`Running::set_attribute`]: matter_rs_engine::Running::set_attribute
//! [`Running::set_attributes`]: matter_rs_engine::Running::set_attributes
//! [`Running::emit_event`]: matter_rs_engine::Running::emit_event
//! [`Running::set_reachable`]: matter_rs_engine::Running::set_reachable
//! [`Running::remove_fabric`]: matter_rs_engine::Running::remove_fabric
//! [`Running::open_commissioning_window`]: matter_rs_engine::Running::open_commissioning_window
//! [`Running::pairing_info`]: matter_rs_engine::Running::pairing_info
//! [`Event::CommissioningChanged`]: matter_rs_engine::Event::CommissioningChanged
//! [`Event::FabricAdded`]: matter_rs_engine::Event::FabricAdded
//! [`Event::FabricRemoved`]: matter_rs_engine::Event::FabricRemoved
//! [`Event::Stopped`]: matter_rs_engine::Event::Stopped
//! [`Event::WriteRequested`]: matter_rs_engine::Event::WriteRequested
//! [`Event::CommandInvoked`]: matter_rs_engine::Event::CommandInvoked
//! [`NodeIdentity`]: matter_rs_engine::NodeIdentity
//! [`NetworkConfig`]: matter_rs_engine::NetworkConfig
//! [`Attestation::Test`]: matter_rs_engine::Attestation::Test
//! [`Attestation::Files`]: matter_rs_engine::Attestation::Files
//! [`Storage`]: matter_rs_engine::Storage
//! [`Status::Success`]: matter_rs_engine::Status::Success
//! [`Capabilities::enhanced_window`]: matter_rs_engine::Capabilities::enhanced_window
//! [`Window::Enhanced`]: matter_rs_engine::Window::Enhanced
//! [`Secret`]: matter_rs_engine::Secret

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

// rs-matter's timers run on embassy-time; under std its driver and the timer queue are separate
// crates that must be linked into every binary. Naming them here keeps them in the link even
// though no code of this crate calls them.
use embassy_time as _;
use embassy_time_queue_utils as _;

mod attestation;
mod channel;
#[cfg(test)]
mod contract_on_rsmatter;
mod engine;
mod error;
mod handler;
mod network;
mod node;
mod pairing;
mod store;
mod table;
mod tlv;

pub use engine::{DEFAULT_ENDPOINT_CAPACITY, DEFAULT_RESPONSE_TIMEOUT, RsMatterEngine};
pub use error::{Error, RsMatterError};
pub use pairing::pairing_info_for;
