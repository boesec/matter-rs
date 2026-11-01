//! The fake engine against the engine contract: the round trips of commands and writes, the
//! endpoint lifecycle, capacity, commissioning, fabrics, pairing information and the lifecycle
//! of the node itself. A product task runs on its own thread, as it would in a product crate.

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use matter_rs_engine::testing::{EmittedEvent, FakeEngine, block_on};
use matter_rs_engine::{
    Attestation, AttributeId, AttributeValue, Capabilities, ClusterDescription, ClusterId,
    CommandId, CommissioningState, Correlation, DeviceType, DeviceTypeId, EndpointDescription,
    EndpointId, Engine, Error, Event, EventId, FabricIndex, FabricInfo, MemoryStorage,
    NetworkConfig, NodeIdentity, NodeRole, ProductId, REDACTED, Response, Running, Secret, Status,
    Value, VendorId, Window, bridged_device,
};

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

fn identity() -> NodeIdentity {
    NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
        vendor_name: "matter-rs".into(),
        product_name: "Bridge".into(),
        node_label: "Bridge".into(),
        serial_number: Some("serial".into()),
        software_version: 1,
        software_version_string: "0.1.0".into(),
        unique_id: "bridge-1".into(),
        passcode: Secret::new(20_202_021),
        discriminator: 3840,
    }
}

fn start(fake: &FakeEngine) -> Arc<dyn Running> {
    let engine: &dyn Engine = fake;
    let node = block_on(engine.start(
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::new(MemoryStorage::new()),
    ))
    .expect("the node starts");
    Arc::from(node)
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

/// An On/Off light with a Level Control cluster, both with initial values.
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

fn fabric(index: u8) -> FabricInfo {
    FabricInfo {
        index: FabricIndex::new(index),
        vendor_id: VendorId::new(0xFFF1),
        fabric_id: 0x1234,
        node_id: u64::from(index),
        label: format!("fabric {index}"),
    }
}

/// Runs the product's event loop on its own thread: every event goes to `handle`, which
/// answers requests through the node; the loop ends with the stop event. The thread returns
/// every event it saw.
fn spawn_product<F>(node: Arc<dyn Running>, mut handle: F) -> JoinHandle<Vec<Event>>
where
    F: FnMut(&dyn Running, &Event) + Send + 'static,
{
    thread::spawn(move || {
        let mut seen = Vec::new();
        while let Ok(event) = block_on(node.next_event()) {
            handle(&*node, &event);
            let stopped = matches!(event, Event::Stopped(_));
            seen.push(event);
            if stopped {
                break;
            }
        }
        seen
    })
}

fn respond(node: &dyn Running, correlation: Correlation, response: Response) {
    block_on(node.respond(correlation, response)).expect("response accepted");
}

#[test]
fn fake_engine_command_round_trip() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    assert_eq!(
        fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE),
        Some(Value::Bool(false))
    );

    let product = spawn_product(Arc::clone(&node), |node, event| {
        if let Event::CommandInvoked {
            correlation,
            endpoint,
            cluster,
            command,
            payload,
            fabric,
        } = event
        {
            assert_eq!((*endpoint, *cluster, *command), (LIGHT, ON_OFF, ON));
            assert_eq!(payload, &Value::Struct(vec![(0, Value::U8(7))]));
            assert_eq!(*fabric, None);
            block_on(node.set_attribute(*endpoint, *cluster, ON_OFF_ATTRIBUTE, Value::Bool(true)))
                .unwrap();
            respond(
                node,
                *correlation,
                Response::with_payload(Value::Struct(vec![(0, Value::Bool(true))])),
            );
        }
    });

    let response =
        block_on(fake.invoke(LIGHT, ON_OFF, ON, Value::Struct(vec![(0, Value::U8(7))]))).unwrap();
    assert_eq!(response.status, Status::Success);
    assert_eq!(
        response.payload,
        Some(Value::Struct(vec![(0, Value::Bool(true))]))
    );
    assert_eq!(
        fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE),
        Some(Value::Bool(true))
    );
    assert_eq!(
        fake.attribute_log(),
        vec![AttributeValue {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: ON_OFF_ATTRIBUTE,
            value: Value::Bool(true),
        }]
    );

    block_on(node.stop()).unwrap();
    let seen = product.join().unwrap();
    assert!(matches!(seen[0], Event::CommandInvoked { .. }));
    assert_eq!(
        seen.last(),
        Some(&Event::Stopped(matter_rs_engine::StopReason::Requested))
    );
}

