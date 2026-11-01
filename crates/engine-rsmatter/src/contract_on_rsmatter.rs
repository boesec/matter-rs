//! The engine contract on rs-matter, in process: the tests of the fake engine that need no
//! Matter client, run against a real node on a dedicated thread, plus what only this engine
//! can show (mDNS, storage, logs, the response timeout).
//!
//! Every test starts its own node on an ephemeral Matter port; the mDNS responders share port
//! 5353. The interface comes from the engine's heuristic, or from `MATTER_RS_TEST_INTERFACE`.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::pin::pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use matter_rs_engine::testing::block_on;
use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, Capabilities, ClusterDescription, ClusterId,
    CommandId, CommissioningState, Correlation, DeviceType, DeviceTypeId, EndpointDescription,
    EndpointId, Engine, Error, Event, EventId, FabricIndex, MemoryStorage, NetworkConfig,
    NodeIdentity, NodeRole, ProductId, REDACTED, Response, Running, Secret, Status, StopReason,
    Storage, Value, VendorId, Window, bridged_device,
};
use rs_matter::Matter;
use rs_matter::cert::r#gen::VALID_FOREVER;
use rs_matter::crypto::{
    CanonAeadKeyRef, CanonPkcSecretKey, Crypto, SecretKey, SigningSecretKey, test_only_crypto,
};
use rs_matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::fabric::FabricPersist;
use rs_matter::onboard::cac::RcacGenerator;
use rs_matter::onboard::noc::NocGenerator;
use rs_matter::transport::network::mdns::{MDNS_IPV4_BROADCAST_ADDR, MDNS_PORT};

use crate::engine::RsMatterEngine;
use crate::network;
use crate::store::{FacadeKvStore, KeyIndex};

const AGGREGATOR: EndpointId = EndpointId::new(1);
const LIGHT: EndpointId = EndpointId::new(2);
const ON_OFF: ClusterId = ClusterId::new(0x0006);
const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0x0000);
const ON: CommandId = CommandId::new(0x01);
const TOGGLE: CommandId = CommandId::new(0x02);
const LEVEL: ClusterId = ClusterId::new(0x0008);
const CURRENT_LEVEL: AttributeId = AttributeId::new(0x0000);
const ON_OFF_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0100);
const NODE_LABEL: AttributeId = AttributeId::new(0x0005);

/// The passcode of every test identity; the test vector of the reference study.
const PASSCODE: u32 = 20_202_021;
/// The manual pairing code of `PASSCODE` with discriminator 3840.
const MANUAL_CODE: &str = "34970112332";

/// How long a test waits for an event before it fails.
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

fn identity() -> NodeIdentity {
    NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
        vendor_name: "matter-rs".into(),
        product_name: "Test node".into(),
        node_label: "Test node".into(),
        serial_number: None,
        software_version: 1,
        software_version_string: "0.0.0".into(),
        unique_id: "contract-test".into(),
        passcode: Secret::new(PASSCODE),
        discriminator: 3840,
    }
}

/// An ephemeral Matter port on the interface the environment names, or the engine's choice.
fn network() -> NetworkConfig {
    NetworkConfig {
        interface: std::env::var("MATTER_RS_TEST_INTERFACE")
            .ok()
            .filter(|name| !name.is_empty()),
        port: 0,
        ..NetworkConfig::default()
    }
}

fn engine() -> RsMatterEngine {
    RsMatterEngine::new().with_response_timeout(Duration::from_millis(200))
}

fn start_with(
    engine: &RsMatterEngine,
    identity: NodeIdentity,
    storage: Arc<dyn Storage>,
) -> Arc<dyn Running> {
    let engine: &dyn Engine = engine;
    let node = block_on(engine.start(
        identity,
        network(),
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        storage,
    ))
    .expect("the node starts");
    Arc::from(node)
}

fn start(engine: &RsMatterEngine, storage: Arc<dyn Storage>) -> Arc<dyn Running> {
    start_with(engine, identity(), storage)
}

fn on_off_cluster() -> ClusterDescription {
    ClusterDescription {
        id: ON_OFF,
        revision: 6,
        feature_map: 0,
        attributes: vec![ON_OFF_ATTRIBUTE],
        accepted_commands: vec![ON, TOGGLE],
        generated_commands: vec![],
        events: vec![],
    }
}

