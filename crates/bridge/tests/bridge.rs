//! The bridge against the fake engine: numbering, kinds, composed devices, Bridged Device Basic
//! Information, cluster semantics, the device API and isolation.
//!
//! Every test starts its own bridge on a fake engine. A product thread pulls commands with
//! `Bridge::next_command` and forwards them over a channel; the test plays the Matter client
//! with the fake engine's hooks.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use matter_rs_bridge::ids::{
    boolean_state, bridged_device_basic_information as bdbi, color_control as cc, identify,
    level_control as lc, on_off, temperature_measurement,
};
use matter_rs_bridge::numbering::{AGGREGATOR, MAP_KEY, NEXT_KEY};
use matter_rs_bridge::{
    Attribute, Bridge, CommandEvent, DeviceCommand, DeviceHandle, DeviceKey, DeviceKind,
    DeviceSpec, Error, InitialValues, Options, PartSpec, SemanticTag, Timer,
};
use matter_rs_engine::testing::{EmittedEvent, FakeEngine, block_on};
use matter_rs_engine::{
    Attestation, AttributeId, BoxFuture, ClusterDescription, ClusterId, CommandId,
    EndpointDescription, EndpointId, MemoryStorage, NetworkConfig, NodeIdentity, ProductId,
    Response, Secret, Status, Storage, Value, VendorId, bridged_device,
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
        unique_id: "bridge-test".into(),
        passcode: Secret::new(20_202_021),
        discriminator: 3840,
    }
}

fn key(name: &str) -> DeviceKey {
    DeviceKey::new(name).unwrap()
}

fn ep(id: u16) -> EndpointId {
    EndpointId::new(id)
}

fn fields(fields: &[(u64, Value)]) -> Value {
    Value::Struct(fields.to_vec())
}

/// A timer the test fires by hand.
#[derive(Clone, Default)]
struct ManualTimer {
    state: Arc<Mutex<(bool, Vec<Waker>)>>,
}