#[test]
fn fake_engine_write_round_trip() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let product = spawn_product(Arc::clone(&node), |node, event| {
        if let Event::WriteRequested {
            correlation,
            endpoint,
            cluster,
            attribute,
            value,
            ..
        } = event
        {
            assert_eq!(
                (*endpoint, *cluster, *attribute),
                (LIGHT, LEVEL, CURRENT_LEVEL)
            );
            assert_eq!(value, &Value::U8(42));
            respond(node, *correlation, Response::success());
        }
    });

    let response = block_on(fake.write(LIGHT, LEVEL, CURRENT_LEVEL, Value::U8(42))).unwrap();
    assert_eq!(response.status, Status::Success);
    assert_eq!(fake.read(LIGHT, LEVEL, CURRENT_LEVEL), Some(Value::U8(42)));
    assert_eq!(
        fake.attribute_log(),
        vec![AttributeValue {
            endpoint: LIGHT,
            cluster: LEVEL,
            attribute: CURRENT_LEVEL,
            value: Value::U8(42),
        }]
    );

    block_on(node.stop()).unwrap();
    product.join().unwrap();
}

#[test]
fn rejected_write_keeps_value() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let product = spawn_product(Arc::clone(&node), |node, event| {
        if let Event::WriteRequested { correlation, .. } = event {
            respond(
                node,
                *correlation,
                Response::status(Status::ConstraintError),
            );
        }
    });

    let response = block_on(fake.write(LIGHT, LEVEL, CURRENT_LEVEL, Value::U8(255))).unwrap();
    assert_eq!(response.status, Status::ConstraintError);
    assert_eq!(fake.read(LIGHT, LEVEL, CURRENT_LEVEL), Some(Value::U8(0)));
    assert!(fake.attribute_log().is_empty());

    block_on(node.stop()).unwrap();
    product.join().unwrap();
}

#[test]
fn unknown_paths_are_answered_by_the_engine() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let status = |fut: matter_rs_engine::BoxFuture<'static, Result<Response, Error>>| {
        block_on(fut).unwrap().status
    };
    assert_eq!(
        status(fake.invoke(EndpointId::new(9), ON_OFF, ON, Value::Null)),
        Status::UnsupportedEndpoint
    );
    assert_eq!(
        status(fake.invoke(LIGHT, ClusterId::new(0x0300), ON, Value::Null)),
        Status::UnsupportedCluster
    );
    assert_eq!(
        status(fake.invoke(LIGHT, ON_OFF, CommandId::new(0x40), Value::Null)),
        Status::UnsupportedCommand
    );
    assert_eq!(
        status(fake.write(EndpointId::new(9), ON_OFF, ON_OFF_ATTRIBUTE, Value::Null)),
        Status::UnsupportedEndpoint
    );
    assert_eq!(
        status(fake.write(LIGHT, ClusterId::new(0x0300), ON_OFF_ATTRIBUTE, Value::Null)),
        Status::UnsupportedCluster
    );
    assert_eq!(
        status(fake.write(LIGHT, ON_OFF, AttributeId::new(0x4000), Value::Null)),
        Status::UnsupportedAttribute
    );
    assert_eq!(
        status(fake.write(LIGHT, ON_OFF, AttributeId::new(0xFFFD), Value::U16(1))),
        Status::UnsupportedWrite
    );

    // None of these reached the product: the first event is the stop.
    block_on(node.stop()).unwrap();
    assert!(matches!(block_on(node.next_event()), Ok(Event::Stopped(_))));
}

