//! The engine-agnostic test suite: what every implementation of the engine contract must pass.
//!
//! The suite is written against the facade only ([`Engine`], [`Running`], [`Event`], the
//! descriptions and the storage contract) and knows no engine. The fake engine runs it, the
//! rs-matter engine runs it, and an engine written later runs it as well: an implementation is
//! accepted when every case is green. Each case pins one of the rules listed in the
//! [`engine`](crate::engine) module documentation and says which in its own documentation.
//!
//! The suite covers what can be shown in process, without a Matter client: the node lifecycle,
//! the endpoint rules, attribute and event paths, commissioning windows, factory reset, the
//! storage namespace and the handling of secrets. Everything a client sees (reads, subscription
//! reports, the status of a command or a write on the wire) belongs to the client-driven
//! scenarios of the interop harness and is not here.
//!
//! # Running the suite against an engine
//!
//! 1. Implement [`EngineUnderTest`]: [`EngineUnderTest::engine`] returns a fresh engine; the
//!    other methods have defaults (the development identity with passcode `20202021` and
//!    discriminator `3840`, an ephemeral port on every interface, test attestation, a fresh
//!    in-memory storage, aggregator endpoint 1, a ten-second timeout for events) that an
//!    implementation overrides where it must, for example to name the network interface.
//! 2. Expand [`contract_tests!`] with an expression that builds the implementation. The macro
//!    expands to one `#[test]` per case, named like the case, so that a failing run names the
//!    rule that was broken.
//!
//! ```ignore
//! use matter_rs_engine::contract::EngineUnderTest;
//! use matter_rs_engine::Engine;
//!
//! struct MyEngineUnderTest;
//!
//! impl EngineUnderTest for MyEngineUnderTest {
//!     fn engine(&self) -> Box<dyn Engine> {
//!         Box::new(my_engine::MyEngine::new())
//!     }
//! }
//!
//! matter_rs_engine::contract_tests!(MyEngineUnderTest);
//! ```
//!
//! Every case is also a `pub fn` taking `&dyn EngineUnderTest`, and [`CASES`] lists them all
//! with their names, so that a harness can run them one by one (for example under
//! `catch_unwind`, to show that a broken engine fails).
//!
//! Every case starts its own node and stops it at the end; an engine that limits itself to one
//! node at a time is fine because the cases of one process may run in parallel only when each
//! case has its own engine, which [`EngineUnderTest::engine`] guarantees.

mod cases;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::task::{Wake, Waker};
use std::thread::{self, Thread};
use std::time::Instant;

pub use cases::{
    basic_window_open_close_are_events, capacity_matches_capabilities,
    commissioning_state_readable, duplicate_endpoint_rejected, endpoint_can_be_re_added,
    engine_keys_are_namespaced, errors_never_contain_the_passcode,
    factory_reset_reopens_window_and_clears_engine_keys, global_attribute_in_list_rejected,
    missing_initial_value_rejected, pairing_info_redacted_and_stable,
    remove_cascades_to_composed_parts, respond_unknown_correlation_rejected,
    second_start_is_already_running, set_attribute_on_unknown_path_rejected,
    set_attributes_is_atomic, set_reachable_on_bridged_endpoint, start_stop_restart,
    stop_ends_event_stream, unknown_parent_rejected,
};

use crate::engine::{Engine, Event, Running};
use crate::error::Error;
use crate::id::{EndpointId, ProductId, VendorId};
use crate::identity::{Attestation, NetworkConfig, NodeIdentity, NodeRole};
use crate::secret::Secret;
use crate::storage::{MemoryStorage, Storage, StorageError};
use core::fmt;

/// The passcode of the suite's default identity: the development passcode every Matter SDK
/// uses in its examples.
pub const DEFAULT_PASSCODE: u32 = 20_202_021;

/// The discriminator of the suite's default identity.
pub const DEFAULT_DISCRIMINATOR: u16 = 3840;