impl ManualTimer {
    fn fire(&self) {
        let wakers = {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            std::mem::take(&mut state.1)
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

impl Timer for ManualTimer {
    fn sleep(&self, _duration: Duration) -> BoxFuture<'static, ()> {
        let state = Arc::clone(&self.state);
        Box::pin(core::future::poll_fn(move |cx| {
            let mut state = state.lock().unwrap();
            if state.0 {
                Poll::Ready(())
            } else {
                state.1.push(cx.waker().clone());
                Poll::Pending
            }
        }))
    }
}

/// A bridge on a fake engine with a product thread.
struct Harness {
    fake: FakeEngine,
    bridge: Arc<Bridge>,
    storage: Arc<MemoryStorage>,
    commands: Receiver<CommandEvent>,
    product: Option<JoinHandle<()>>,
}

impl Harness {
    fn start() -> Self {
        Self::start_on(Arc::new(MemoryStorage::new()), Options::default())
    }

    fn start_on(storage: Arc<MemoryStorage>, options: Options) -> Self {
        let fake = FakeEngine::new();
        let bridge = block_on(Bridge::start_with(
            Box::new(fake.clone()),
            identity(),
            NetworkConfig::default(),
            Attestation::Test,
            Arc::clone(&storage) as Arc<dyn Storage>,
            options,
        ))
        .expect("the bridge starts");
        let bridge = Arc::new(bridge);
        let (sender, commands) = channel();
        let product = {
            let bridge = Arc::clone(&bridge);
            thread::spawn(move || {
                while let Ok(command) = block_on(bridge.next_command()) {
                    if sender.send(command).is_err() {
                        break;
                    }
                }
            })
        };
        Self {
            fake,
            bridge,
            storage,
            commands,
            product: Some(product),
        }
    }

    fn add(&self, spec: DeviceSpec, initial: InitialValues) -> DeviceHandle {
        block_on(self.bridge.add_device(spec, initial)).expect("the device is added")
    }

    fn on_off_light(&self, name: &str, on: bool) -> DeviceHandle {
        self.add(
            DeviceSpec::single(key(name), name, DeviceKind::OnOffLight),
            InitialValues::new().value(Attribute::OnOff, on),
        )
    }

    fn dimmable_light(&self, name: &str, on: bool, level: u8) -> DeviceHandle {
        self.add(
            DeviceSpec::single(key(name), name, DeviceKind::DimmableLight),
            InitialValues::new()
                .value(Attribute::OnOff, on)
                .value(Attribute::CurrentLevel, Value::U8(level)),
        )
    }

    fn invoke(
        &self,
        endpoint: u16,
        cluster: ClusterId,
        command: CommandId,
        payload: Value,
    ) -> Status {
        block_on(self.fake.invoke(ep(endpoint), cluster, command, payload))
            .expect("the node runs")
            .status
    }

    fn write(
        &self,
        endpoint: u16,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) -> Status {
        block_on(self.fake.write(ep(endpoint), cluster, attribute, value))
            .expect("the node runs")
            .status
    }

    fn read(&self, endpoint: u16, attribute: Attribute) -> Option<Value> {
        let (cluster, id) = attribute.path();
        self.fake.read(ep(endpoint), cluster, id)
    }

    fn next_command(&self) -> CommandEvent {
        self.commands
            .recv_timeout(Duration::from_secs(5))
            .expect("a command reaches the adapter")
    }

    fn no_command(&self) {
        assert!(
            self.commands
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "no command was expected"
        );
    }

    fn endpoint(&self, id: u16) -> EndpointDescription {
        self.fake
            .endpoints()
            .into_iter()
            .find(|e| e.id == ep(id))
            .unwrap_or_else(|| panic!("endpoint {id} exists"))
    }

    fn stop(mut self) {
        block_on(self.bridge.stop()).unwrap();
        if let Some(product) = self.product.take() {
            product.join().unwrap();
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.fake.is_running() {
            let _ = block_on(self.bridge.stop());
        }
        if let Some(product) = self.product.take() {
            let _ = product.join();
        }
    }
}

fn required_values(kind: DeviceKind) -> InitialValues {
    let mut initial = InitialValues::new();
    for attribute in kind.required_attributes() {
        let value = match attribute {
            Attribute::OnOff => Value::Bool(false),
            Attribute::CurrentLevel => Value::U8(100),
            Attribute::ColorTemperatureMireds => Value::U16(300),
            Attribute::ColorTempPhysicalMinMireds => Value::U16(153),
            Attribute::ColorTempPhysicalMaxMireds => Value::U16(500),
            Attribute::StateValue => Value::Bool(true),
            Attribute::Temperature => Value::I16(2150),
            Attribute::Humidity => Value::U16(4500),
            Attribute::Pressure => Value::I16(1013),
            Attribute::Illuminance => Value::U16(20000),
            Attribute::Occupancy => Value::U8(0),
            other => panic!("no test value for required attribute {other:?}"),
        };
        initial = initial.value(attribute, value);
    }
    initial
}

fn composed_strip() -> (DeviceSpec, InitialValues) {
    let spec = DeviceSpec {
        key: key("strip"),
        name: "Power strip".into(),
        parts: vec![
            PartSpec::new(DeviceKind::OnOffPlugInUnit),
            PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Lamp"),
            PartSpec::new(DeviceKind::TemperatureSensor),
        ],
    };
    let initial = InitialValues::new()
        .with(0, Attribute::OnOff, false)
        .with(1, Attribute::OnOff, true)
        .with(2, Attribute::Temperature, Value::I16(2000));
    (spec, initial)
}

// --- numbering -------------------------------------------------------------------------------

#[test]
fn numbering_is_stable_across_restart() {
    let storage = Arc::new(MemoryStorage::new());
    let first = Harness::start_on(Arc::clone(&storage), Options::default());
    let a = first.on_off_light("a", false);
    let (spec, initial) = composed_strip();
    let strip = first.add(spec, initial);
    let c = first.on_off_light("c", true);
    let numbers = |handle: &DeviceHandle| -> Vec<EndpointId> {
        handle
            .endpoints()
            .unwrap()
            .iter()
            .map(|e| e.endpoint)
            .collect()
    };
    assert_eq!(numbers(&a), vec![ep(2)]);
    assert_eq!(numbers(&strip), vec![ep(3), ep(4), ep(5), ep(6)]);
    assert_eq!(numbers(&c), vec![ep(7)]);
    block_on(first.bridge.remove_device(&key("a"))).unwrap();
    first.stop();

    // A new engine on the same storage: the devices come back in another order and get the
    // same numbers; a new device continues after the last assigned number.
    let second = Harness::start_on(Arc::clone(&storage), Options::default());
    let c = second.on_off_light("c", true);
    let (spec, initial) = composed_strip();
    let strip = second.add(spec, initial);
    let a = second.on_off_light("a", false);
    assert_eq!(numbers(&c), vec![ep(7)]);
    assert_eq!(numbers(&strip), vec![ep(3), ep(4), ep(5), ep(6)]);
    assert_eq!(numbers(&a), vec![ep(2)]);
    let d = second.on_off_light("d", false);
    assert_eq!(numbers(&d), vec![ep(8)]);
    // The aggregator is endpoint 1, which the engine created.
    assert_eq!(AGGREGATOR, ep(1));
    assert!(second.fake.endpoints().iter().all(|e| e.id != AGGREGATOR));
    second.stop();
}

#[test]
fn numbering_reserves_removed_devices() {
    let harness = Harness::start();
    let a = harness.on_off_light("a", false);
    assert_eq!(a.endpoints().unwrap()[0].endpoint, ep(2));
    block_on(harness.bridge.remove_device(&key("a"))).unwrap();
    assert!(harness.fake.endpoints().is_empty());
    assert_eq!(harness.bridge.reserved_endpoints(), vec![ep(2)]);
    // A new device does not take the reserved number.
    let b = harness.on_off_light("b", false);
    assert_eq!(b.endpoints().unwrap()[0].endpoint, ep(3));
    // The removed device comes back with its number.
    let a = harness.on_off_light("a", false);
    assert_eq!(a.endpoints().unwrap()[0].endpoint, ep(2));
    // A handle of a removed device fails, the re-added one works.
    block_on(harness.bridge.remove_device(&key("b"))).unwrap();
    assert_eq!(
        block_on(b.set(0, Attribute::OnOff, true)),
        Err(Error::UnknownDevice(key("b")))
    );
    assert_eq!(
        block_on(harness.bridge.remove_device(&key("b"))),
        Err(Error::UnknownDevice(key("b")))
    );
    harness.stop();
}

#[test]
fn numbering_forget_releases() {
    let harness = Harness::start();
    harness.on_off_light("a", false);
    let (spec, initial) = composed_strip();
    harness.add(spec, initial);
    assert_eq!(harness.bridge.reserved_endpoints().len(), 5);
    // Forgetting an added device removes it and releases its numbers.
    block_on(harness.bridge.forget_device(&key("strip"))).unwrap();
    assert_eq!(harness.bridge.reserved_endpoints(), vec![ep(2)]);
    assert_eq!(harness.fake.endpoints().len(), 1);
    assert_eq!(harness.bridge.devices(), vec![key("a")]);
    // Forgetting a removed device releases its numbers too; forgetting an unknown one is fine.
    block_on(harness.bridge.remove_device(&key("a"))).unwrap();
    block_on(harness.bridge.forget_device(&key("a"))).unwrap();
    block_on(harness.bridge.forget_device(&key("never"))).unwrap();
    assert!(harness.bridge.reserved_endpoints().is_empty());
    // The counter does not go back: a forgotten key is a new device with a new number.
    let a = harness.on_off_light("a", false);
    assert_eq!(a.endpoints().unwrap()[0].endpoint, ep(7));
    harness.stop();
}

#[test]
fn numbering_lives_under_bridge_endpoints() {
    let harness = Harness::start();
    harness.on_off_light("a", false);
    let keys = harness.storage.keys();
    assert!(keys.contains(&MAP_KEY.to_owned()), "{keys:?}");
    assert!(keys.contains(&NEXT_KEY.to_owned()), "{keys:?}");
    assert!(MAP_KEY.starts_with("bridge/endpoints/"));
    assert!(NEXT_KEY.starts_with("bridge/endpoints/"));
    assert!(
        keys.iter()
            .all(|k| k.starts_with("bridge/") || k.starts_with("engine/")),
        "{keys:?}"
    );
    harness.stop();
}

// --- kinds -----------------------------------------------------------------------------------

#[test]
fn kinds_describe_their_endpoints() {
    let expected_clusters: &[(DeviceKind, &[ClusterId])] = &[
        (
            DeviceKind::OnOffLight,
            &[identify::CLUSTER, on_off::CLUSTER],
        ),
        (
            DeviceKind::DimmableLight,
            &[identify::CLUSTER, on_off::CLUSTER, lc::CLUSTER],
        ),
        (
            DeviceKind::ColorTemperatureLight,
            &[identify::CLUSTER, on_off::CLUSTER, lc::CLUSTER, cc::CLUSTER],
        ),
        (
            DeviceKind::ExtendedColorLight,
            &[identify::CLUSTER, on_off::CLUSTER, lc::CLUSTER, cc::CLUSTER],
        ),
        (
            DeviceKind::OnOffPlugInUnit,
            &[identify::CLUSTER, on_off::CLUSTER],
        ),
        (
            DeviceKind::ContactSensor,
            &[identify::CLUSTER, boolean_state::CLUSTER],
        ),
        (
            DeviceKind::TemperatureSensor,
            &[identify::CLUSTER, temperature_measurement::CLUSTER],
        ),
        (
            DeviceKind::HumiditySensor,
            &[identify::CLUSTER, ClusterId::new(0x0405)],
        ),
    ];
    let harness = Harness::start();
    for (index, (kind, clusters)) in expected_clusters.iter().enumerate() {
        let name = kind.name();
        let handle = harness.add(
            DeviceSpec::single(key(name), name, *kind),
            required_values(*kind),
        );
        let id = 2 + u16::try_from(index).unwrap();
        assert_eq!(handle.endpoints().unwrap()[0].endpoint, ep(id), "{name}");
        let description = harness.endpoint(id);
        assert_eq!(
            description.device_types,
            vec![
                kind.device_type(),
                matter_rs_engine::DeviceType {
                    id: matter_rs_bridge::ids::device_type::BRIDGED_NODE.0,
                    revision: matter_rs_bridge::ids::device_type::BRIDGED_NODE.1,
                }
            ],
            "{name}"
        );
        let mut listed: Vec<ClusterId> = description.clusters.iter().map(|c| c.id).collect();
        let mut expected: Vec<ClusterId> = clusters.to_vec();
        expected.push(bdbi::CLUSTER);
        listed.sort();
        expected.sort();
        assert_eq!(listed, expected, "{name}");
        // Every listed attribute is readable: the engine never invents a value.
        for cluster in &description.clusters {
            assert!(!cluster.attributes.is_empty(), "{name}: {}", cluster.id);
            for attribute in &cluster.attributes {
                assert!(
                    harness.fake.read(ep(id), cluster.id, *attribute).is_some(),
                    "{name}: {}/{attribute} has no value",
                    cluster.id
                );
            }
        }
        // The Bridged Device Basic Information cluster carries label, reachability and id.
        let bdbi = description.cluster(bdbi::CLUSTER).unwrap();
        assert!(bdbi.has_attribute(bdbi::NODE_LABEL), "{name}");
        assert!(bdbi.has_attribute(bdbi::REACHABLE), "{name}");
        assert!(bdbi.has_attribute(bdbi::UNIQUE_ID), "{name}");
        assert!(bdbi.has_event(bridged_device::REACHABLE_CHANGED), "{name}");
        assert_eq!(
            harness.fake.read(ep(id), bdbi::CLUSTER, bdbi::NODE_LABEL),
            Some(Value::from(name)),
            "{name}"
        );
    }
    assert_eq!(DeviceKind::ALL.len(), expected_clusters.len());
    harness.stop();
}

#[test]
fn kinds_set_their_feature_maps_and_defaults() {
    let harness = Harness::start();
    let name = DeviceKind::ExtendedColorLight.name();
    harness.add(
        DeviceSpec::single(key(name), name, DeviceKind::ExtendedColorLight),
        required_values(DeviceKind::ExtendedColorLight),
    );
    let description = harness.endpoint(2);
    assert_eq!(
        description.cluster(on_off::CLUSTER).unwrap().feature_map,
        on_off::FEATURE_LIGHTING
    );
    assert_eq!(description.cluster(lc::CLUSTER).unwrap().feature_map, 0b11);
    assert_eq!(description.cluster(cc::CLUSTER).unwrap().feature_map, 0x19);
    assert_eq!(harness.read(2, Attribute::ColorMode), Some(Value::U8(2)));
    assert_eq!(harness.read(2, Attribute::OnLevel), Some(Value::Null));
    assert_eq!(harness.read(2, Attribute::StartUpOnOff), Some(Value::Null));
    assert_eq!(
        harness.fake.read(ep(2), lc::CLUSTER, lc::MIN_LEVEL),
        Some(Value::U8(1))
    );
    assert_eq!(
        harness.fake.read(
            ep(2),
            cc::CLUSTER,
            cc::COUPLE_COLOR_TEMP_TO_LEVEL_MIN_MIREDS
        ),
        Some(Value::U16(153))
    );
    assert_eq!(
        harness
            .fake
            .read(ep(2), cc::CLUSTER, cc::COLOR_CAPABILITIES),
        Some(Value::U16(0x19))
    );
    harness.stop();
}

#[test]
fn missing_initial_value_is_rejected() {
    let harness = Harness::start();
    for kind in DeviceKind::ALL {
        for missing in kind.required_attributes() {
            let full = required_values(kind);
            let mut initial = InitialValues::new();
            for (part, attribute, value) in full.iter() {
                if attribute != missing {
                    initial = initial.with(part, attribute, value.clone());
                }
            }
            let error = block_on(
                harness
                    .bridge
                    .add_device(DeviceSpec::single(key("dev"), "Device", kind), initial),
            )
            .unwrap_err();
            assert_eq!(
                error,
                Error::MissingInitialValue {
                    key: key("dev"),
                    part: 0,
                    attribute: missing,
                },
                "{kind:?}"
            );
            assert!(harness.fake.endpoints().is_empty(), "{kind:?}");
            assert!(harness.bridge.devices().is_empty(), "{kind:?}");
        }
    }
    // A value of the wrong type names the attribute as well.
    let error = block_on(harness.bridge.add_device(
        DeviceSpec::single(key("dev"), "Device", DeviceKind::OnOffLight),
        InitialValues::new().value(Attribute::OnOff, Value::U8(1)),
    ))
    .unwrap_err();
    assert!(
        matches!(
            error,
            Error::InvalidValue {
                attribute: Attribute::OnOff,
                ..
            }
        ),
        "{error:?}"
    );
    harness.stop();
}

// --- composed devices ------------------------------------------------------------------------

#[test]
fn composed_device_endpoints() {
    let harness = Harness::start();
    let (spec, initial) = composed_strip();
    let strip = harness.add(spec, initial);
    let endpoints = strip.endpoints().unwrap();
    assert_eq!(endpoints.len(), 4);
    let parent = harness.endpoint(2);
    assert_eq!(parent.parent, None);
    assert_eq!(parent.device_types.len(), 1);
    assert_eq!(
        parent.device_types[0].id,
        matter_rs_bridge::ids::device_type::BRIDGED_NODE.0
    );
    assert_eq!(parent.clusters.len(), 1);
    assert_eq!(parent.clusters[0].id, bdbi::CLUSTER);
    assert_eq!(
        harness.fake.read(ep(2), bdbi::CLUSTER, bdbi::NODE_LABEL),
        Some(Value::from("Power strip"))
    );
    for (id, kind) in [
        (3, DeviceKind::OnOffPlugInUnit),
        (4, DeviceKind::OnOffPlugInUnit),
        (5, DeviceKind::TemperatureSensor),
    ] {
        let child = harness.endpoint(id);
        assert_eq!(child.parent, Some(ep(2)));
        assert_eq!(child.device_types, vec![kind.device_type()]);
        assert!(child.cluster(bdbi::CLUSTER).is_none());
    }
    assert_eq!(harness.read(3, Attribute::OnOff), Some(Value::Bool(false)));
    assert_eq!(harness.read(4, Attribute::OnOff), Some(Value::Bool(true)));
    assert_eq!(
        harness.read(5, Attribute::Temperature),
        Some(Value::I16(2000))
    );
    // Parts are addressed by index.
    block_on(strip.set(1, Attribute::OnOff, false)).unwrap();
    assert_eq!(harness.read(4, Attribute::OnOff), Some(Value::Bool(false)));
    assert_eq!(
        block_on(strip.set(3, Attribute::OnOff, false)),
        Err(Error::UnknownPart {
            key: key("strip"),
            part: 3
        })
    );
    harness.stop();
}

#[test]
fn composed_siblings_get_tags() {
    let harness = Harness::start();
    let (spec, initial) = composed_strip();
    let strip = harness.add(spec, initial);
    let endpoints = strip.endpoints().unwrap();
    assert!(endpoints[0].tags.is_empty());
    assert_eq!(endpoints[1].tags, vec![SemanticTag::standard(0x07, 1)]);
    assert_eq!(
        endpoints[2].tags,
        vec![SemanticTag::standard(0x07, 2).with_label("Lamp")]
    );
    assert!(endpoints[3].tags.is_empty());
    harness.stop();
}

#[test]
fn remove_cascades() {
    let harness = Harness::start();
    let (spec, initial) = composed_strip();
    harness.add(spec, initial);
    harness.on_off_light("other", true);
    assert_eq!(harness.fake.endpoints().len(), 5);
    block_on(harness.bridge.remove_device(&key("strip"))).unwrap();
    let left: Vec<EndpointId> = harness.fake.endpoints().iter().map(|e| e.id).collect();
    assert_eq!(left, vec![ep(6)]);
    assert_eq!(harness.bridge.devices(), vec![key("other")]);
    harness.stop();
}

// --- Bridged Device Basic Information --------------------------------------------------------

#[test]
fn bdbi_unique_id_is_stable() {
    let storage = Arc::new(MemoryStorage::new());
    let first = Harness::start_on(Arc::clone(&storage), Options::default());
    first.on_off_light("lamp-1", false);
    first.on_off_light("lamp-2", false);
    let read = |h: &Harness, id: u16| h.fake.read(ep(id), bdbi::CLUSTER, bdbi::UNIQUE_ID);
    let one = read(&first, 2).unwrap();
    let two = read(&first, 3).unwrap();
    assert_ne!(one, two);
    assert_eq!(one, Value::from("lamp-1"));
    first.stop();

    let second = Harness::start_on(storage, Options::default());
    second.on_off_light("lamp-2", true);
    second.on_off_light("lamp-1", true);
    assert_eq!(read(&second, 2), Some(one));
    assert_eq!(read(&second, 3), Some(two));
    // A key that does not fit the attribute becomes a stable hash of at most 32 characters.
    second.add(
        DeviceSpec::single(
            key("a key with spaces that is far too long for the attribute"),
            "Long",
            DeviceKind::OnOffLight,
        ),
        InitialValues::new().value(Attribute::OnOff, false),
    );
    let Some(Value::Utf8(hashed)) = read(&second, 4) else {
        panic!("a unique id");
    };
    assert!(hashed.len() <= 32);
    assert_eq!(
        hashed,
        matter_rs_bridge::unique_id(&key(
            "a key with spaces that is far too long for the attribute"
        ))
    );
    second.stop();
}

#[test]
fn node_label_write_is_rename() {
    let storage = Arc::new(MemoryStorage::new());
    let harness = Harness::start_on(Arc::clone(&storage), Options::default());
    let (spec, initial) = composed_strip();
    harness.add(spec, initial);
    let status = harness.write(
        2,
        bdbi::CLUSTER,
        bdbi::NODE_LABEL,
        Value::from("Kitchen strip"),
    );
    assert_eq!(status, Status::Success);
    assert_eq!(
        harness.fake.read(ep(2), bdbi::CLUSTER, bdbi::NODE_LABEL),
        Some(Value::from("Kitchen strip"))
    );
    let command = harness.next_command();
    assert_eq!(command.key, key("strip"));
    assert_eq!(command.part, None);
    assert_eq!(
        command.command,
        DeviceCommand::Rename("Kitchen strip".into())
    );
    // Too long is a constraint error and changes nothing.
    let status = harness.write(
        2,
        bdbi::CLUSTER,
        bdbi::NODE_LABEL,
        Value::from("x".repeat(33).as_str()),
    );
    assert_eq!(status, Status::ConstraintError);
    harness.no_command();
    // The label is stored under the bridge's attribute keys and restored after a restart.
    assert!(
        storage
            .keys()
            .contains(&"bridge/attributes/strip".to_owned())
    );
    harness.stop();
    let second = Harness::start_on(storage, Options::default());
    let (spec, initial) = composed_strip();
    second.add(spec, initial);
    assert_eq!(
        second.fake.read(ep(2), bdbi::CLUSTER, bdbi::NODE_LABEL),
        Some(Value::from("Kitchen strip"))
    );
    second.stop();
}

#[test]
fn reachable_change_is_event() {
    let harness = Harness::start();
    let light = harness.on_off_light("lamp", false);
    assert!(light.is_reachable());
    assert_eq!(
        harness.fake.read(ep(2), bdbi::CLUSTER, bdbi::REACHABLE),
        Some(Value::Bool(true))
    );
    block_on(light.set_reachable(false)).unwrap();
    assert!(!light.is_reachable());
    assert_eq!(
        harness.fake.read(ep(2), bdbi::CLUSTER, bdbi::REACHABLE),
        Some(Value::Bool(false))
    );
    assert_eq!(
        harness.fake.event_log(),
        vec![EmittedEvent {
            endpoint: ep(2),
            cluster: bdbi::CLUSTER,
            event: bridged_device::REACHABLE_CHANGED,
            payload: Value::Struct(vec![(
                bridged_device::REACHABLE_NEW_VALUE_TAG,
                Value::Bool(false)
            )]),
        }]
    );
    // A device may start unreachable; the composed device's parent carries the flag.
    let (spec, initial) = composed_strip();
    let strip = harness.add(spec, initial.reachable(false));
    assert!(!strip.is_reachable());
    assert_eq!(
        harness.fake.read(ep(3), bdbi::CLUSTER, bdbi::REACHABLE),
        Some(Value::Bool(false))
    );
    block_on(strip.set_reachable(true)).unwrap();
    assert_eq!(harness.fake.event_log().len(), 2);
    assert_eq!(harness.fake.event_log()[1].endpoint, ep(3));
    // A controller cannot write `Reachable`: the engine's metadata or the bridge refuse it.
    let status = harness.write(2, bdbi::CLUSTER, bdbi::REACHABLE, Value::Bool(true));
    assert_eq!(status, Status::UnsupportedWrite);
    harness.stop();
}

// --- semantics -------------------------------------------------------------------------------

#[test]
fn light_commands_set_target_values() {
    /// One row: the command a client sends and what the bridge must do.
    struct Case {
        name: &'static str,
        cluster: ClusterId,
        command: CommandId,
        payload: Value,
        expected_values: Vec<(Attribute, Value)>,
        expected_command: DeviceCommand,
    }
    let cases = vec![
        Case {
            name: "On",
            cluster: on_off::CLUSTER,
            command: on_off::ON,
            payload: Value::Null,
            expected_values: vec![(Attribute::OnOff, Value::Bool(true))],
            expected_command: DeviceCommand::SetOnOff { on: true },
        },
        Case {
            name: "Off",
            cluster: on_off::CLUSTER,
            command: on_off::OFF,
            payload: Value::Null,
            expected_values: vec![(Attribute::OnOff, Value::Bool(false))],
            expected_command: DeviceCommand::SetOnOff { on: false },
        },
        Case {
            name: "Toggle",
            cluster: on_off::CLUSTER,
            command: on_off::TOGGLE,
            payload: Value::Null,
            expected_values: vec![(Attribute::OnOff, Value::Bool(true))],
            expected_command: DeviceCommand::SetOnOff { on: true },
        },
        Case {
            name: "OffWithEffect",
            cluster: on_off::CLUSTER,
            command: on_off::OFF_WITH_EFFECT,
            payload: fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
            expected_values: vec![(Attribute::OnOff, Value::Bool(false))],
            expected_command: DeviceCommand::SetOnOff { on: false },
        },
        Case {
            name: "OnWithRecallGlobalScene",
            cluster: on_off::CLUSTER,
            command: on_off::ON_WITH_RECALL_GLOBAL_SCENE,
            payload: Value::Null,
            expected_values: vec![(Attribute::OnOff, Value::Bool(true))],
            expected_command: DeviceCommand::SetOnOff { on: true },
        },
        Case {
            name: "OnWithTimedOff",
            cluster: on_off::CLUSTER,
            command: on_off::ON_WITH_TIMED_OFF,
            payload: fields(&[(0, Value::U8(0)), (1, Value::U16(300)), (2, Value::U16(0))]),
            expected_values: vec![
                (Attribute::OnOff, Value::Bool(true)),
                (Attribute::OnTime, Value::U16(300)),
            ],
            expected_command: DeviceCommand::SetOnOff { on: true },
        },
        Case {
            name: "MoveToLevel",
            cluster: lc::CLUSTER,
            command: lc::MOVE_TO_LEVEL,
            payload: fields(&[
                (0, Value::U8(200)),
                (1, Value::U16(10)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
            expected_values: vec![(Attribute::CurrentLevel, Value::U8(200))],
            expected_command: DeviceCommand::SetLevel {
                level: 200,
                transition: Some(Duration::from_secs(1)),
                on_off: None,
            },
        },
        Case {
            name: "MoveToLevelWithOnOff",
            cluster: lc::CLUSTER,
            command: lc::MOVE_TO_LEVEL_WITH_ON_OFF,
            payload: fields(&[
                (0, Value::U8(1)),
                (1, Value::Null),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
            expected_values: vec![
                (Attribute::CurrentLevel, Value::U8(1)),
                (Attribute::OnOff, Value::Bool(false)),
            ],
            expected_command: DeviceCommand::SetLevel {
                level: 1,
                transition: None,
                on_off: Some(false),
            },
        },
        Case {
            name: "Move",
            cluster: lc::CLUSTER,
            command: lc::MOVE,
            payload: fields(&[
                (0, Value::U8(0)),
                (1, Value::U8(154)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
            expected_values: vec![(Attribute::CurrentLevel, Value::U8(254))],
            expected_command: DeviceCommand::SetLevel {
                level: 254,
                transition: Some(Duration::from_secs(1)),
                on_off: None,
            },
        },
        Case {
            name: "Step",
            cluster: lc::CLUSTER,
            command: lc::STEP,
            payload: fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(50)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
            expected_values: vec![(Attribute::CurrentLevel, Value::U8(50))],
            expected_command: DeviceCommand::SetLevel {
                level: 50,
                transition: Some(Duration::ZERO),
                on_off: None,
            },
        },
        Case {
            name: "Stop",
            cluster: lc::CLUSTER,
            command: lc::STOP,
            payload: fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
            expected_values: vec![(Attribute::CurrentLevel, Value::U8(100))],
            expected_command: DeviceCommand::SetLevel {
                level: 100,
                transition: Some(Duration::ZERO),
                on_off: None,
            },
        },
        Case {
            name: "MoveToColorTemperature",
            cluster: cc::CLUSTER,
            command: cc::MOVE_TO_COLOR_TEMPERATURE,
            payload: fields(&[
                (0, Value::U16(250)),
                (1, Value::U16(5)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
            expected_values: vec![
                (Attribute::ColorTemperatureMireds, Value::U16(250)),
                (Attribute::ColorMode, Value::U8(2)),
            ],
            expected_command: DeviceCommand::SetColorTemperature {
                mireds: 250,
                transition: Some(Duration::from_millis(500)),
            },
        },
        Case {
            name: "MoveToHue",
            cluster: cc::CLUSTER,
            command: cc::MOVE_TO_HUE,
            payload: fields(&[
                (0, Value::U8(10)),
                (1, Value::U8(0)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
            expected_values: vec![
                (Attribute::CurrentHue, Value::U8(10)),
                (Attribute::ColorMode, Value::U8(0)),
            ],
            expected_command: DeviceCommand::SetHue {
                hue: 10,
                transition: Some(Duration::ZERO),
            },
        },
        Case {
            name: "MoveToSaturation",
            cluster: cc::CLUSTER,
            command: cc::MOVE_TO_SATURATION,
            payload: fields(&[
                (0, Value::U8(20)),
                (1, Value::U16(0)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
            expected_values: vec![
                (Attribute::CurrentSaturation, Value::U8(20)),
                (Attribute::ColorMode, Value::U8(0)),
            ],
            expected_command: DeviceCommand::SetSaturation {
                saturation: 20,
                transition: Some(Duration::ZERO),
            },
        },
        Case {
            name: "MoveToColor",
            cluster: cc::CLUSTER,
            command: cc::MOVE_TO_COLOR,
            payload: fields(&[
                (0, Value::U16(1000)),
                (1, Value::U16(2000)),
                (2, Value::U16(20)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
            expected_values: vec![
                (Attribute::CurrentX, Value::U16(1000)),
                (Attribute::CurrentY, Value::U16(2000)),
                (Attribute::ColorMode, Value::U8(1)),
            ],
            expected_command: DeviceCommand::SetColorXy {
                x: 1000,
                y: 2000,
                transition: Some(Duration::from_secs(2)),
            },
        },
        Case {
            name: "Identify",
            cluster: identify::CLUSTER,
            command: identify::IDENTIFY,
            payload: fields(&[(0, Value::U16(30))]),
            expected_values: vec![(Attribute::IdentifyTime, Value::U16(30))],
            expected_command: DeviceCommand::Identify {
                duration: Duration::from_secs(30),
            },
        },
    ];
    let harness = Harness::start();
    for (index, case) in cases.into_iter().enumerate() {
        // A fresh extended colour light per row: off (so `Toggle` turns it on), level 100.
        let name = format!("light-{index}");
        let initial = InitialValues::new()
            .value(Attribute::OnOff, false)
            .value(Attribute::CurrentLevel, Value::U8(100))
            .value(Attribute::ColorTemperatureMireds, Value::U16(300))
            .value(Attribute::ColorTempPhysicalMinMireds, Value::U16(153))
            .value(Attribute::ColorTempPhysicalMaxMireds, Value::U16(500))
            // Lights are off in this table, so the colour and level commands without an
            // On/Off effect need `ExecuteIfOff`.
            .value(Attribute::LevelOptions, Value::U8(1))
            .value(Attribute::ColorOptions, Value::U8(1));
        let light = harness.add(
            DeviceSpec::single(key(&name), &name, DeviceKind::ExtendedColorLight),
            initial,
        );
        let id = light.endpoints().unwrap()[0].endpoint.raw();
        let status = harness.invoke(id, case.cluster, case.command, case.payload);
        assert_eq!(status, Status::Success, "{}", case.name);
        for (attribute, value) in &case.expected_values {
            assert_eq!(
                harness.read(id, *attribute).as_ref(),
                Some(value),
                "{}: {attribute:?}",
                case.name
            );
            assert_eq!(
                light.value(0, *attribute).as_ref(),
                Some(value),
                "{}: cache of {attribute:?}",
                case.name
            );
        }
        let command = harness.next_command();
        assert_eq!(command.key, key(&name), "{}", case.name);
        assert_eq!(command.part, Some(0), "{}", case.name);
        assert_eq!(command.command, case.expected_command, "{}", case.name);
    }
    harness.no_command();
    harness.stop();
}

#[test]
fn on_level_is_applied_by_on() {
    let harness = Harness::start();
    harness.dimmable_light("lamp", false, 10);
    assert_eq!(
        harness.write(2, lc::CLUSTER, lc::ON_LEVEL, Value::U8(200)),
        Status::Success
    );
    assert_eq!(
        harness.next_command().command,
        DeviceCommand::Written {
            attribute: Attribute::OnLevel,
            value: Value::U8(200)
        }
    );
    assert_eq!(
        harness.invoke(2, on_off::CLUSTER, on_off::ON, Value::Null),
        Status::Success
    );
    assert_eq!(
        harness.read(2, Attribute::CurrentLevel),
        Some(Value::U8(200))
    );
    assert_eq!(
        harness.next_command().command,
        DeviceCommand::SetLevel {
            level: 200,
            transition: None,
            on_off: Some(true)
        }
    );
    harness.stop();
}

#[test]
fn writable_attributes_persist() {
    let storage = Arc::new(MemoryStorage::new());
    let harness = Harness::start_on(Arc::clone(&storage), Options::default());
    harness.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    let writes = [
        (lc::CLUSTER, lc::ON_LEVEL, Value::U8(77), Attribute::OnLevel),
        (
            on_off::CLUSTER,
            on_off::START_UP_ON_OFF,
            Value::U8(1),
            Attribute::StartUpOnOff,
        ),
        (
            lc::CLUSTER,
            lc::START_UP_CURRENT_LEVEL,
            Value::U8(50),
            Attribute::StartUpCurrentLevel,
        ),
        (
            lc::CLUSTER,
            lc::OPTIONS,
            Value::U8(1),
            Attribute::LevelOptions,
        ),
        (
            cc::CLUSTER,
            cc::OPTIONS,
            Value::U8(1),
            Attribute::ColorOptions,
        ),
        (
            cc::CLUSTER,
            cc::START_UP_COLOR_TEMPERATURE_MIREDS,
            Value::U16(400),
            Attribute::StartUpColorTemperatureMireds,
        ),
    ];
    for (cluster, attribute, value, typed) in &writes {
        assert_eq!(
            harness.write(2, *cluster, *attribute, value.clone()),
            Status::Success,
            "{typed:?}"
        );
        assert_eq!(harness.read(2, *typed), Some(value.clone()), "{typed:?}");
        assert_eq!(
            harness.next_command().command,
            DeviceCommand::Written {
                attribute: *typed,
                value: value.clone()
            }
        );
    }
    // A write outside the constraint is refused and leaves the value.
    assert_eq!(
        harness.write(2, on_off::CLUSTER, on_off::START_UP_ON_OFF, Value::U8(9)),
        Status::ConstraintError
    );
    assert_eq!(harness.read(2, Attribute::StartUpOnOff), Some(Value::U8(1)));
    harness.no_command();
    assert!(
        storage
            .keys()
            .contains(&"bridge/attributes/lamp".to_owned())
    );
    harness.stop();

    // After a restart the written values are back; the start-up ones take effect.
    let second = Harness::start_on(Arc::clone(&storage), Options::default());
    second.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    for (_, _, value, typed) in &writes {
        assert_eq!(second.read(2, *typed), Some(value.clone()), "{typed:?}");
    }
    // Forgetting the device deletes its values.
    block_on(second.bridge.forget_device(&key("lamp"))).unwrap();
    assert!(
        !storage
            .keys()
            .contains(&"bridge/attributes/lamp".to_owned())
    );
    second.stop();
}

#[test]
fn startup_values_never_invented() {
    let storage = Arc::new(MemoryStorage::new());
    // Without persisted start-up values the adapter's initial values stand and no command is
    // sent.
    let harness = Harness::start_on(Arc::clone(&storage), Options::default());
    harness.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    assert_eq!(harness.read(2, Attribute::OnOff), Some(Value::Bool(false)));
    assert_eq!(
        harness.read(2, Attribute::CurrentLevel),
        Some(Value::U8(100))
    );
    assert_eq!(
        harness.read(2, Attribute::ColorTemperatureMireds),
        Some(Value::U16(300))
    );
    harness.no_command();
    // A controller sets the start-up behaviour.
    assert_eq!(
        harness.write(
            2,
            on_off::CLUSTER,
            on_off::START_UP_ON_OFF,
            Value::U8(on_off::START_UP_ON)
        ),
        Status::Success
    );
    assert_eq!(
        harness.write(2, lc::CLUSTER, lc::START_UP_CURRENT_LEVEL, Value::U8(0)),
        Status::Success
    );
    assert_eq!(
        harness.write(
            2,
            cc::CLUSTER,
            cc::START_UP_COLOR_TEMPERATURE_MIREDS,
            Value::U16(100)
        ),
        Status::Success
    );
    for _ in 0..3 {
        harness.next_command();
    }
    harness.stop();

    // On the next start the persisted start-up values are applied as targets: on, minimum
    // level, the coldest colour temperature the device has (100 is clamped to 153).
    let second = Harness::start_on(Arc::clone(&storage), Options::default());
    second.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    assert_eq!(second.read(2, Attribute::OnOff), Some(Value::Bool(true)));
    assert_eq!(second.read(2, Attribute::CurrentLevel), Some(Value::U8(1)));
    assert_eq!(
        second.read(2, Attribute::ColorTemperatureMireds),
        Some(Value::U16(153))
    );
    let mut commands = vec![
        second.next_command().command,
        second.next_command().command,
        second.next_command().command,
    ];
    commands.sort_by_key(|c| format!("{c:?}"));
    let mut expected = vec![
        DeviceCommand::SetOnOff { on: true },
        DeviceCommand::SetLevel {
            level: 1,
            transition: None,
            on_off: None,
        },
        DeviceCommand::SetColorTemperature {
            mireds: 153,
            transition: None,
        },
    ];
    expected.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(commands, expected);
    second.no_command();
    second.stop();

    // Null start-up values (the specification's "previous value") change nothing either.
    let third = Harness::start_on(Arc::clone(&storage), Options::default());
    third.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    for _ in 0..3 {
        third.next_command();
    }
    assert_eq!(
        third.write(2, on_off::CLUSTER, on_off::START_UP_ON_OFF, Value::Null),
        Status::Success
    );
    assert_eq!(
        third.write(2, lc::CLUSTER, lc::START_UP_CURRENT_LEVEL, Value::Null),
        Status::Success
    );
    assert_eq!(
        third.write(
            2,
            cc::CLUSTER,
            cc::START_UP_COLOR_TEMPERATURE_MIREDS,
            Value::Null
        ),
        Status::Success
    );
    third.stop();
    let fourth = Harness::start_on(storage, Options::default());
    fourth.add(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::ColorTemperatureLight),
        required_values(DeviceKind::ColorTemperatureLight),
    );
    assert_eq!(fourth.read(2, Attribute::OnOff), Some(Value::Bool(false)));
    fourth.no_command();
    fourth.stop();
}

#[test]
fn unknown_command_is_unsupported() {
    let harness = Harness::start();
    harness.on_off_light("lamp", false);
    // A command the endpoint does not list is refused before it reaches the adapter; a command
    // on a cluster the endpoint does not have as well.
    assert_eq!(
        harness.invoke(2, on_off::CLUSTER, CommandId::new(0x7E), Value::Null),
        Status::UnsupportedCommand
    );
    assert_eq!(
        harness.invoke(2, lc::CLUSTER, lc::MOVE_TO_LEVEL, Value::Null),
        Status::UnsupportedCluster
    );
    // A malformed payload is an invalid command.
    assert_eq!(
        harness.invoke(2, on_off::CLUSTER, on_off::OFF_WITH_EFFECT, Value::Null),
        Status::InvalidCommand
    );
    harness.no_command();
    assert_eq!(harness.read(2, Attribute::OnOff), Some(Value::Bool(false)));
    harness.stop();
}

// --- device API ------------------------------------------------------------------------------

fn vendor_cluster() -> ClusterDescription {
    ClusterDescription {
        id: ClusterId::new(0xFFF1_FC01),
        revision: 1,
        feature_map: 0,
        attributes: vec![AttributeId::new(0)],
        accepted_commands: vec![CommandId::new(1)],
        generated_commands: vec![],
        events: vec![],
    }
}

fn light_with_vendor_cluster(harness: &Harness) -> DeviceHandle {
    let mut spec = DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::OnOffLight);
    spec.parts[0].extra_clusters.push(vendor_cluster());
    harness.add(
        spec,
        InitialValues::new().value(Attribute::OnOff, false).value(
            Attribute::Raw(vendor_cluster().id, AttributeId::new(0)),
            Value::U8(0),
        ),
    )
}

#[test]
fn raw_command_times_out() {
    let timer = ManualTimer::default();
    let harness = Harness::start_on(
        Arc::new(MemoryStorage::new()),
        Options {
            response_timeout: Duration::from_secs(1),
            timer: Some(Arc::new(timer.clone())),
        },
    );
    light_with_vendor_cluster(&harness);
    let vendor = vendor_cluster().id;
    let invoke = harness
        .fake
        .invoke(ep(2), vendor, CommandId::new(1), Value::U8(7));
    let client = thread::spawn(move || block_on(invoke));
    let command = harness.next_command();
    let DeviceCommand::Raw {
        cluster,
        command: id,
        payload,
        response,
    } = command.command
    else {
        panic!("a raw command, got {:?}", command.command);
    };
    assert_eq!(
        (cluster, id, payload),
        (vendor, CommandId::new(1), Value::U8(7))
    );
    // The adapter does not answer; the timeout passes; the bridge answers `Failure`.
    assert!(!client.is_finished());
    timer.fire();
    let answer = client.join().unwrap().unwrap();
    assert_eq!(answer.status, Status::Failure);
    // A late answer is refused.
    assert!(matches!(
        block_on(response.succeed()),
        Err(Error::Engine(matter_rs_engine::Error::UnknownCorrelation(
            _
        )))
    ));
    harness.stop();
}

#[test]
fn raw_command_and_write_reach_the_adapter_with_a_response_handle() {
    let harness = Harness::start();
    let light = light_with_vendor_cluster(&harness);
    let vendor = vendor_cluster().id;
    // A raw command answered by the adapter with a payload.
    let invoke = harness
        .fake
        .invoke(ep(2), vendor, CommandId::new(1), Value::Null);
    let client = thread::spawn(move || block_on(invoke));
    let command = harness.next_command();
    let DeviceCommand::Raw { response, .. } = command.command else {
        panic!("a raw command");
    };
    block_on(response.respond(Response::with_payload(Value::U8(42)))).unwrap();
    let answer = client.join().unwrap().unwrap();
    assert_eq!(answer.status, Status::Success);
    assert_eq!(answer.payload, Some(Value::U8(42)));
    // A write forwarded and accepted: the engine stores it, the bridge's cache follows.
    let write = harness
        .fake
        .write(ep(2), vendor, AttributeId::new(0), Value::U8(5));
    let client = thread::spawn(move || block_on(write));
    let command = harness.next_command();
    let DeviceCommand::Write {
        attribute,
        value,
        response,
    } = command.command
    else {
        panic!("a write");
    };
    assert_eq!(attribute, Attribute::Raw(vendor, AttributeId::new(0)));
    assert_eq!(value, Value::U8(5));
    block_on(response.succeed()).unwrap();
    assert_eq!(client.join().unwrap().unwrap().status, Status::Success);
    assert_eq!(
        harness.fake.read(ep(2), vendor, AttributeId::new(0)),
        Some(Value::U8(5))
    );
    assert_eq!(
        light.value(0, Attribute::Raw(vendor, AttributeId::new(0))),
        Some(Value::U8(5))
    );
    // A write refused by the adapter leaves the value.
    let write = harness
        .fake
        .write(ep(2), vendor, AttributeId::new(0), Value::U8(9));
    let client = thread::spawn(move || block_on(write));
    let DeviceCommand::Write { response, .. } = harness.next_command().command else {
        panic!("a write");
    };
    block_on(response.fail(Status::ConstraintError)).unwrap();
    assert_eq!(
        client.join().unwrap().unwrap().status,
        Status::ConstraintError
    );
    assert_eq!(
        harness.fake.read(ep(2), vendor, AttributeId::new(0)),
        Some(Value::U8(5))
    );
    // A dropped handle is a failure.
    let invoke = harness
        .fake
        .invoke(ep(2), vendor, CommandId::new(1), Value::Null);
    let client = thread::spawn(move || block_on(invoke));
    drop(harness.next_command());
    assert_eq!(client.join().unwrap().unwrap().status, Status::Failure);
    // The adapter sets a raw attribute of the extra cluster like any other.
    block_on(light.set(0, Attribute::Raw(vendor, AttributeId::new(0)), Value::U8(1))).unwrap();
    assert_eq!(
        harness.fake.read(ep(2), vendor, AttributeId::new(0)),
        Some(Value::U8(1))
    );
    harness.stop();
}

#[test]
fn device_errors_are_isolated() {
    let harness = Harness::start();
    let a = harness.dimmable_light("a", true, 100);
    let b = harness.dimmable_light("b", true, 50);
    // Wrong type, out of range, unknown attribute, unknown part: each an error on `a` only.
    assert!(matches!(
        block_on(a.set(0, Attribute::OnOff, Value::U8(1))),
        Err(Error::InvalidValue { .. })
    ));
    assert!(matches!(
        block_on(a.set(0, Attribute::CurrentLevel, Value::U8(0))),
        Err(Error::InvalidValue { .. })
    ));
    assert_eq!(
        block_on(a.set(0, Attribute::Temperature, Value::I16(1))),
        Err(Error::UnknownAttribute {
            key: key("a"),
            part: 0,
            attribute: Attribute::Temperature
        })
    );
    assert_eq!(
        block_on(a.set(1, Attribute::OnOff, false)),
        Err(Error::UnknownPart {
            key: key("a"),
            part: 1
        })
    );
    assert_eq!(
        block_on(a.emit(
            0,
            matter_rs_bridge::DeviceEvent::InitialPress { position: 1 }
        )),
        Err(Error::UnsupportedEvent {
            key: key("a"),
            part: 0
        })
    );
    // `a` itself is unchanged, and so is `b`.
    assert_eq!(harness.read(2, Attribute::OnOff), Some(Value::Bool(true)));
    assert_eq!(
        harness.read(2, Attribute::CurrentLevel),
        Some(Value::U8(100))
    );
    assert_eq!(harness.read(3, Attribute::OnOff), Some(Value::Bool(true)));
    assert_eq!(
        harness.read(3, Attribute::CurrentLevel),
        Some(Value::U8(50))
    );
    assert!(a.is_reachable());
    assert!(b.is_reachable());
    // A removed device's handle fails while the other keeps working.
    block_on(harness.bridge.remove_device(&key("a"))).unwrap();
    assert_eq!(
        block_on(a.set_reachable(false)),
        Err(Error::UnknownDevice(key("a")))
    );
    assert_eq!(a.endpoints(), Err(Error::UnknownDevice(key("a"))));
    block_on(b.set(0, Attribute::CurrentLevel, Value::U8(60))).unwrap();
    assert_eq!(
        harness.read(3, Attribute::CurrentLevel),
        Some(Value::U8(60))
    );
    assert!(
        harness
            .fake
            .attribute_log()
            .iter()
            .all(|v| v.endpoint == ep(3))
    );
    harness.stop();
}

#[test]
fn contact_sensor_change_emits_state_change() {
    let harness = Harness::start();
    let contact = harness.add(
        DeviceSpec::single(key("door"), "Door", DeviceKind::ContactSensor),
        InitialValues::new().value(Attribute::StateValue, true),
    );
    block_on(contact.set(0, Attribute::StateValue, false)).unwrap();
    // Setting the same value again reports no change.
    block_on(contact.set(0, Attribute::StateValue, false)).unwrap();
    assert_eq!(
        harness.read(2, Attribute::StateValue),
        Some(Value::Bool(false))
    );
    assert_eq!(
        harness.fake.event_log(),
        vec![EmittedEvent {
            endpoint: ep(2),
            cluster: boolean_state::CLUSTER,
            event: boolean_state::STATE_CHANGE,
            payload: Value::Struct(vec![(0, Value::Bool(false))]),
        }]
    );
    harness.stop();
}

#[test]
fn duplicate_device_is_rejected_and_nothing_is_left_after_a_failed_add() {
    let harness = Harness::start();
    harness.on_off_light("lamp", false);
    let error = block_on(harness.bridge.add_device(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::OnOffLight),
        InitialValues::new().value(Attribute::OnOff, false),
    ))
    .unwrap_err();
    assert_eq!(error, Error::DuplicateDevice(key("lamp")));
    // An engine with room for two endpoints refuses the composed device's third; the bridge
    // removes what it added.
    let limited = FakeEngine::with_capabilities(matter_rs_engine::Capabilities {
        max_endpoints: Some(2),
        ..matter_rs_engine::Capabilities::unbounded()
    });
    let bridge = block_on(Bridge::start(
        Box::new(limited.clone()),
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        Arc::new(MemoryStorage::new()),
    ))
    .unwrap();
    let (spec, initial) = composed_strip();
    let error = block_on(bridge.add_device(spec, initial)).unwrap_err();
    assert!(
        matches!(
            error,
            Error::Engine(matter_rs_engine::Error::CapacityExceeded { .. })
        ),
        "{error:?}"
    );
    assert!(limited.endpoints().is_empty());
    assert!(bridge.devices().is_empty());
    block_on(bridge.stop()).unwrap();
    harness.stop();
}

#[test]
fn node_lifecycle_passes_through() {
    let harness = Harness::start();
    let state = block_on(harness.bridge.commissioning_state()).unwrap();
    assert!(!state.commissioned);
    let info = block_on(harness.bridge.pairing_info()).unwrap();
    assert_eq!(info.discriminator, 3840);
    assert!(block_on(harness.bridge.fabrics()).unwrap().is_empty());
    block_on(harness.bridge.close_commissioning_window()).unwrap();
    assert_eq!(
        block_on(harness.bridge.commissioning_state())
            .unwrap()
            .window,
        None
    );
    block_on(
        harness
            .bridge
            .open_commissioning_window(matter_rs_engine::Window::Basic, Duration::from_secs(60)),
    )
    .unwrap();
    assert_eq!(
        block_on(harness.bridge.commissioning_state())
            .unwrap()
            .window,
        Some(matter_rs_engine::Window::Basic)
    );
    assert_eq!(
        harness.bridge.capabilities(),
        matter_rs_engine::Capabilities::unbounded()
    );
    harness.stop();
}

#[test]
fn factory_reset_forgets_removed_devices_and_written_values() {
    let storage = Arc::new(MemoryStorage::new());
    let harness = Harness::start_on(Arc::clone(&storage), Options::default());
    harness.dimmable_light("kept", true, 100);
    harness.on_off_light("removed", true);
    assert_eq!(
        harness.write(2, lc::CLUSTER, lc::ON_LEVEL, Value::U8(7)),
        Status::Success
    );
    harness.next_command();
    block_on(harness.bridge.remove_device(&key("removed"))).unwrap();
    block_on(harness.bridge.factory_reset()).unwrap();
    assert_eq!(harness.bridge.reserved_endpoints(), vec![ep(2)]);
    assert!(
        !storage
            .keys()
            .iter()
            .any(|k| k.starts_with("bridge/attributes/"))
    );
    // The kept device keeps its endpoint and number; a new device gets the next number.
    assert_eq!(harness.fake.endpoints().len(), 1);
    let new = harness.on_off_light("new", false);
    assert_eq!(new.endpoints().unwrap()[0].endpoint, ep(4));
    harness.stop();
}

#[test]
fn bridge_futures_are_send() {
    fn assert_send<T: Send>(_: &T) {}
    let fake = FakeEngine::new();
    let start = Bridge::start(
        Box::new(fake.clone()),
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        Arc::new(MemoryStorage::new()),
    );
    assert_send(&start);
    let bridge = block_on(start).unwrap();
    assert_send(&bridge.next_command());
    assert_send(&bridge.next_event());
    let add = bridge.add_device(
        DeviceSpec::single(key("lamp"), "Lamp", DeviceKind::OnOffLight),
        InitialValues::new().value(Attribute::OnOff, false),
    );
    assert_send(&add);
    let light = block_on(add).unwrap();
    assert_send(&light.set(0, Attribute::OnOff, true));
    assert_send(&light.set_reachable(false));
    assert_send(&bridge.remove_device(&key("lamp")));
    assert_send(&bridge.stop());
    block_on(bridge.stop()).unwrap();
}