#[test]
fn duplicate_endpoint_rejected() {
    let fake = FakeEngine::new();
    let node = start(&fake);
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
    assert_eq!(fake.endpoints().len(), 1);
}

#[test]
fn unknown_endpoint_rejected() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
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
        block_on(node.set_reachable(unknown, false)),
        Err(Error::UnknownEndpoint(unknown))
    );
    // The engine's own endpoints are not the product's to remove.
    assert_eq!(
        block_on(node.remove_endpoint(AGGREGATOR)),
        Err(Error::UnknownEndpoint(AGGREGATOR))
    );

    // An existing endpoint, but a cluster or attribute it does not list.
    assert_eq!(
        block_on(node.set_attribute(LIGHT, ON_OFF, AttributeId::new(0x4000), Value::Bool(true))),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: AttributeId::new(0x4000),
        })
    );
    assert_eq!(
        block_on(node.set_attribute(
            LIGHT,
            ClusterId::new(0x0300),
            ON_OFF_ATTRIBUTE,
            Value::Bool(true)
        )),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: ClusterId::new(0x0300),
            attribute: ON_OFF_ATTRIBUTE,
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
    assert_eq!(
        block_on(node.set_reachable(LIGHT, false)),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: bridged_device::CLUSTER,
            attribute: bridged_device::REACHABLE,
        })
    );
    assert!(fake.attribute_log().is_empty());
    assert!(fake.event_log().is_empty());
}

#[test]
fn endpoint_can_be_re_added() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    block_on(node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))).unwrap();

    block_on(node.remove_endpoint(LIGHT)).unwrap();
    assert!(fake.endpoints().is_empty());
    assert_eq!(fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE), None);

    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    assert_eq!(fake.endpoints(), vec![light(LIGHT)]);
    // The re-added endpoint starts from its initial values, not from the old state.
    assert_eq!(
        fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE),
        Some(Value::Bool(false))
    );
}

#[test]
fn removing_an_endpoint_removes_its_parts() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    let part = EndpointId::new(3);
    let part_of_part = EndpointId::new(4);
    let other = EndpointId::new(5);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    block_on(node.add_endpoint(EndpointDescription {
        parent: Some(LIGHT),
        ..light(part)
    }))
    .unwrap();
    block_on(node.add_endpoint(EndpointDescription {
        parent: Some(part),
        ..light(part_of_part)
    }))
    .unwrap();
    block_on(node.add_endpoint(light(other))).unwrap();

    block_on(node.remove_endpoint(LIGHT)).unwrap();
    let remaining: Vec<EndpointId> = fake.endpoints().iter().map(|e| e.id).collect();
    assert_eq!(remaining, vec![other]);
}

#[test]
fn unknown_parent_rejected() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    let missing = EndpointId::new(7);
    assert_eq!(
        block_on(node.add_endpoint(EndpointDescription {
            parent: Some(missing),
            ..light(LIGHT)
        })),
        Err(Error::UnknownEndpoint(missing))
    );
    assert!(fake.endpoints().is_empty());

    // The aggregator and an endpoint the product added are valid parents.
    block_on(node.add_endpoint(EndpointDescription {
        parent: Some(AGGREGATOR),
        ..light(LIGHT)
    }))
    .unwrap();
    block_on(node.add_endpoint(EndpointDescription {
        parent: Some(LIGHT),
        ..light(EndpointId::new(3))
    }))
    .unwrap();
    assert_eq!(fake.endpoints().len(), 2);
}