fn level_cluster() -> ClusterDescription {
    ClusterDescription {
        id: LEVEL,
        revision: 6,
        feature_map: 0,
        attributes: vec![CURRENT_LEVEL],
        accepted_commands: vec![],
        generated_commands: vec![],
        events: vec![],
    }
}

fn bridged_device_cluster() -> ClusterDescription {
    ClusterDescription {
        id: bridged_device::CLUSTER,
        revision: 6,
        feature_map: 0,
        attributes: vec![NODE_LABEL, bridged_device::REACHABLE],
        accepted_commands: vec![],
        generated_commands: vec![],
        events: vec![bridged_device::REACHABLE_CHANGED],
    }
}

fn light(id: EndpointId) -> EndpointDescription {
    EndpointDescription {
        id,
        device_types: vec![DeviceType {
            id: ON_OFF_LIGHT,
            revision: 3,
        }],
        clusters: vec![on_off_cluster(), level_cluster()],
        parent: None,
        initial_values: vec![
            (ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false)),
            (LEVEL, CURRENT_LEVEL, Value::U8(0)),
        ],
    }
}

/// Runs a future to completion, failing the test when `timeout` passes first.
fn block_on_within<F: Future>(future: F, timeout: Duration) -> F::Output {
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
        assert!(now < deadline, "timed out after {timeout:?}");
        thread::park_timeout(deadline - now);
    }
}

fn next_event(node: &dyn Running) -> Event {
    block_on_within(node.next_event(), EVENT_TIMEOUT).expect("an event")
}

fn stop(node: &dyn Running) {
    block_on_within(node.stop(), EVENT_TIMEOUT).expect("the node stops");
}

#[test]
fn start_stop_restart() {
    let engine = engine();
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let node = start(&engine, Arc::clone(&storage));
    assert_eq!(
        block_on(node.commissioning_state()).unwrap(),
        CommissioningState {
            commissioned: false,
            window: Some(Window::Basic),
        }
    );
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
    assert_eq!(block_on(node.next_event()), Err(Error::NotRunning));
    assert_eq!(block_on(node.stop()), Err(Error::NotRunning));

    // The same engine starts again on the same storage; endpoints are not persisted.
    let node = start(&engine, storage);
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![]);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
}

#[test]
fn duplicate_endpoint_rejected() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    assert_eq!(
        block_on(node.add_endpoint(light(LIGHT))),
        Err(Error::DuplicateEndpoint(LIGHT))
    );
    assert_eq!(
        block_on(node.add_endpoint(light(AGGREGATOR))),
        Err(Error::DuplicateEndpoint(AGGREGATOR))
    );
    let root = block_on(node.add_endpoint(light(EndpointId::new(0)))).unwrap_err();
    assert!(
        matches!(&root, Error::InvalidDescription { endpoint, reason }
            if *endpoint == EndpointId::new(0) && reason.contains("root")),
        "{root}"
    );
    stop(&*node);
}

#[test]
fn missing_initial_value_rejected() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let without_level = EndpointDescription {
        initial_values: vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))],
        ..light(LIGHT)
    };
    match block_on(node.add_endpoint(without_level)).unwrap_err() {
        Error::InvalidDescription { endpoint, reason } => {
            assert_eq!(endpoint, LIGHT);
            assert!(reason.contains("0x0000"), "{reason}");
            assert!(reason.contains("0x0008"), "{reason}");
            assert!(reason.contains("no initial value"), "{reason}");
        }
        other => panic!("unexpected error {other:?}"),
    }
    // The endpoint was not added: the id is free.
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);
}