/// An engine implementation as the suite sees it.
///
/// [`engine`](EngineUnderTest::engine) is the only required method; it must return a fresh
/// engine every time, so that cases can run in parallel and a restart case can start the same
/// engine twice. The other methods supply what [`Engine::start`] needs and have defaults that
/// suit an engine without network requirements.
pub trait EngineUnderTest: Sync {
    /// A fresh engine that has never started a node.
    fn engine(&self) -> Box<dyn Engine>;

    /// The identity every case starts its node with. The default is the development identity;
    /// an override keeps the passcode and the discriminator, which some cases assert on.
    fn identity(&self) -> NodeIdentity {
        NodeIdentity {
            vendor_id: VendorId::new(0xFFF1),
            product_id: ProductId::new(0x8001),
            vendor_name: String::from("matter-rs"),
            product_name: String::from("Contract test node"),
            node_label: String::from("Contract test node"),
            serial_number: None,
            software_version: 1,
            software_version_string: String::from("0.0.0"),
            unique_id: String::from("contract-test"),
            passcode: Secret::new(DEFAULT_PASSCODE),
            discriminator: DEFAULT_DISCRIMINATOR,
        }
    }

    /// The network configuration: by default an ephemeral port on every interface, both
    /// families, so that many nodes can run at once.
    fn network(&self) -> NetworkConfig {
        NetworkConfig {
            port: 0,
            ..NetworkConfig::default()
        }
    }

    /// The attestation credentials; the test credentials by default.
    fn attestation(&self) -> Attestation {
        Attestation::Test
    }

    /// A fresh storage for one case; an in-memory storage by default.
    fn storage(&self) -> Arc<dyn Storage> {
        Arc::new(MemoryStorage::new())
    }

    /// The aggregator endpoint the bridge role uses; endpoint 1 by default.
    fn aggregator(&self) -> EndpointId {
        EndpointId::new(1)
    }

    /// How long a case waits for an event or for a request to complete before it fails.
    fn timeout(&self) -> Duration {
        Duration::from_secs(10)
    }
}

/// One case of the suite: a function that panics when the engine breaks the rule it pins.
pub type Case = fn(&dyn EngineUnderTest);

/// Every case of the suite with its name, in the order the [`contract_tests!`] macro
/// expands them.
pub const CASES: &[(&str, Case)] = &[
    ("start_stop_restart", start_stop_restart),
    (
        "second_start_is_already_running",
        second_start_is_already_running,
    ),
    ("duplicate_endpoint_rejected", duplicate_endpoint_rejected),
    ("unknown_parent_rejected", unknown_parent_rejected),
    (
        "missing_initial_value_rejected",
        missing_initial_value_rejected,
    ),
    (
        "global_attribute_in_list_rejected",
        global_attribute_in_list_rejected,
    ),
    ("endpoint_can_be_re_added", endpoint_can_be_re_added),
    (
        "remove_cascades_to_composed_parts",
        remove_cascades_to_composed_parts,
    ),
    (
        "set_attribute_on_unknown_path_rejected",
        set_attribute_on_unknown_path_rejected,
    ),
    ("set_attributes_is_atomic", set_attributes_is_atomic),
    (
        "capacity_matches_capabilities",
        capacity_matches_capabilities,
    ),
    ("commissioning_state_readable", commissioning_state_readable),
    (
        "basic_window_open_close_are_events",
        basic_window_open_close_are_events,
    ),
    (
        "pairing_info_redacted_and_stable",
        pairing_info_redacted_and_stable,
    ),
    (
        "factory_reset_reopens_window_and_clears_engine_keys",
        factory_reset_reopens_window_and_clears_engine_keys,
    ),
    ("engine_keys_are_namespaced", engine_keys_are_namespaced),
    ("stop_ends_event_stream", stop_ends_event_stream),
    (
        "respond_unknown_correlation_rejected",
        respond_unknown_correlation_rejected,
    ),
    (
        "set_reachable_on_bridged_endpoint",
        set_reachable_on_bridged_endpoint,
    ),
    (
        "errors_never_contain_the_passcode",
        errors_never_contain_the_passcode,
    ),
];