#[test]
fn missing_initial_value_rejected() {
    let fake = FakeEngine::new();
    let node = start(&fake);

    let without_level = EndpointDescription {
        initial_values: vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))],
        ..light(LIGHT)
    };
    let error = block_on(node.add_endpoint(without_level)).unwrap_err();
    match &error {
        Error::InvalidDescription { endpoint, reason } => {
            assert_eq!(*endpoint, LIGHT);
            assert!(reason.contains("0x0000"), "{reason}");
            assert!(reason.contains("0x0008"), "{reason}");
            assert!(reason.contains("no initial value"), "{reason}");
        }
        other => panic!("unexpected error {other:?}"),
    }
    assert!(fake.endpoints().is_empty());
}

#[test]
fn inconsistent_descriptions_are_rejected() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    let reason_of =
        |description: EndpointDescription| match block_on(node.add_endpoint(description)) {
            Err(Error::InvalidDescription { endpoint, reason }) => {
                assert_eq!(endpoint, LIGHT);
                reason
            }
            other => panic!("expected InvalidDescription, got {other:?}"),
        };

    let mut extra_value = light(LIGHT);
    extra_value
        .initial_values
        .push((ON_OFF, AttributeId::new(0x4000), Value::U8(1)));
    assert!(reason_of(extra_value).contains("not listed"));

    let mut twice = light(LIGHT);
    twice
        .initial_values
        .push((ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true)));
    assert!(reason_of(twice).contains("two initial values"));

    let mut global = light(LIGHT);
    global.clusters[0].attributes.push(AttributeId::new(0xFFFD));
    global
        .initial_values
        .push((ON_OFF, AttributeId::new(0xFFFD), Value::U16(6)));
    assert!(reason_of(global).contains("global attribute"));

    let mut duplicate_cluster = light(LIGHT);
    duplicate_cluster.clusters.push(on_off_cluster());
    assert!(reason_of(duplicate_cluster).contains("listed twice"));

    let mut duplicate_attribute = light(LIGHT);
    duplicate_attribute.clusters[0]
        .attributes
        .push(ON_OFF_ATTRIBUTE);
    assert!(reason_of(duplicate_attribute).contains("listed twice"));

    let mut no_device_type = light(LIGHT);
    no_device_type.device_types.clear();
    assert!(reason_of(no_device_type).contains("no device type"));

    assert!(fake.endpoints().is_empty());
}

#[test]
fn capacity_exceeded_reported() {
    let fake = FakeEngine::with_capabilities(Capabilities {
        max_endpoints: Some(2),
        ..Capabilities::unbounded()
    });
    let engine: &dyn Engine = &fake;
    assert_eq!(engine.capabilities().max_endpoints, Some(2));

    let node = start(&fake);
    assert_eq!(node.capabilities().max_endpoints, Some(2));
    block_on(node.add_endpoint(light(EndpointId::new(2)))).unwrap();
    block_on(node.add_endpoint(light(EndpointId::new(3)))).unwrap();
    assert_eq!(
        block_on(node.add_endpoint(light(EndpointId::new(4)))),
        Err(Error::CapacityExceeded { what: "endpoints" })
    );
    assert_eq!(fake.endpoints().len(), 2);

    // Room is freed by removing an endpoint.
    block_on(node.remove_endpoint(EndpointId::new(2))).unwrap();
    block_on(node.add_endpoint(light(EndpointId::new(4)))).unwrap();
}

#[test]
fn set_attributes_is_all_or_nothing() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let batch = vec![
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
    ];
    assert!(matches!(
        block_on(node.set_attributes(batch)),
        Err(Error::UnknownAttribute { .. })
    ));
    assert_eq!(
        fake.read(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE),
        Some(Value::Bool(false))
    );
    assert!(fake.attribute_log().is_empty());

    let batch = vec![
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
    ];
    block_on(node.set_attributes(batch.clone())).unwrap();
    assert_eq!(fake.read(LIGHT, LEVEL, CURRENT_LEVEL), Some(Value::U8(100)));
    assert_eq!(fake.attribute_log(), batch);
}

