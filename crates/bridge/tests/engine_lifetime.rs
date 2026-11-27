//! The bridge keeps the engine it was started on for as long as it runs.
//!
//! Some engines stop their node when they are dropped. The test imitates such an engine with a
//! wrapper around the fake engine and checks that the bridge does not drop it early.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use matter_rs_bridge::Bridge;
use matter_rs_engine::testing::{FakeEngine, block_on};
use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, BoxFuture, Capabilities, ClusterId,
    CommissioningState, Correlation, EndpointDescription, EndpointId, Engine, Error, Event,
    EventId, FabricIndex, FabricInfo, MemoryStorage, NetworkConfig, NodeIdentity, NodeRole,
    PairingInfo, ProductId, Response, Running, Secret, Storage, Value, VendorId, Window,
};

fn identity() -> NodeIdentity {
    NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
        vendor_name: "matter-rs".into(),
        product_name: "Bridge".into(),
        node_label: "Bridge".into(),
        serial_number: None,
        software_version: 1,
        software_version_string: "0.0.0".into(),
        unique_id: "lifetime-test".into(),
        passcode: Secret::new(20_202_021),
        discriminator: 3840,
    }
}

type Node = Arc<Mutex<Option<Arc<dyn Running>>>>;

/// An engine that stops its node when it is dropped, as the rs-matter engine does, and records
/// the drop.
struct StopsOnDrop {
    inner: FakeEngine,
    node: Node,
    dropped: Arc<AtomicBool>,
}

impl Drop for StopsOnDrop {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        let node = self.node.lock().unwrap().take();
        if let Some(node) = node {
            let _ = block_on(node.stop());
        }
    }
}

impl Engine for StopsOnDrop {
    fn start(
        &self,
        identity: NodeIdentity,
        network: NetworkConfig,
        attestation: Attestation,
        role: NodeRole,
        storage: Arc<dyn Storage>,
    ) -> BoxFuture<'_, Result<Box<dyn Running>, Error>> {
        Box::pin(async move {
            let running = self
                .inner
                .start(identity, network, attestation, role, storage)
                .await?;
            let running: Arc<dyn Running> = Arc::from(running);
            *self.node.lock().unwrap() = Some(Arc::clone(&running));
            let forward: Box<dyn Running> = Box::new(Forward(running));
            Ok(forward)
        })
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

/// Hands every call to the wrapped node, so that the engine can keep a handle to stop it.
struct Forward(Arc<dyn Running>);

impl Running for Forward {
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
    fn commissioning_state(&self) -> BoxFuture<'_, Result<CommissioningState, Error>> {
        self.0.commissioning_state()
    }
    fn open_commissioning_window(
        &self,
        window: Window,
        timeout: Duration,
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

#[test]
fn bridge_keeps_the_engine_alive() {
    let fake = FakeEngine::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let engine = StopsOnDrop {
        inner: fake.clone(),
        node: Node::default(),
        dropped: Arc::clone(&dropped),
    };

    let bridge = block_on(Bridge::start(
        Box::new(engine),
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        Arc::new(MemoryStorage::new()),
    ))
    .unwrap();

    // `start` has returned: the engine is still there and the node still serves.
    assert!(!dropped.load(Ordering::SeqCst), "the engine was dropped");
    assert!(fake.is_running(), "the node stopped");
    block_on(bridge.commissioning_state()).unwrap();

    block_on(bridge.stop()).unwrap();
    drop(bridge);
    assert!(
        dropped.load(Ordering::SeqCst),
        "the engine was not dropped with the bridge"
    );
}