/// Expands to one `#[test]` function per case of the engine contract suite.
///
/// The argument is an expression that builds the [`EngineUnderTest`]; it is evaluated once per
/// test. The tests are named like the cases, so a failure names the contract rule that was
/// broken. Expand the macro inside a test module of the crate that implements the engine:
///
/// ```ignore
/// matter_rs_engine::contract_tests!(MyEngineUnderTest::new());
/// ```
#[macro_export]
macro_rules! contract_tests {
    ($eut:expr) => {
        $crate::contract_tests! {
            @cases $eut;
            start_stop_restart,
            second_start_is_already_running,
            duplicate_endpoint_rejected,
            unknown_parent_rejected,
            missing_initial_value_rejected,
            global_attribute_in_list_rejected,
            endpoint_can_be_re_added,
            remove_cascades_to_composed_parts,
            set_attribute_on_unknown_path_rejected,
            set_attributes_is_atomic,
            capacity_matches_capabilities,
            commissioning_state_readable,
            basic_window_open_close_are_events,
            pairing_info_redacted_and_stable,
            factory_reset_reopens_window_and_clears_engine_keys,
            engine_keys_are_namespaced,
            stop_ends_event_stream,
            respond_unknown_correlation_rejected,
            set_reachable_on_bridged_endpoint,
            errors_never_contain_the_passcode,
        }
    };
    (@cases $eut:expr; $($case:ident),* $(,)?) => {
        $(
            #[test]
            fn $case() {
                let under_test = $eut;
                let under_test: &dyn $crate::contract::EngineUnderTest = &under_test;
                $crate::contract::$case(under_test);
            }
        )*
    };
}

/// Runs a future to completion on the current thread, failing when `timeout` passes first.
///
/// # Panics
///
/// When the future is not ready within `timeout`.
pub fn block_on_within<F: Future>(future: F, timeout: Duration) -> F::Output {
    struct ThreadWaker(Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let deadline = Instant::now() + timeout;
    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        let now = Instant::now();
        assert!(
            now < deadline,
            "the engine did not answer within {timeout:?}"
        );
        thread::park_timeout(deadline - now);
    }
}

/// A storage that records every key the engine touches, so that the suite can check the
/// engine's namespace and what a factory reset removes.
///
/// It wraps the storage the [`EngineUnderTest`] provides and forwards every call.
pub struct RecordingStorage {
    inner: Arc<dyn Storage>,
    touched: Mutex<BTreeSet<String>>,
    present: Mutex<BTreeSet<String>>,
}

impl RecordingStorage {
    /// Wraps `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn Storage>) -> Self {
        Self {
            inner,
            touched: Mutex::new(BTreeSet::new()),
            present: Mutex::new(BTreeSet::new()),
        }
    }

    /// Every key that was loaded, stored or removed through this storage, in order.
    #[must_use]
    pub fn touched(&self) -> Vec<String> {
        self.touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// Every key that was stored through this storage and not removed since, in order.
    #[must_use]
    pub fn present(&self) -> Vec<String> {
        self.present
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn touch(&self, key: &str) {
        self.touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(String::from(key));
    }
}

impl fmt::Debug for RecordingStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingStorage")
            .field("touched", &self.touched())
            .field("present", &self.present())
            .finish_non_exhaustive()
    }
}

impl Storage for RecordingStorage {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.touch(key);
        self.inner.load(key)
    }

    fn store(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        self.touch(key);
        self.inner.store(key, value)?;
        self.present
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(String::from(key));
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<(), StorageError> {
        self.touch(key);
        self.inner.remove(key)?;
        self.present
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
        Ok(())
    }
}

/// Starts a node on `engine` with the parameters of `eut` and the given storage.
///
/// # Panics
///
/// When the engine refuses to start or does not answer within the timeout.
pub fn start_on(
    eut: &dyn EngineUnderTest,
    engine: &dyn Engine,
    storage: Arc<dyn Storage>,
) -> Arc<dyn Running> {
    let node = block_on_within(
        engine.start(
            eut.identity(),
            eut.network(),
            eut.attestation(),
            NodeRole::Bridge {
                aggregator: eut.aggregator(),
            },
            storage,
        ),
        eut.timeout(),
    )
    .expect("the node starts");
    Arc::from(node)
}