#[test]
fn set_reachable_sets_attribute_and_emits_event() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    let mut bridged = light(LIGHT);
    bridged.clusters.push(bridged_device_cluster());
    bridged
        .initial_values
        .push((bridged_device::CLUSTER, NODE_LABEL, Value::from("Kitchen")));
    bridged.initial_values.push((
        bridged_device::CLUSTER,
        bridged_device::REACHABLE,
        Value::Bool(true),
    ));
    block_on(node.add_endpoint(bridged)).unwrap();

    block_on(node.set_reachable(LIGHT, false)).unwrap();
    assert_eq!(
        fake.read(LIGHT, bridged_device::CLUSTER, bridged_device::REACHABLE),
        Some(Value::Bool(false))
    );
    assert_eq!(
        fake.event_log(),
        vec![EmittedEvent {
            endpoint: LIGHT,
            cluster: bridged_device::CLUSTER,
            event: bridged_device::REACHABLE_CHANGED,
            payload: Value::Struct(vec![(
                bridged_device::REACHABLE_NEW_VALUE_TAG,
                Value::Bool(false)
            )]),
        }]
    );

    // A plain event goes through the same log.
    block_on(node.emit_event(
        LIGHT,
        bridged_device::CLUSTER,
        bridged_device::REACHABLE_CHANGED,
        Value::Struct(vec![(0, Value::Bool(true))]),
    ))
    .unwrap();
    assert_eq!(fake.event_log().len(), 2);
}

#[test]
fn commissioning_state_changes_are_events() {
    let fake = FakeEngine::new();
    let node = start(&fake);

    // A node without fabrics starts with a basic window open.
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
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(closed)
    );
    assert_eq!(block_on(node.commissioning_state()).unwrap(), closed);

    block_on(node.open_commissioning_window(Window::Enhanced, Duration::from_secs(300))).unwrap();
    let enhanced = CommissioningState {
        commissioned: false,
        window: Some(Window::Enhanced),
    };
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(enhanced)
    );

    // The test hook plays the network: a commissioner finished elsewhere.
    let commissioned = CommissioningState {
        commissioned: true,
        window: None,
    };
    fake.set_commissioning_state(commissioned).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(commissioned)
    );
    assert_eq!(block_on(node.commissioning_state()).unwrap(), commissioned);

    // Closing a closed window is not a change.
    block_on(node.close_commissioning_window()).unwrap();
    block_on(node.stop()).unwrap();
    assert!(matches!(block_on(node.next_event()), Ok(Event::Stopped(_))));
}

#[test]
fn enhanced_window_needs_the_capability() {
    let fake = FakeEngine::with_capabilities(Capabilities {
        enhanced_window: false,
        ..Capabilities::unbounded()
    });
    let node = start(&fake);
    assert_eq!(
        block_on(node.open_commissioning_window(Window::Enhanced, Duration::from_secs(1))),
        Err(Error::Unsupported {
            operation: "enhanced commissioning window"
        })
    );
    block_on(node.open_commissioning_window(Window::Basic, Duration::from_secs(1))).unwrap();
}

#[test]
fn fabric_add_remove_events() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![]);

    fake.add_fabric(fabric(1)).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricAdded(fabric(1))
    );
    // Commissioning closes the window and makes the node commissioned.
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(CommissioningState {
            commissioned: true,
            window: None,
        })
    );
    fake.add_fabric(fabric(2)).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricAdded(fabric(2))
    );
    assert!(matches!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(_)
    ));
    assert_eq!(
        block_on(node.fabrics()).unwrap(),
        vec![fabric(1), fabric(2)]
    );

    assert!(matches!(fake.add_fabric(fabric(2)), Err(Error::Engine(_))));

    block_on(node.remove_fabric(FabricIndex::new(1))).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricRemoved(FabricIndex::new(1))
    );
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![fabric(2)]);
    assert_eq!(
        block_on(node.remove_fabric(FabricIndex::new(1))),
        Err(Error::UnknownFabric(FabricIndex::new(1)))
    );

    // Removing the last fabric changes the commissioning state.
    block_on(node.remove_fabric(FabricIndex::new(2))).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricRemoved(FabricIndex::new(2))
    );
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: None,
        })
    );
}

