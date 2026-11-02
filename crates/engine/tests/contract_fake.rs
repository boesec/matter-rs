//! The engine contract suite against the fake engine, plus two proofs about the suite itself:
//! it is written against the facade only, and a broken engine fails it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use matter_rs_engine::contract::{CASES, EngineUnderTest};
use matter_rs_engine::testing::FakeEngine;
use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, BoxFuture, Capabilities, ClusterId,
    CommissioningState, Correlation, EndpointDescription, EndpointId, Engine, Error, Event,
    EventId, FabricIndex, FabricInfo, NetworkConfig, NodeIdentity, NodeRole, PairingInfo, Response,
    Running, Storage, Value, Window,
};

/// The fake engine as the suite sees it.
struct FakeUnderTest;

impl EngineUnderTest for FakeUnderTest {
    fn engine(&self) -> Box<dyn Engine> {
        Box::new(FakeEngine::new())
    }
}

matter_rs_engine::contract_tests!(FakeUnderTest);

/// The suite names every case once, and the table and the macro agree on the names.
#[test]
fn every_case_is_in_the_table() {
    let names: Vec<&str> = CASES.iter().map(|(name, _)| *name).collect();
    for expected in [
        "start_stop_restart",
        "second_start_is_already_running",
        "duplicate_endpoint_rejected",
        "unknown_parent_rejected",
        "missing_initial_value_rejected",
        "global_attribute_in_list_rejected",
        "endpoint_can_be_re_added",
        "remove_cascades_to_composed_parts",
        "set_attribute_on_unknown_path_rejected",
        "set_attributes_is_atomic",
        "capacity_matches_capabilities",
        "commissioning_state_readable",
        "basic_window_open_close_are_events",
        "pairing_info_redacted_and_stable",
        "factory_reset_reopens_window_and_clears_engine_keys",
        "engine_keys_are_namespaced",
        "stop_ends_event_stream",
        "respond_unknown_correlation_rejected",
        "set_reachable_on_bridged_endpoint",
        "errors_never_contain_the_passcode",
    ] {
        assert!(names.contains(&expected), "case {expected} is missing");
    }
}

/// The suite must stay engine-agnostic: its source names nothing of the fake engine.
#[test]
fn contract_module_uses_only_the_facade() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contract");
    let mut files = 0;
    for entry in std::fs::read_dir(&directory).expect("the contract module directory") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            files += 1;
            let source = std::fs::read_to_string(&path).expect("readable source");
            for forbidden in ["FakeEngine", "testing::", "mod testing", "EmittedEvent"] {
                assert!(
                    !source.contains(forbidden),
                    "{} names {forbidden}, which belongs to the fake engine",
                    path.display()
                );
            }
        }
    }
    assert!(files >= 2, "the module has a root and its cases");
}

/// A wrapper around the fake engine that breaks one rule: the commissioning state it reports
/// is wrong. The suite must catch it, and only the cases that pin that rule may fail.
#[derive(Clone)]
struct BrokenEngine(FakeEngine);

struct BrokenRunning(Box<dyn Running>);

impl Engine for BrokenEngine {
    fn start(
        &self,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        role: NodeRole,
        storage: Arc<dyn Storage>,
    ) -> BoxFuture<'_, Result<Box<dyn Running>, Error>> {
        Box::pin(async move {
            let inner = self
                .0
                .start(identity, network, attestation, role, storage)
                .await?;
            let broken: Box<dyn Running> = Box::new(BrokenRunning(inner));
            Ok(broken)
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
}

impl Running for BrokenRunning {
    fn add_endpoint(&self, description: EndpointDescription) -> BoxFuture<'_, Result<(), Error>> {
        self.0.add_endpoint(description)
    }

    fn remove_endpoint(&self, endpoint: EndpointId) -> BoxFuture<'_, Result<(), Error>> {
        self.0.remove_endpoint(endpoint)
    }

    fn set_attribute(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.0.set_attribute(endpoint, cluster, attribute, value)
    }

    fn set_attributes(&self, values: Vec<AttributeValue>) -> BoxFuture<'_, Result<(), Error>> {
        self.0.set_attributes(values)
    }

    fn emit_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
        payload: Value,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.0.emit_event(endpoint, cluster, event, payload)
    }

    fn set_reachable(
        &self,
        endpoint: EndpointId,
        reachable: bool,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.0.set_reachable(endpoint, reachable)
    }

    fn respond(
        &self,
        correlation: Correlation,
        response: Response,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.0.respond(correlation, response)
    }

    /// Wrong: always reports a commissioned node without a window.
    fn commissioning_state(&self) -> BoxFuture<'_, Result<CommissioningState, Error>> {
        Box::pin(async move {
            self.0.commissioning_state().await?;
            Ok(CommissioningState {
                commissioned: true,
                window: None,
            })
        })
    }

    fn open_commissioning_window(
        &self,
        window: Window,
        timeout: std::time::Duration,
    ) -> BoxFuture<'_, Result<(), Error>> {
        self.0.open_commissioning_window(window, timeout)
    }

    fn close_commissioning_window(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.0.close_commissioning_window()
    }

    fn fabrics(&self) -> BoxFuture<'_, Result<Vec<FabricInfo>, Error>> {
        self.0.fabrics()
    }

    fn remove_fabric(&self, fabric: FabricIndex) -> BoxFuture<'_, Result<(), Error>> {
        self.0.remove_fabric(fabric)
    }

    fn pairing_info(&self) -> BoxFuture<'_, Result<PairingInfo, Error>> {
        self.0.pairing_info()
    }

    fn factory_reset(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.0.factory_reset()
    }

    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    fn next_event(&self) -> BoxFuture<'_, Result<Event, Error>> {
        self.0.next_event()
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), Error>> {
        self.0.stop()
    }
}

struct BrokenUnderTest;

impl EngineUnderTest for BrokenUnderTest {
    fn engine(&self) -> Box<dyn Engine> {
        Box::new(BrokenEngine(FakeEngine::new()))
    }

    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(2)
    }
}

/// The suite is not vacuous: an engine that reports the commissioning state wrong fails the
/// cases that pin that rule, named, and the cases that do not read the state still pass.
#[test]
fn broken_engine_fails_the_suite() {
    let mut failed: Vec<&str> = Vec::new();
    for (name, case) in CASES {
        let outcome = catch_unwind(AssertUnwindSafe(|| case(&BrokenUnderTest)));
        if outcome.is_err() {
            failed.push(name);
        }
    }
    for expected in [
        "commissioning_state_readable",
        "start_stop_restart",
        "basic_window_open_close_are_events",
        "factory_reset_reopens_window_and_clears_engine_keys",
    ] {
        assert!(
            failed.contains(&expected),
            "the wrong commissioning state went unnoticed by {expected}; failed: {failed:?}"
        );
    }
    for unrelated in [
        "duplicate_endpoint_rejected",
        "stop_ends_event_stream",
        "set_attributes_is_atomic",
    ] {
        assert!(
            !failed.contains(&unrelated),
            "{unrelated} does not read the state and must pass; failed: {failed:?}"
        );
    }
}