#[test]
fn capacity_reported() {
    let engine = engine().with_endpoint_capacity(2);
    let expected = Capabilities {
        max_endpoints: Some(2),
        max_fabrics: Some(5),
        max_subscriptions: Some(15),
        enhanced_window: false,
    };
    assert_eq!(Engine::capabilities(&engine), expected);
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    assert_eq!(node.capabilities(), expected);
    block_on(node.add_endpoint(light(EndpointId::new(2)))).unwrap();
    block_on(node.add_endpoint(light(EndpointId::new(3)))).unwrap();
    assert_eq!(
        block_on(node.add_endpoint(light(EndpointId::new(4)))),
        Err(Error::CapacityExceeded { what: "endpoints" })
    );
    block_on(node.remove_endpoint(EndpointId::new(2))).unwrap();
    block_on(node.add_endpoint(light(EndpointId::new(4)))).unwrap();
    assert_eq!(
        block_on(node.open_commissioning_window(Window::Enhanced, Duration::from_secs(60))),
        Err(Error::Unsupported {
            operation: "enhanced commissioning window"
        })
    );
    stop(&*node);
}

/// Installs a fabric into `storage` the way a commissioner would leave it: a root CA, a node
/// operational certificate signed by it and the fabric record under rs-matter's keys.
fn install_fabric(storage: &Arc<dyn Storage>, fabric_id: u64, node_id: u64) -> u8 {
    const IPK: [u8; 16] = [0x5a; 16];

    let crypto = test_only_crypto();
    let mut rcac_buffer = [0_u8; 1024];
    let mut rcac_generator = RcacGenerator::new(&mut rcac_buffer);
    let (rcac_key, rcac) = rcac_generator
        .generate(&crypto, fabric_id, VALID_FOREVER)
        .expect("root certificate");

    let node_key = crypto.generate_secret_key().expect("node key");
    let mut node_secret = CanonPkcSecretKey::new();
    node_key
        .write_canon(&mut node_secret)
        .expect("canonical key");
    let mut csr_buffer = [0_u8; 1024];
    let csr = node_key.csr(&mut csr_buffer).expect("csr");

    let mut noc_buffer = [0_u8; 1024];
    let mut generator = NocGenerator::create(rcac_key.reference(), rcac, &[], &mut noc_buffer)
        .expect("noc generator");
    let noc = generator
        .generate(&crypto, csr, node_id, &[], VALID_FOREVER)
        .expect("node certificate");

    // A node object only for its fabric table and persistence; it never runs.
    let matter = Matter::new(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT, 5540);
    let index = Arc::new(KeyIndex::load(Arc::clone(storage)).expect("key index"));
    let kv = matter.kv(FacadeKvStore::new(Arc::clone(storage), index));
    matter.with_state(|state| {
        let fabric = state
            .fabrics
            .add(
                &crypto,
                node_secret.reference(),
                rcac,
                noc,
                &[],
                Some(CanonAeadKeyRef::new(&IPK)),
                0xFFF1,
                node_id,
            )
            .expect("fabric added");
        let index = fabric.fab_idx();
        let fabric = state
            .fabrics
            .update_label(index, "contract")
            .expect("label set");
        let mut persist = FabricPersist::new(&kv);
        persist.store(fabric).expect("fabric stored");
        persist
            .store_next_fab_idx(state.fabrics.next_fab_idx())
            .expect("next index stored");
        persist.run().expect("persisted");
        index.get()
    })
}

#[test]
fn fabrics_survive_restart_in_storage() {
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let index = install_fabric(&storage, 0x1234, 0x5678);

    let engine = engine();
    let node = start(&engine, Arc::clone(&storage));
    let fabrics = block_on(node.fabrics()).unwrap();
    assert_eq!(fabrics.len(), 1);
    assert_eq!(fabrics[0].index, FabricIndex::new(index));
    assert_eq!(fabrics[0].vendor_id, VendorId::new(0xFFF1));
    assert_eq!(fabrics[0].fabric_id, 0x1234);
    assert_eq!(fabrics[0].node_id, 0x5678);
    assert_eq!(fabrics[0].label, "contract");
    // A commissioned node starts without a window.
    assert_eq!(
        block_on(node.commissioning_state()).unwrap(),
        CommissioningState {
            commissioned: true,
            window: None,
        }
    );
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));

    // The fabric lives in the storage, under the engine's keys.
    let memory = Arc::new(MemoryStorage::new());
    let memory_storage: Arc<dyn Storage> = memory.clone();
    install_fabric(&memory_storage, 1, 2);
    assert!(
        memory
            .keys()
            .iter()
            .all(|key| key.starts_with("engine/rsmatter/")),
        "{:?}",
        memory.keys()
    );

    // Restart: still there. Removing it reports the events and persists the removal.
    let node = start(&engine, Arc::clone(&storage));
    assert_eq!(block_on(node.fabrics()).unwrap().len(), 1);
    block_on(node.remove_fabric(FabricIndex::new(index))).unwrap();
    assert_eq!(
        block_on(node.remove_fabric(FabricIndex::new(index))),
        Err(Error::UnknownFabric(FabricIndex::new(index)))
    );
    assert_eq!(
        next_event(&*node),
        Event::FabricRemoved(FabricIndex::new(index))
    );
    assert_eq!(
        next_event(&*node),
        Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: None,
        })
    );
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));

    let node = start(&engine, Arc::clone(&storage));
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![]);
    stop(&*node);
}

