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
//! <https://github.com/project-chip/rs-matter>: `af3e49ac71c778f62a466c272535d2bbf4d301fb` of
//! 2026-11-26, which is 0.4.1 plus the unreleased changes up to that commit. Since the split of
//! rs-matter into micro crates (`rs-matter-tlv`, `rs-matter-dm-core`, `rs-matter-im`,
//! `rs-matter-node`, `rs-matter-stack` and the others), the `rs-matter` crate is a facade that
//! re-exports them under the paths this crate has always used; the micro crates are not named
//! in any manifest of this workspace and follow the `rs-matter` entry of the lockfile, so they
//! can never be at another revision. The default features `os`, `rustcrypto` and `log` are
//! enabled and nothing else: in particular not `send-sync`, which upstream marks as work in
//! progress, and not `groups`, `persistent-subscriptions` or a `large-buffers-*` feature.
//! `cargo deny` allows that git source.
//!
//! To upgrade:
//!
//! 1. Read upstream's `CHANGELOG.md` from the pinned revision to the new one and note every
//!    line marked breaking; the APIs this crate uses are listed under "What this crate uses"
//!    below. Read the description of every pull request that moves code as well.
//! 2. Change the `rev` of both crates in the workspace manifest and run `cargo update -p
//!    rs-matter -p rs-matter-macros`: the lockfile entries of the micro crates follow (or
//!    delete every `rs-matter*` lockfile entry and let cargo resolve).
//! 3. Run the quality gate (`scripts/check.ps1` or `scripts/check.sh`): the unit tests of this
//!    crate pin the TLV mapping, the metadata shapes and the pairing codes; the contract tests
//!    start real nodes. `cargo deny check` reports a new licence or advisory, which is a
//!    decision of the project owner, never a silent exception.
//! 4. Run the local interop scenario (`tests/interop-local/`) against the matter.js shell on
//!    Windows and Linux, and the interop harness with the engine example as the device under
//!    test on CI.
//! 5. Audit rs-matter's log statements at the new revision for the passcode, the pairing code
//!    and the QR text (see "Secrets" below) and extend the list of upgrades below.
//! 6. Switch to a crates.io release when one appears that contains what the pin was chosen
//!    for.
//!
//! What this crate uses of rs-matter: `Matter` (construction with a `DevCommData`, startup,
//! factory reset, `remove_fabric`, the commissioning window, `comm_window_state`,
//! `has_fabrics`, `mdns_services`, `with_state` for the fabric table), `InteractionModel`
//! (construction over a `ByteBuffers` pool, startup, factory reset, `run`,
//! `open_basic_comm_window`, `close_comm_window`, `bump_configuration_version`,
//! `emit_shut_down`, `notify_endpoint_changed`, `notify_attr_changed`, the
//! `AttrChangeNotifier`, `EventEmitter` and `HandlerContext` traits), `MatterBuffers`,
//! `DefaultResponder`, the `AsyncHandler`,
//! `Metadata`, `Node`, `Endpoint` and `Cluster` types with the generated `FULL_CLUSTER`
//! constants, `EthSysHandlerBuilder` and `root_endpoint!`, `KvBlobStore`, the TLV reader and
//! writer with their `TlvError`, `BasicCommData`'s pairing code and `QrPayload`,
//! `DeviceAttestation` and the test credentials, `BuiltinMdns` and the async-io UDP adapter;
//! the contract tests also use `FabricPersist`, the certificate generators of `onboard` and
//! the test-only crypto to mint a fabric into the storage.
//!
//! ## Upgrades so far
//!
//! **`14baec0` (2026-10-31) to `af3e49a` (2026-11-26).** 43 upstream commits. The breaking
//! changes this crate met, and what was done about them:
//!
//! - The crate split (upstream #634, #636): every path this crate uses still resolves through
//!   the `rs-matter` facade; nothing was renamed here. `EVENT_DATA_TAG` moved from
//!   `im::events` to `dm`, `MatterBuffers` from `transport::exchange` to `transport`.
//! - The system cluster handlers take the `Matter` when built (#634):
//!   `EthSysHandlerBuilder::build(&matter, rand)`.
//! - `QrPayload::new_from_basic_info` is gone (#634); `QrPayload::new` takes the vendor and
//!   product ids and the serial number directly, which is what the pairing code computation
//!   does now.
//! - `Matter::new` takes a `DevCommData` (#659): the passcode is wrapped as
//!   `DevCommData::Passcode` at the one place the commissioning data is built. The verifier
//!   variant, for devices whose passcode stays at the factory, is not used; the facade has no
//!   way to express it.
//! - The Interaction Model takes a `ByteBuffers` pool instead of `Buffers<IMBuffer>` (#678);
//!   `MatterBuffers` implements it, with 9 UDP-sized buffers by default and no large ones
//!   because no `large-buffers-*` feature is enabled.
//! - The TLV crate reports its own `TlvError` (#679): the `ToTLV` implementation and the TLV
//!   write helpers of this crate return it, and the places that answer rs-matter's global
//!   error convert it with `?` or `map_err`.
//! - `Matter::remove_fabric` is public (#681) and replaces the hand-rolled removal this crate
//!   had: it removes the fabric's sessions and persisted record, withdraws the operational mDNS
//!   record, drops a trusted root staged in the fail-safe and emits the `Leave` event, so the
//!   former limit that the mDNS record stayed until the next change is gone.
//!
//! Behaviour that changed underneath without a code change here: an empty `unique_id` makes
//! rs-matter serve a random, persisted one (#669); the default `MaxPathsPerInvoke` is 1 again
//! (#690); the max interval granted to a subscription is capped at 120 s by default (#642);
//! opening a commissioning window fails with `Busy` while the fail-safe is armed (#690); on
//! Windows the UDP adapter ignores the connection reset a receive reports after a send to a
//! closed port (#635); session state is one enum with a `Defunct` state (#648); an ICD in LIT
//! mode advertises no session idle interval (#644, not relevant to this always-on node). The
//! `paste` advisory ignore and the MPL-2.0 exceptions of `deny.toml` are still needed at this
//! revision: `pinned-init` still depends on `paste`, and the code generator still on
//! `nom-supreme`.
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
//! Adding or removing an endpoint tells the interaction model that the added or removed
//! endpoints changed as a whole (every cluster's data version is bumped, every attribute is
//! reported to subscribers) and that the Descriptor `PartsList` of the parents up to the
//! aggregator, of the aggregator and of the root changed (that one attribute is reported and
//! only the Descriptor's data version moves), then bumps `BasicInformation::ConfigurationVersion`,
//! which rs-matter persists and reports. This is what the reference SDK does when an endpoint is
//! enabled or disabled; an endpoint-wide notification of the root would re-report every system
//! cluster to every subscriber on every bridged add or removal.
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
//! - **Adding and removing while running**, with chip-tool as the client (the engine example
//!   as the device under test of the interop harness, scenarios `test_engine_*`): an endpoint
//!   added while commissioned is read, listed in the aggregator's and the root's `PartsList`,
//!   and `ConfigurationVersion` has increased (`test_engine_runtime_added_endpoint_is_served`);
//!   a controller with a wildcard subscription receives a report with the attributes of an
//!   endpoint added after it subscribed (`test_engine_wildcard_subscription_sees_added_endpoint`);
//!   after a removal a read answers `UnsupportedEndpoint`, `PartsList` no longer lists the
//!   endpoint, and a concrete-path subscription to it receives that status in its next report
//!   and stays alive (`test_engine_removed_endpoint_is_unsupported`). The connectedhomeip
//!   scripts `TC_BRBINFO_2_1` and `TC_DeviceBasicComposition` (with `TC_DESC_2_1`, `TC_DESC_2_2`
//!   and `TC_IDM_10_1`) pass against the node (`test_engine_python_conformance`).
//! - **Notification of the ancestors** is attribute-level (see "Endpoints and values"): shown
//!   in process through a real interaction model over this crate's data model, where an add
//!   bumps on the root exactly the Descriptor's and Basic Information's data versions
//!   (`interaction_model_bumps_only_the_descriptor_on_ancestors_and_root`).
//! - **Shape stability**: an endpoint id keeps its shape while present (a second description
//!   under the same id is a duplicate), and may come back with any shape after a removal
//!   (`re_add_same_id_while_present_is_duplicate`,
//!   `re_add_with_different_shape_after_remove_is_fine`). rs-matter's path expansion recovers
//!   by endpoint id, the re-added endpoint gets fresh data versions and the configuration
//!   version is bumped, so an expansion that straddles the change skips or repeats an item at
//!   worst.
//!
//! What stays open, and lies in rs-matter rather than in this crate:
//!
//! - A concrete-path subscription to a removed endpoint receives `UnsupportedEndpoint` in
//!   **every** report, keep-alives included, because the path expander returns the status of a
//!   non-wildcard path before the report filter is consulted; the reference SDK reports a dirty
//!   path that no longer resolves once. A deviation in frequency, not in content.
//! - A subscription whose report fails to reach its subscriber is retried with a back-off
//!   until its maximum interval has elapsed (see the findings below); the reference SDK drops
//!   it at the first failure.
//! - rs-matter has no ready-made `Metadata` for a lock-guarded dynamic node, its bridge example
//!   is not conformant (`UniqueID`), and its test suite has no bridge device under test; none of
//!   this is needed by this crate, which serves its own metadata and Descriptor.
//!
//! # Findings from the client-driven scenarios
//!
//! Four observations from the first harness runs, each with its verdict:
//!
//! - **The PASE session is gone after commissioning.** rs-matter's behaviour, as the
//!   specification requires, not this crate's: the General Commissioning handler removes the
//!   node's PASE sessions when `CommissioningComplete` succeeds, marking the one the command
//!   came over as expired so that the response still goes out
//!   (`rs-matter-dm/src/clusters/gen_comm.rs`, `handle_commissioning_complete`, at the pin;
//!   its comment quotes the rule). The reference SDK does the same
//!   (`src/app/server/CommissioningWindowManager.cpp`, `ExpireAllPASESessions` on the
//!   commissioning-complete event). The Python test framework keeps using its PASE session when
//!   it commissioned the node in the same process; against this node the next interaction fails
//!   with "not connected", against the reference SDK a subscription over PASE is refused with
//!   `UnsupportedAccess`, which the framework's composition test tolerates with a comment
//!   (`src/python_testing/TC_DeviceBasicComposition.py`). The harness therefore runs the
//!   composition script over CASE. Nothing to fix here.
//! - **Subscription reports are retried after a subscriber vanished.** rs-matter's design, not
//!   this crate's, and not changeable from here: when a report fails (the MRP gives up after
//!   its retransmissions, "Too many retransmissions. Giving up", `rs-matter-transport/src/mrp.rs`),
//!   a non-ICD node keeps the subscription and retries with an exponential back-off of 2, 4,
//!   8 … seconds capped at the maximum interval, without advancing its watermarks, until the
//!   maximum interval has elapsed since the last successful report, when the subscription
//!   expires (`rs-matter-im/src/lib.rs`, `process_subscriptions`; `rs-matter-im/src/subscriptions.rs`,
//!   `ReportContext::set_keep_retry`, `Subscription::is_expired`, `retry_backoff_secs`). Only an
//!   ICD drops at the first failure (`set_drop_failed`). The noise is therefore bounded by the
//!   subscription's maximum interval (120 seconds at most with rs-matter's default cap): about
//!   six attempts, each with the MRP's retransmissions. The reference SDK closes the read handler
//!   at the first unacknowledged report (`src/app/ReadHandler.cpp`, `OnResponseTimeout`). The
//!   interaction model offers no choice to the application; a switch to drop on the first
//!   failure would be an upstream change.
//! - **General Commissioning attribute `0x000C` answers `UnsupportedAttribute`.** Correct by the
//!   data model: in revision 2 of the cluster (data model 1.6,
//!   `data_model/1.6/clusters/GeneralCommissioningCluster.xml`) attribute `0x000C` is
//!   `IsCommissioningWithoutPower`, provisional and optional; it is not one of the terms and
//!   conditions attributes (`0x0005` to `0x0009`), which are mandatory only with the `TC`
//!   feature that this node does not advertise. rs-matter's handler serves the required
//!   attributes only (`rs-matter-dm/src/clusters/gen_comm.rs`, `with!(required)`), so a read
//!   of an optional, unsupported attribute is answered `UnsupportedAttribute` as the
//!   Interaction Model prescribes. Nothing to do.
//! - **The Python framework removes other administrators' fabrics.** Framework behaviour: its
//!   cleanup removes every fabric that was not in the baseline it captured at setup
//!   (`remove_extra_fabrics`, on by default, in the framework's `matter_testing.py`), which
//!   after its own commissioning is every other fabric, chip-tool's included. The harness runs
//!   the framework scripts last on this node for that reason; nothing for the engine.
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
//! - The capabilities are compile-time: 5 fabrics, 15 subscriptions, and the endpoint capacity
//!   of this engine ([`DEFAULT_ENDPOINT_CAPACITY`], configurable).
//!
//! # Secrets
//!
//! No log line of this crate contains the passcode, the pairing code or key material. rs-matter's
//! log statements are audited at every pinned revision (last at `af3e49a`): the only ones that
//! print the pairing code or the QR text are `Matter::print_standard_qr_text`,
//! `print_standard_qr_code` and `print_standard_nfc_ndef`, which this crate never calls; the
//! commissioning data's `Debug` output is rs-matter's redacting `CryptoSensitive` wrapper. [`Running::pairing_info`] and
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
