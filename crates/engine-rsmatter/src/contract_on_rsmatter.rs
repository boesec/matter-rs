//! The engine contract on rs-matter, in process: the engine-agnostic suite of the facade
//! (`matter_rs_engine::contract`) run against a real node on a dedicated thread, plus what
//! only this engine can show: fabrics minted into the storage and read back, mDNS
//! announcements, the response timeout, the state file permissions, the log capture and the
//! refusals that come from rs-matter itself.
//!
//! Every test starts its own node on an ephemeral Matter port; the mDNS responders share port
//! 5353. The interface comes from the engine's heuristic, or from `MATTER_RS_TEST_INTERFACE`.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use matter_rs_engine::contract::{EngineUnderTest, block_on_within};
use matter_rs_engine::testing::block_on;
use matter_rs_engine::{
    Attestation, AttributeId, Capabilities, ClusterDescription, ClusterId, CommandId,
    CommissioningState, DeviceType, DeviceTypeId, EndpointDescription, EndpointId, Engine, Error,
    Event, FabricIndex, MemoryStorage, NetworkConfig, NodeIdentity, NodeRole, ProductId, Response,
    Running, Secret, Status, StopReason, Storage, Value, VendorId, Window,
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

/// The rs-matter engine as the engine-agnostic suite sees it: a short response timeout and a
/// small endpoint capacity, so that the capacity case exhausts it quickly.
struct RsMatterUnderTest;

impl EngineUnderTest for RsMatterUnderTest {
    fn engine(&self) -> Box<dyn Engine> {
        Box::new(engine().with_endpoint_capacity(8))
    }

    fn network(&self) -> NetworkConfig {
        network()
    }
}

matter_rs_engine::contract_tests!(RsMatterUnderTest);

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

fn next_event(node: &dyn Running) -> Event {
    block_on_within(node.next_event(), EVENT_TIMEOUT).expect("an event")
}

fn stop(node: &dyn Running) {
    block_on_within(node.stop(), EVENT_TIMEOUT).expect("the node stops");
}

/// The endpoint capacity is a setting of this engine, and its other limits are rs-matter's
/// compile-time constants.
#[test]
fn configured_capacity_is_reported() {
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
    // DNS wire format puts a length byte before every label, so the service name is matched
    // by its first label and the node by the discriminator in its TXT record.
    let is_announcement =
        |packet: &[u8]| contains(packet, b"_matterc") && contains(packet, b"D=2345");
    // Other tests of this process run their own nodes, which answer the same queries; every
    // packet that is waiting is read, not just the first one, so that their announcements
    // cannot crowd this node's out.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = false;
    let mut buffer = [0_u8; 2048];
    while Instant::now() < deadline && !seen {
        listener.send_to(&mdns_query(), group).unwrap();
        querier.send_to(&mdns_query(), direct).unwrap();
        for socket in [&listener, &querier] {
            while !seen && let Ok((len, _)) = socket.recv_from(&mut buffer) {
                seen = is_announcement(&buffer[..len]);
            }
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

/// A second window while one is open is rs-matter's refusal, reported as an engine error.
#[test]
fn second_window_while_open_is_refused() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    assert!(matches!(
        block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(60))),
        Err(Error::Engine(_))
    ));
    stop(&*node);
    assert_eq!(next_event(&*node), Event::Stopped(StopReason::Requested));
}

/// The running node's pairing information is rs-matter's own computation, and it matches
/// the offline computation of `pairing_info_for` and the known manual code of the test vector.
#[test]
fn pairing_info_matches_the_offline_computation() {
    let engine = engine();
    let node = start(&engine, Arc::new(MemoryStorage::new()));
    let info = block_on(node.pairing_info()).unwrap();
    assert_eq!(info.manual_code.expose(), MANUAL_CODE);
    assert_eq!(
        crate::pairing_info_for(&identity()).unwrap().qr_text,
        info.qr_text
    );
    stop(&*node);
}

/// A network configuration naming an interface the host does not have is refused before the
/// node starts, and the engine is free afterwards.
#[test]
fn missing_interface_rejected() {
    let engine = engine();
    let engine_ref: &dyn Engine = &engine;
    let missing_interface = NetworkConfig {
        interface: Some(String::from("no-such-interface")),
        ..network()
    };
    let error = block_on(engine_ref.start(
        identity(),
        missing_interface,
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::new(MemoryStorage::new()),
    ))
    .map(|_| ())
    .unwrap_err();
    assert!(matches!(&error, Error::InvalidIdentity { reason } if reason.contains("interface")));

    let node = start(&engine, Arc::new(MemoryStorage::new()));
    stop(&*node);
}