#[test]
fn factory_reset_removes_fabrics_and_keys() {
    let memory = Arc::new(MemoryStorage::new());
    let storage: Arc<dyn Storage> = memory.clone();
    let index = install_fabric(&storage, 0x42, 0x43);
    memory.store("bridge/own", b"kept").unwrap();

    let engine = engine();
    let node = start(&engine, Arc::clone(&storage));
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    block_on(node.factory_reset()).unwrap();
    assert_eq!(
        next_event(&*node),
        Event::FabricRemoved(FabricIndex::new(index))
    );
    assert_eq!(
        next_event(&*node),
        Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: None,
        })
    );
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![]);
    // The engine's keys are gone, the product's stay; the node keeps running.
    assert_eq!(memory.keys(), vec!["bridge/own"]);
    block_on(node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))).unwrap();
    block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(300))).unwrap();
    assert_eq!(
        next_event(&*node),
        Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: Some(Window::Basic),
        })
    );
    stop(&*node);
}

#[test]
fn unanswered_command_times_out() {
    use crate::channel::Shared;
    use crate::handler::DynamicHandler;
    use crate::table::Table;
    use std::cell::RefCell;
    use std::rc::Rc;

    // The handler waits for the product's response with its timeout; the Interaction Model
    // turns the error into a `Failure` status for the client.
    let shared = Arc::new(Shared::new());
    let table = Rc::new(RefCell::new(Table::new(AGGREGATOR, 4)));
    let handler = DynamicHandler::new(table, Arc::clone(&shared), Duration::from_millis(100));

    let correlation = shared.issue();
    let started = Instant::now();
    let error = block_on_within(handler.response(correlation), Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.code(), rs_matter::error::ErrorCode::Failure);
    assert!(started.elapsed() >= Duration::from_millis(100));
    // A late response is unknown.
    assert_eq!(
        shared.respond(correlation, Response::success()),
        Err(Error::UnknownCorrelation(correlation))
    );

    // Answered in time, the response gets through.
    let correlation = shared.issue();
    let responder = {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            shared.respond(correlation, Response::status(Status::ConstraintError))
        })
    };
    let response = block_on_within(handler.response(correlation), Duration::from_secs(5)).unwrap();
    assert_eq!(response.status, Status::ConstraintError);
    responder.join().unwrap().unwrap();
}