#[test]
fn fabric_capacity_is_reported() {
    let fake = FakeEngine::with_capabilities(Capabilities {
        max_fabrics: Some(1),
        ..Capabilities::unbounded()
    });
    let _node = start(&fake);
    fake.add_fabric(fabric(1)).unwrap();
    assert_eq!(
        fake.add_fabric(fabric(2)),
        Err(Error::CapacityExceeded { what: "fabrics" })
    );
}

#[test]
fn factory_reset_removes_every_fabric() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    fake.add_fabric(fabric(1)).unwrap();
    fake.add_fabric(fabric(2)).unwrap();
    for _ in 0..4 {
        block_on(node.next_event()).unwrap();
    }

    block_on(node.factory_reset()).unwrap();
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricRemoved(FabricIndex::new(1))
    );
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::FabricRemoved(FabricIndex::new(2))
    );
    assert_eq!(
        block_on(node.next_event()).unwrap(),
        Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: None,
        })
    );
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![]);
    // The node keeps running.
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
}

#[test]
fn fabrics_survive_a_restart_endpoints_do_not() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    fake.add_fabric(fabric(1)).unwrap();
    block_on(node.stop()).unwrap();

    let node = start(&fake);
    assert_eq!(block_on(node.fabrics()).unwrap(), vec![fabric(1)]);
    assert!(fake.endpoints().is_empty());
    // A commissioned node starts without a window.
    assert_eq!(
        block_on(node.commissioning_state()).unwrap(),
        CommissioningState {
            commissioned: true,
            window: None,
        }
    );
}

#[test]
fn pairing_info_is_redacted() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    let info = block_on(node.pairing_info()).unwrap();
    assert_eq!(info.discriminator, 3840);
    let output = format!("{info:?}");
    assert!(output.contains(REDACTED), "{output}");
    assert!(!output.contains("20202021"), "{output}");
    assert!(output.contains("3840"), "{output}");
    // The values are reachable only through `expose`.
    assert!(info.qr_text.expose().starts_with("MT:"));
    assert!(info.manual_code.expose().contains("20202021"));

    block_on(node.open_commissioning_window(Window::Enhanced, Duration::from_secs(60))).unwrap();
    let info = block_on(node.pairing_info()).unwrap();
    assert!(!info.manual_code.expose().contains("20202021"));
}

#[test]
fn engine_is_dyn_compatible() {
    let fake = FakeEngine::new();
    let engine: Box<dyn Engine> = Box::new(fake.clone());
    assert_eq!(engine.capabilities(), Capabilities::unbounded());
    let node: Box<dyn Running> = block_on(engine.start(
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::new(MemoryStorage::new()),
    ))
    .unwrap();
    assert!(fake.is_running());
    block_on(node.add_endpoint(light(LIGHT))).unwrap();
    let shared: Arc<dyn Running> = Arc::from(node);
    block_on(shared.stop()).unwrap();
    assert!(!fake.is_running());
}