/// The next event of `node`, within the timeout of `eut`.
///
/// # Panics
///
/// When no event arrives in time or the stream has ended.
pub fn next_event(eut: &dyn EngineUnderTest, node: &dyn Running) -> Event {
    block_on_within(node.next_event(), eut.timeout()).expect("an event")
}

/// Stops `node` and checks that [`Event::Stopped`] with [`StopReason::Requested`] is the next
/// event.
///
/// # Panics
///
/// When the stop fails or the stop event does not follow.
///
/// [`StopReason::Requested`]: crate::StopReason::Requested
pub fn stop(eut: &dyn EngineUnderTest, node: &dyn Running) {
    block_on_within(node.stop(), eut.timeout()).expect("the node stops");
    assert_eq!(
        next_event(eut, node),
        Event::Stopped(crate::engine::StopReason::Requested),
        "the stop event is the last event"
    );
}

/// Runs a request to completion within the timeout of `eut`.
///
/// # Panics
///
/// When the engine does not answer in time.
pub fn run<T>(eut: &dyn EngineUnderTest, future: impl Future<Output = T>) -> T {
    block_on_within(future, eut.timeout())
}

/// Checks that `error` is an engine refusal that fits `predicate`, with a readable message
/// when it does not.
#[track_caller]
pub(crate) fn assert_error(
    result: &Result<(), Error>,
    what: &str,
    predicate: impl Fn(&Error) -> bool,
) {
    match result {
        Err(error) if predicate(error) => {}
        Err(error) => panic!("{what}: unexpected error {error:?}"),
        Ok(()) => panic!("{what}: the engine accepted it"),
    }
}

#[cfg(test)]
mod tests {
    use super::{CASES, EngineUnderTest, RecordingStorage, block_on_within};
    use crate::storage::{MemoryStorage, Storage};
    use alloc::sync::Arc;
    use core::time::Duration;

    #[test]
    fn case_table_is_unique_and_named() {
        let mut names: Vec<&str> = CASES.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CASES.len(), "every case is listed once");
        assert_eq!(CASES.len(), 20);
    }

    #[test]
    fn case_table_matches_the_macro() {
        // The macro lists the cases a second time; this keeps both lists the same.
        let source = include_str!("mod.rs");
        let macro_body = source
            .split("@cases $eut;")
            .nth(1)
            .expect("the macro's case list")
            .split('}')
            .next()
            .expect("the end of the case list");
        let listed: Vec<&str> = macro_body
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        let table: Vec<&str> = CASES.iter().map(|(name, _)| *name).collect();
        assert_eq!(listed, table);
    }

    #[test]
    fn recording_storage_tracks_keys() {
        let inner: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
        let storage = RecordingStorage::new(inner);
        storage.store("engine/a", b"1").unwrap();
        storage.store("product/b", b"2").unwrap();
        assert_eq!(storage.load("engine/a").unwrap(), Some(b"1".to_vec()));
        storage.remove("engine/a").unwrap();
        assert_eq!(storage.load("engine/a").unwrap(), None);
        assert_eq!(storage.present(), vec!["product/b"]);
        assert_eq!(storage.touched(), vec!["engine/a", "product/b"]);
    }

    #[test]
    fn block_on_within_returns_ready_output() {
        assert_eq!(block_on_within(async { 7 }, Duration::from_secs(1)), 7);
    }

    #[test]
    #[should_panic(expected = "did not answer")]
    fn block_on_within_fails_on_timeout() {
        block_on_within(core::future::pending::<()>(), Duration::from_millis(50));
    }

    #[test]
    fn defaults_are_the_development_identity() {
        struct Nothing;
        impl EngineUnderTest for Nothing {
            fn engine(&self) -> alloc::boxed::Box<dyn crate::Engine> {
                unreachable!("not started here")
            }
        }
        let identity = Nothing.identity();
        assert_eq!(*identity.passcode.expose(), 20_202_021);
        assert_eq!(identity.discriminator, 3840);
        assert_eq!(Nothing.network().port, 0);
        assert_eq!(Nothing.aggregator(), crate::EndpointId::new(1));
        assert!(matches!(Nothing.attestation(), crate::Attestation::Test));
    }
}