/// A DNS query for the commissionable service type.
fn mdns_query() -> Vec<u8> {
    let mut packet = vec![
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    for label in ["_matterc", "_udp", "local"] {
        packet.push(u8::try_from(label.len()).unwrap());
        packet.extend_from_slice(label.as_bytes());
    }
    packet.extend_from_slice(&[0x00, 0x00, 0x0C, 0x00, 0x01]);
    packet
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn node_announces_on_mdns() {
    let config = network();
    let interface = network::select_interface(&config).expect("an interface");
    let Some(ipv4) = interface.ipv4 else {
        panic!("the test needs an interface with an IPv4 address");
    };

    // A listener on the mDNS port, joined to the group on the node's interface, sees the
    // node's announcements and its multicast answers. Its multicast interface is set
    // explicitly, as the engine does, so that a multi-homed host loops the packets back.
    let listener = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    listener.set_reuse_address(true).unwrap();
    #[cfg(unix)]
    listener.set_reuse_port(true).unwrap();
    listener
        .bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT)).into())
        .unwrap();
    listener.set_multicast_if_v4(&ipv4).unwrap();
    listener.set_multicast_loop_v4(true).unwrap();
    let listener: UdpSocket = listener.into();
    listener
        .join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &ipv4)
        .unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    // A legacy (unicast) querier on an ephemeral port gets a unicast answer.
    let querier = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    querier
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();

    let mut identity = identity();
    identity.discriminator = 2345;
    identity.unique_id = "mdns-test".into();
    let engine = engine();
    let node = start_with(&engine, identity, Arc::new(MemoryStorage::new()));

    let group = SocketAddr::V4(SocketAddrV4::new(MDNS_IPV4_BROADCAST_ADDR, MDNS_PORT));
    let direct = SocketAddr::V4(SocketAddrV4::new(ipv4, MDNS_PORT));
    let is_announcement =
        |packet: &[u8]| contains(packet, b"_matterc_udplocal") && contains(packet, b"D=2345");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = false;
    let mut buffer = [0_u8; 2048];
    while Instant::now() < deadline && !seen {
        listener.send_to(&mdns_query(), group).unwrap();
        querier.send_to(&mdns_query(), direct).unwrap();
        if let Ok((len, _)) = listener.recv_from(&mut buffer) {
            seen = is_announcement(&buffer[..len]);
        }
        if !seen && let Ok((len, _)) = querier.recv_from(&mut buffer) {
            seen = is_announcement(&buffer[..len]);
        }
    }
    stop(&*node);
    assert!(
        seen,
        "no _matterc._udp announcement with D=2345 was received"
    );
}

#[cfg(unix)]
#[test]
fn state_files_are_restricted() {
    use std::os::unix::fs::PermissionsExt as _;

    use matter_rs_engine::DirectoryStorage;

    let directory = std::env::temp_dir().join(format!(
        "matter-rs-engine-rsmatter-{}-state",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    let storage = Arc::new(DirectoryStorage::open(&directory).unwrap());
    let engine = engine();
    let node = start(&engine, storage);
    // Adding an endpoint bumps the configuration version, which rs-matter persists.
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);

    let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "directory mode {mode:o}");
    let mut files = 0;
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(name.starts_with("engine%2Frsmatter%2F"), "{name}");
        let mode = entry.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{name}: mode {mode:o}");
        files += 1;
    }
    assert!(
        files >= 2,
        "the configuration version and the key index are persisted"
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// A logger that keeps every line, so that tests can look for secrets in them.
struct Capture(Mutex<Vec<String>>);

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{} {}", record.level(), record.args()));
    }

    fn flush(&self) {}
}

fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<&'static Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture: &'static Capture = Box::leak(Box::new(Capture(Mutex::new(Vec::new()))));
        log::set_logger(capture).expect("the first logger of the process");
        log::set_max_level(log::LevelFilter::Trace);
        capture
    })
}

#[test]
fn logs_contain_no_secret() {
    let capture = capture();
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let info = block_on(node.pairing_info()).unwrap();
    assert_eq!(info.manual_code.expose(), MANUAL_CODE);
    block_on(node.close_commissioning_window()).unwrap();
    assert!(matches!(next_event(&*node), Event::CommissioningChanged(_)));
    block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(600))).unwrap();
    assert!(matches!(next_event(&*node), Event::CommissioningChanged(_)));
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));

    let lines = capture.0.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        lines.iter().any(|line| line.contains("node started")),
        "the engine logged nothing; the capture is not installed"
    );
    let qr = info.qr_text.expose().as_str();
    for line in lines.iter() {
        assert!(
            !line.contains(&PASSCODE.to_string()),
            "passcode in log: {line}"
        );
        assert!(!line.contains(MANUAL_CODE), "manual code in log: {line}");
        assert!(
            !line.contains("3497-011-2332"),
            "manual code in log: {line}"
        );
        assert!(!line.contains(qr), "QR text in log: {line}");
        assert!(!line.contains("MT:"), "QR text in log: {line}");
    }
}