#[test]
fn invalid_identity_rejected() {
    let fake = FakeEngine::new();
    let engine: &dyn Engine = &fake;
    let start_with = |identity: NodeIdentity, network: NetworkConfig| {
        block_on(engine.start(
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
    let error = start_with(wide_discriminator, NetworkConfig::default()).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidIdentity { reason } if reason.contains("discriminator"))
    );

    let mut forbidden_passcode = identity();
    forbidden_passcode.passcode = Secret::new(12_345_678);
    let error = start_with(forbidden_passcode, NetworkConfig::default()).unwrap_err();
    match &error {
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
        ..NetworkConfig::default()
    };
    let error = start_with(identity(), no_family).unwrap_err();
    assert!(matches!(&error, Error::InvalidIdentity { reason } if reason.contains("IPv4")));

    assert!(!fake.is_running());
}

#[test]
fn stop_ends_the_event_stream_and_the_node() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    // A request that is still pending when the node stops fails instead of hanging.
    let pending = fake.invoke(LIGHT, ON_OFF, TOGGLE, Value::Null);
    let correlation = match block_on(node.next_event()).unwrap() {
        Event::CommandInvoked { correlation, .. } => correlation,
        other => panic!("unexpected event {other:?}"),
    };

    block_on(node.stop()).unwrap();
    assert_eq!(block_on(pending), Err(Error::NotRunning));
    assert_eq!(
        block_on(node.next_event()),
        Ok(Event::Stopped(matter_rs_engine::StopReason::Requested))
    );
    assert_eq!(block_on(node.next_event()), Err(Error::NotRunning));

    assert_eq!(block_on(node.stop()), Err(Error::NotRunning));
    assert_eq!(
        block_on(node.respond(correlation, Response::success())),
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
        block_on(fake.invoke(LIGHT, ON_OFF, ON, Value::Null)),
        Err(Error::NotRunning)
    );
    assert_eq!(
        block_on(fake.write(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Null)),
        Err(Error::NotRunning)
    );
    assert_eq!(fake.add_fabric(fabric(1)), Err(Error::NotRunning));
    assert_eq!(
        fake.set_commissioning_state(CommissioningState {
            commissioned: false,
            window: None
        }),
        Err(Error::NotRunning)
    );
}

#[test]
fn engine_runs_one_node_at_a_time() {
    let fake = FakeEngine::new();
    let engine: &dyn Engine = &fake;
    let node = start(&fake);
    let second = block_on(engine.start(
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::new(MemoryStorage::new()),
    ));
    assert!(matches!(second, Err(Error::AlreadyRunning)));
    block_on(node.stop()).unwrap();
    let _again = start(&fake);
}

#[test]
fn a_response_is_accepted_once() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let request = fake.invoke(LIGHT, ON_OFF, TOGGLE, Value::Null);
    let correlation = match block_on(node.next_event()).unwrap() {
        Event::CommandInvoked { correlation, .. } => correlation,
        other => panic!("unexpected event {other:?}"),
    };
    block_on(node.respond(correlation, Response::success())).unwrap();
    assert_eq!(
        block_on(node.respond(correlation, Response::success())),
        Err(Error::UnknownCorrelation(correlation))
    );
    assert_eq!(block_on(request).unwrap().status, Status::Success);
    assert_eq!(
        block_on(node.respond(Correlation::new(999), Response::success())),
        Err(Error::UnknownCorrelation(Correlation::new(999)))
    );
}

#[test]
fn requests_to_one_endpoint_arrive_in_order() {
    let fake = FakeEngine::new();
    let node = start(&fake);
    block_on(node.add_endpoint(light(LIGHT))).unwrap();

    let first = fake.invoke(LIGHT, ON_OFF, ON, Value::Null);
    let second = fake.write(LIGHT, LEVEL, CURRENT_LEVEL, Value::U8(3));
    let third = fake.invoke(LIGHT, ON_OFF, TOGGLE, Value::Null);

    let product = spawn_product(Arc::clone(&node), |node, event| match event {
        Event::CommandInvoked { correlation, .. } | Event::WriteRequested { correlation, .. } => {
            respond(node, *correlation, Response::success());
        }
        _ => {}
    });
    assert_eq!(block_on(first).unwrap().status, Status::Success);
    assert_eq!(block_on(second).unwrap().status, Status::Success);
    assert_eq!(block_on(third).unwrap().status, Status::Success);
    block_on(node.stop()).unwrap();
    let seen = product.join().unwrap();
    let kinds: Vec<&str> = seen
        .iter()
        .map(|event| match event {
            Event::CommandInvoked { command, .. } if *command == ON => "on",
            Event::CommandInvoked { .. } => "toggle",
            Event::WriteRequested { .. } => "write",
            Event::Stopped(_) => "stopped",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["on", "write", "toggle", "stopped"]);
}