#[test]
fn endpoint_lifecycle_and_values() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let unknown = EndpointId::new(99);
    assert_eq!(
        block_on(node.remove_endpoint(unknown)),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        block_on(node.set_attribute(unknown, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        block_on(node.emit_event(unknown, ON_OFF, EventId::new(0), Value::Null)),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        block_on(node.remove_endpoint(AGGREGATOR)),
        Err(Error::UnknownEndpoint(AGGREGATOR))
    );
    assert_eq!(
        block_on(node.add_endpoint(EndpointDescription {
            parent: Some(EndpointId::new(7)),
            ..light(LIGHT)
        })),
        Err(Error::UnknownEndpoint(EndpointId::new(7)))
    );

    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    assert_eq!(
        block_on(node.set_attribute(LIGHT, ON_OFF, AttributeId::new(0x4000), Value::Bool(true))),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: AttributeId::new(0x4000),
        })
    );
    assert_eq!(
        block_on(node.emit_event(LIGHT, ON_OFF, EventId::new(0), Value::Null)),
        Err(Error::UnknownEvent {
            endpoint: LIGHT,
            cluster: ON_OFF,
            event: EventId::new(0),
        })
    );
    assert!(matches!(
        block_on(node.set_reachable(LIGHT, false)),
        Err(Error::UnknownEvent { .. })
    ));
    // All or nothing.
    assert!(matches!(
        block_on(node.set_attributes(vec![
            AttributeValue {
                endpoint: LIGHT,
                cluster: ON_OFF,
                attribute: ON_OFF_ATTRIBUTE,
                value: Value::Bool(true),
            },
            AttributeValue {
                endpoint: LIGHT,
                cluster: LEVEL,
                attribute: AttributeId::new(0x4000),
                value: Value::U8(1),
            },
        ])),
        Err(Error::UnknownAttribute { .. })
    ));
    block_on(node.set_attributes(vec![
        AttributeValue {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: ON_OFF_ATTRIBUTE,
            value: Value::Bool(true),
        },
        AttributeValue {
            endpoint: LIGHT,
            cluster: LEVEL,
            attribute: CURRENT_LEVEL,
            value: Value::U8(100),
        },
    ]))
    .unwrap();

    // Composed endpoints go with their parent; the id can be used again.
    let part = EndpointId::new(3);
    block_on(node.add_endpoint(EndpointDescription {
        parent: Some(LIGHT),
        ..light(part)
    }))
    .unwrap();
    block_on(node.remove_endpoint(LIGHT)).unwrap();
    assert_eq!(
        block_on(node.remove_endpoint(part)),
        Err(Error::UnknownEndpoint(part))
    );
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    // A bridged endpoint: reachability sets the attribute and emits the event.
    let bridged = EndpointId::new(4);
    let mut description = light(bridged);
    description.clusters.push(bridged_device_cluster());
    description
        .initial_values
        .push((bridged_device::CLUSTER, NODE_LABEL, Value::from("Kitchen")));
    description.initial_values.push((
        bridged_device::CLUSTER,
        bridged_device::REACHABLE,
        Value::Bool(true),
    ));
    block_on(node.add_endpoint(description)).unwrap();
    block_on(node.set_reachable(bridged, false)).unwrap();
    block_on(node.emit_event(
        bridged,
        bridged_device::CLUSTER,
        bridged_device::REACHABLE_CHANGED,
        Value::Struct(vec![(0, Value::Bool(true))]),
    ))
    .unwrap();

    // Nothing of this reached the product as an event.
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
}

#[test]
fn commissioning_window_changes_are_events() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let open = CommissioningState {
        commissioned: false,
        window: Some(Window::Basic),
    };
    assert_eq!(block_on(node.commissioning_state()).unwrap(), open);

    block_on(node.close_commissioning_window()).unwrap();
    let closed = CommissioningState {
        commissioned: false,
        window: None,
    };
    assert_eq!(next_event(&*node), Event::CommissioningChanged(closed));
    assert_eq!(block_on(node.commissioning_state()).unwrap(), closed);
    // Closing a closed window is not a change.
    block_on(node.close_commissioning_window()).unwrap();

    block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(60))).unwrap();
    assert_eq!(next_event(&*node), Event::CommissioningChanged(open));
    // A second window while one is open is rs-matter's refusal.
    assert!(matches!(
        block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(60))),
        Err(Error::Engine(_))
    ));
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
}

#[test]
fn pairing_info_is_redacted_and_matches() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let info = block_on(node.pairing_info()).unwrap();
    assert_eq!(info.discriminator, 3840);
    assert_eq!(info.manual_code.expose(), MANUAL_CODE);
    assert!(info.qr_text.expose().starts_with("MT:"));
    let output = format!("{info:?}");
    assert!(output.contains(REDACTED), "{output}");
    assert!(!output.contains(MANUAL_CODE), "{output}");
    assert_eq!(
        crate::pairing_info_for(&identity()).unwrap().qr_text,
        info.qr_text
    );
    stop(&*node);
}

#[test]
fn stop_ends_the_event_stream_and_the_node() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
    assert_eq!(block_on(node.next_event()), Err(Error::NotRunning));
    assert_eq!(block_on(node.stop()), Err(Error::NotRunning));
    assert_eq!(
        block_on(node.respond(Correlation::new(1), Response::success())),
        Err(Error::NotRunning)
    );
    assert_eq!(
        block_on(node.add_endpoint(light(EndpointId::new(3)))),
        Err(Error::NotRunning)
    );
    assert_eq!(block_on(node.commissioning_state()), Err(Error::NotRunning));
    assert_eq!(block_on(node.fabrics()), Err(Error::NotRunning));
    assert_eq!(block_on(node.pairing_info()), Err(Error::NotRunning));
    assert_eq!(block_on(node.factory_reset()), Err(Error::NotRunning));
    assert_eq!(
        block_on(node.close_commissioning_window()),
        Err(Error::NotRunning)
    );
}

#[test]
fn engine_runs_one_node_at_a_time() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let engine_ref: &dyn Engine = &engine;
    let second = block_on(engine_ref.start(
        identity(),
        network(),
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::new(MemoryStorage::new()),
    ));
    assert!(matches!(second, Err(Error::AlreadyRunning)));
    // A response to a correlation nothing waits for is unknown while the node runs.
    assert_eq!(
        block_on(node.respond(Correlation::new(42), Response::success())),
        Err(Error::UnknownCorrelation(Correlation::new(42)))
    );
    stop(&*node);
    let _again = start(&engine, Arc::new(MemoryStorage::new()));
}

#[test]
fn invalid_identity_rejected() {
    let engine = engine();
    let engine_ref: &dyn Engine = &engine;
    let start_with = |identity: NodeIdentity, network: NetworkConfig| {
        block_on(engine_ref.start(
            identity,
            network,
            Attestation::Test,
            NodeRole::Bridge {
                aggregator: AGGREGATOR,
            },
            Arc::new(MemoryStorage::new()),
        ))
        .map(|_| ())
    };

    let mut wide_discriminator = identity();
    wide_discriminator.discriminator = 0x1000;
    let error = start_with(wide_discriminator, network()).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidIdentity { reason } if reason.contains("discriminator"))
    );

    let mut forbidden_passcode = identity();
    forbidden_passcode.passcode = Secret::new(12_345_678);
    match start_with(forbidden_passcode, network()).unwrap_err() {
        Error::InvalidIdentity { reason } => {
            assert!(reason.contains("passcode"), "{reason}");
            assert!(
                !reason.contains("12345678"),
                "the passcode leaked: {reason}"
            );
        }
        other => panic!("unexpected error {other:?}"),
    }

    let no_family = NetworkConfig {
        ipv4: false,
        ipv6: false,
        ..network()
    };
    let error = start_with(identity(), no_family).unwrap_err();
    assert!(matches!(&error, Error::InvalidIdentity { reason } if reason.contains("IPv4")));

    let missing_interface = NetworkConfig {
        interface: Some(String::from("no-such-interface")),
        ..network()
    };
    let error = start_with(identity(), missing_interface).unwrap_err();
    assert!(matches!(&error, Error::InvalidIdentity { reason } if reason.contains("interface")));

    // After a refused start the engine is free.
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    stop(&*node);
}
