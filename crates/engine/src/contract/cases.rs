//! The cases of the engine contract suite, one `pub fn` each.
//!
//! Every case documents the rule of the [`engine`](crate::engine) module it pins, starts its
//! own node and stops it. The ids used here are those of the On/Off, Level Control and Bridged
//! Device Basic Information clusters and of the On/Off Light device type; the suite needs no
//! cluster semantics, only ids the descriptions can name.

// Every case panics exactly when the engine breaks the rule it pins; that is what a test case
// is for, and a `# Panics` section on each would repeat its first sentence.
#![allow(clippy::missing_panics_doc)]

use alloc::format;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use super::{EngineUnderTest, RecordingStorage, assert_error, next_event, run, start_on, stop};
use crate::description::{ClusterDescription, DeviceType, EndpointDescription};
use crate::engine::{
    AttributeValue, CommissioningState, Correlation, Event, Response, Window, bridged_device,
};
use crate::error::Error;
use crate::id::{AttributeId, ClusterId, CommandId, DeviceTypeId, EndpointId, EventId};
use crate::identity::{NetworkConfig, NodeRole};
use crate::secret::{REDACTED, Secret};
use crate::storage::Storage;
use crate::value::Value;

const LIGHT: EndpointId = EndpointId::new(2);
const ON_OFF: ClusterId = ClusterId::new(0x0006);
const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0x0000);
const ON: CommandId = CommandId::new(0x01);
const TOGGLE: CommandId = CommandId::new(0x02);
const LEVEL: ClusterId = ClusterId::new(0x0008);
const CURRENT_LEVEL: AttributeId = AttributeId::new(0x0000);
const ON_OFF_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0100);
const NODE_LABEL: AttributeId = AttributeId::new(0x0005);
/// A global attribute id (`ClusterRevision`), which a description must not list.
const CLUSTER_REVISION: AttributeId = AttributeId::new(0xFFFD);

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

fn bridged_device_cluster(with_event: bool) -> ClusterDescription {
    ClusterDescription {
        id: bridged_device::CLUSTER,
        revision: 6,
        feature_map: 0,
        attributes: vec![NODE_LABEL, bridged_device::REACHABLE],
        accepted_commands: vec![],
        generated_commands: vec![],
        events: if with_event {
            vec![bridged_device::REACHABLE_CHANGED]
        } else {
            vec![]
        },
    }
}

/// An On/Off light with a Level Control cluster, every attribute with an initial value.
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

/// A light that also carries the Bridged Device Basic Information cluster.
fn bridged_light(id: EndpointId, with_event: bool) -> EndpointDescription {
    let mut description = light(id);
    description
        .clusters
        .push(bridged_device_cluster(with_event));
    description
        .initial_values
        .push((bridged_device::CLUSTER, NODE_LABEL, Value::from("Kitchen")));
    description.initial_values.push((
        bridged_device::CLUSTER,
        bridged_device::REACHABLE,
        Value::Bool(true),
    ));
    description
}

fn open_basic(state: CommissioningState) -> bool {
    state
        == CommissioningState {
            commissioned: false,
            window: Some(Window::Basic),
        }
}

/// Rule: a node starts, stops with [`Event::Stopped`] as its last event, and the same engine
/// starts again on the same storage; endpoints are not persisted, fabrics are.
///
/// A fresh node has no fabrics and a basic window open. After the stop every method answers
/// [`Error::NotRunning`]. The second start finds no fabrics and takes the endpoint again, which
/// shows that the first node's endpoints were not kept.
pub fn start_stop_restart(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let storage = eut.storage();
    let node = start_on(eut, &*engine, Arc::clone(&storage));
    assert!(
        open_basic(run(eut, node.commissioning_state()).unwrap()),
        "a fresh node has a basic window open"
    );
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    stop(eut, &*node);
    assert_eq!(run(eut, node.next_event()), Err(Error::NotRunning));
    assert_eq!(run(eut, node.stop()), Err(Error::NotRunning));

    let node = start_on(eut, &*engine, storage);
    assert_eq!(run(eut, node.fabrics()).unwrap(), vec![]);
    run(eut, node.add_endpoint(light(LIGHT))).expect("endpoints are not persisted");
    stop(eut, &*node);
}

/// Rule: an engine runs one node at a time; a second start is [`Error::AlreadyRunning`] and
/// changes nothing, and after the stop the engine starts again.
pub fn second_start_is_already_running(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let second = run(
        eut,
        engine.start(
            eut.identity(),
            eut.network(),
            eut.attestation(),
            NodeRole::Bridge {
                aggregator: eut.aggregator(),
            },
            eut.storage(),
        ),
    );
    assert!(
        matches!(second, Err(Error::AlreadyRunning)),
        "a second start while a node runs is refused"
    );
    // The first node is unaffected.
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    stop(eut, &*node);
    let again = start_on(eut, &*engine, eut.storage());
    stop(eut, &*again);
}

/// Rule: an endpoint id in use, the root endpoint and the aggregator endpoint are refused:
/// [`Error::DuplicateEndpoint`] for an id that exists (the aggregator included), and
/// [`Error::InvalidDescription`] naming the root for endpoint 0.
pub fn duplicate_endpoint_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    assert_eq!(
        run(eut, node.add_endpoint(light(LIGHT))),
        Err(Error::DuplicateEndpoint(LIGHT))
    );
    let aggregator = eut.aggregator();
    assert_eq!(
        run(eut, node.add_endpoint(light(aggregator))),
        Err(Error::DuplicateEndpoint(aggregator))
    );
    assert_error(
        &run(eut, node.add_endpoint(light(EndpointId::new(0)))),
        "adding endpoint 0",
        |error| {
            matches!(error, Error::InvalidDescription { endpoint, reason }
                if *endpoint == EndpointId::new(0) && reason.contains("root"))
        },
    );
    stop(eut, &*node);
}

/// Rule: `parent` must name an endpoint that exists: the aggregator or an endpoint the
/// product added; anything else is [`Error::UnknownEndpoint`] naming the parent.
pub fn unknown_parent_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let missing = EndpointId::new(7);
    assert_eq!(
        run(
            eut,
            node.add_endpoint(EndpointDescription {
                parent: Some(missing),
                ..light(LIGHT)
            })
        ),
        Err(Error::UnknownEndpoint(missing))
    );
    // The refused endpoint was not added: its id is still free.
    run(
        eut,
        node.add_endpoint(EndpointDescription {
            parent: Some(eut.aggregator()),
            ..light(LIGHT)
        }),
    )
    .expect("the aggregator is a valid parent");
    run(
        eut,
        node.add_endpoint(EndpointDescription {
            parent: Some(LIGHT),
            ..light(EndpointId::new(3))
        }),
    )
    .expect("an endpoint the product added is a valid parent");
    stop(eut, &*node);
}

/// Rule: every listed attribute needs an initial value; the engine never invents one. A
/// description without one is [`Error::InvalidDescription`] naming the attribute, and nothing
/// is added.
pub fn missing_initial_value_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let without_level = EndpointDescription {
        initial_values: vec![(ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))],
        ..light(LIGHT)
    };
    assert_error(
        &run(eut, node.add_endpoint(without_level)),
        "an attribute without an initial value",
        |error| {
            matches!(error, Error::InvalidDescription { endpoint, reason }
                if *endpoint == LIGHT
                    && reason.contains("no initial value")
                    && reason.contains(&CURRENT_LEVEL.to_string())
                    && reason.contains(&LEVEL.to_string()))
        },
    );
    run(eut, node.add_endpoint(light(LIGHT))).expect("the id is still free");
    stop(eut, &*node);
}

/// Rule: the global attributes are derived by the engine; a description that lists one is
/// [`Error::InvalidDescription`] and nothing is added.
pub fn global_attribute_in_list_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let mut with_global = light(LIGHT);
    with_global.clusters[0].attributes.push(CLUSTER_REVISION);
    with_global
        .initial_values
        .push((ON_OFF, CLUSTER_REVISION, Value::U16(6)));
    assert_error(
        &run(eut, node.add_endpoint(with_global)),
        "a global attribute in the attribute list",
        |error| {
            matches!(error, Error::InvalidDescription { endpoint, reason }
                if *endpoint == LIGHT && reason.contains("global"))
        },
    );
    run(eut, node.add_endpoint(light(LIGHT))).expect("the id is still free");
    stop(eut, &*node);
}

/// Rule: after [`Running::remove_endpoint`](crate::Running::remove_endpoint) the id is free
/// again; a second removal is [`Error::UnknownEndpoint`], and the id can be added anew.
pub fn endpoint_can_be_re_added(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    run(
        eut,
        node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true)),
    )
    .unwrap();
    run(eut, node.remove_endpoint(LIGHT)).unwrap();
    assert_eq!(
        run(eut, node.remove_endpoint(LIGHT)),
        Err(Error::UnknownEndpoint(LIGHT))
    );
    assert_eq!(
        run(
            eut,
            node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false))
        ),
        Err(Error::UnknownEndpoint(LIGHT))
    );
    run(eut, node.add_endpoint(light(LIGHT))).expect("the id can be used again");
    stop(eut, &*node);
}

/// Rule: removing an endpoint removes every endpoint whose `parent` chain leads to it, and
/// nothing else.
pub fn remove_cascades_to_composed_parts(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let part = EndpointId::new(3);
    let part_of_part = EndpointId::new(4);
    let other = EndpointId::new(5);
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    run(
        eut,
        node.add_endpoint(EndpointDescription {
            parent: Some(LIGHT),
            ..light(part)
        }),
    )
    .unwrap();
    run(
        eut,
        node.add_endpoint(EndpointDescription {
            parent: Some(part),
            ..light(part_of_part)
        }),
    )
    .unwrap();
    run(eut, node.add_endpoint(light(other))).unwrap();

    run(eut, node.remove_endpoint(LIGHT)).unwrap();
    for removed in [LIGHT, part, part_of_part] {
        assert_eq!(
            run(eut, node.remove_endpoint(removed)),
            Err(Error::UnknownEndpoint(removed)),
            "endpoint {removed} went with its parent"
        );
    }
    run(
        eut,
        node.set_attribute(other, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true)),
    )
    .expect("an unrelated endpoint stays");
    // The ids are free again.
    run(eut, node.add_endpoint(light(part))).unwrap();
    stop(eut, &*node);
}

/// Rule: a path the node does not know is refused with the error that names what is
/// missing: [`Error::UnknownEndpoint`] for an endpoint the product did not add (the engine's
/// own endpoints included), [`Error::UnknownAttribute`] for a cluster or attribute the endpoint
/// does not list, [`Error::UnknownEvent`] for an event it does not list.
pub fn set_attribute_on_unknown_path_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    let unknown = EndpointId::new(99);
    assert_eq!(
        run(
            eut,
            node.set_attribute(unknown, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))
        ),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        run(eut, node.remove_endpoint(unknown)),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        run(
            eut,
            node.emit_event(unknown, ON_OFF, EventId::new(0), Value::Null)
        ),
        Err(Error::UnknownEndpoint(unknown))
    );
    assert_eq!(
        run(eut, node.set_reachable(unknown, false)),
        Err(Error::UnknownEndpoint(unknown))
    );
    let aggregator = eut.aggregator();
    assert_eq!(
        run(eut, node.remove_endpoint(aggregator)),
        Err(Error::UnknownEndpoint(aggregator)),
        "the engine's own endpoints are not the product's to remove"
    );

    let unknown_attribute = AttributeId::new(0x4000);
    assert_eq!(
        run(
            eut,
            node.set_attribute(LIGHT, ON_OFF, unknown_attribute, Value::Bool(true))
        ),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: unknown_attribute,
        })
    );
    let unknown_cluster = ClusterId::new(0x0300);
    assert_eq!(
        run(
            eut,
            node.set_attribute(LIGHT, unknown_cluster, ON_OFF_ATTRIBUTE, Value::Bool(true))
        ),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: unknown_cluster,
            attribute: ON_OFF_ATTRIBUTE,
        })
    );
    assert_eq!(
        run(
            eut,
            node.emit_event(LIGHT, ON_OFF, EventId::new(0), Value::Null)
        ),
        Err(Error::UnknownEvent {
            endpoint: LIGHT,
            cluster: ON_OFF,
            event: EventId::new(0),
        })
    );
    // The known path still works.
    run(
        eut,
        node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true)),
    )
    .unwrap();
    stop(eut, &*node);
}

/// Rule: [`Running::set_attributes`](crate::Running::set_attributes) is applied completely or
/// not at all: a batch with one unknown path fails with the error for that path, and a batch of
/// known paths succeeds. (That the values of a refused batch stay unchanged is visible only to
/// a client; the interop scenarios cover it.)
pub fn set_attributes_is_atomic(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    let unknown = AttributeId::new(0x4000);
    let mixed = vec![
        AttributeValue {
            endpoint: LIGHT,
            cluster: ON_OFF,
            attribute: ON_OFF_ATTRIBUTE,
            value: Value::Bool(true),
        },
        AttributeValue {
            endpoint: LIGHT,
            cluster: LEVEL,
            attribute: unknown,
            value: Value::U8(1),
        },
    ];
    assert_eq!(
        run(eut, node.set_attributes(mixed)),
        Err(Error::UnknownAttribute {
            endpoint: LIGHT,
            cluster: LEVEL,
            attribute: unknown,
        })
    );
    let valid = vec![
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
    run(eut, node.set_attributes(valid)).expect("a batch of known paths");
    stop(eut, &*node);
}

/// Rule: capacity is reported, not hidden. [`Engine::capabilities`] and
/// [`Running::capabilities`](crate::Running::capabilities) agree; an endpoint beyond
/// `max_endpoints` is [`Error::CapacityExceeded`] and changes nothing; an enhanced window is
/// [`Error::Unsupported`] exactly when `enhanced_window` is `false`.
///
/// The exhaustion check runs when the limit is at most 16 endpoints, so that an engine with a
/// large limit does not spend the test adding hundreds of endpoints.
pub fn capacity_matches_capabilities(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let expected = engine.capabilities();
    let node = start_on(eut, &*engine, eut.storage());
    assert_eq!(node.capabilities(), expected);

    if let Some(max) = expected.max_endpoints
        && max <= 16
    {
        let ids: Vec<EndpointId> = (0..max)
            .map(|i| EndpointId::new(u16::try_from(i + 2).expect("a small id")))
            .collect();
        for id in &ids {
            run(eut, node.add_endpoint(light(*id))).unwrap();
        }
        let beyond = EndpointId::new(u16::try_from(max + 2).expect("a small id"));
        assert_eq!(
            run(eut, node.add_endpoint(light(beyond))),
            Err(Error::CapacityExceeded { what: "endpoints" })
        );
        assert_eq!(
            run(eut, node.remove_endpoint(beyond)),
            Err(Error::UnknownEndpoint(beyond)),
            "the refused endpoint was not added"
        );
        run(eut, node.remove_endpoint(ids[0])).unwrap();
        run(eut, node.add_endpoint(light(beyond))).expect("room is freed by removing");
    }

    // The basic window a fresh node opens is closed first, so that the outcome of opening an
    // enhanced one does not depend on how an engine treats a second window.
    run(eut, node.close_commissioning_window()).unwrap();
    assert!(matches!(
        next_event(eut, &*node),
        Event::CommissioningChanged(_)
    ));
    let enhanced = run(
        eut,
        node.open_commissioning_window(Window::Enhanced, Duration::from_secs(300)),
    );
    if expected.enhanced_window {
        enhanced.expect("the engine reports enhanced windows as supported");
        assert_eq!(
            next_event(eut, &*node),
            Event::CommissioningChanged(CommissioningState {
                commissioned: false,
                window: Some(Window::Enhanced),
            })
        );
    } else {
        assert_eq!(
            enhanced,
            Err(Error::Unsupported {
                operation: "enhanced commissioning window"
            })
        );
    }
    stop(eut, &*node);
}

/// Rule: the product reads [`Running::commissioning_state`](crate::Running::commissioning_state)
/// instead of assuming it. A node without fabrics starts with a basic window open and reports
/// every change the same way the events do.
pub fn commissioning_state_readable(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let state = run(eut, node.commissioning_state()).unwrap();
    assert!(open_basic(state), "fresh node: {state:?}");
    assert_eq!(run(eut, node.fabrics()).unwrap(), vec![]);
    run(eut, node.close_commissioning_window()).unwrap();
    let closed = CommissioningState {
        commissioned: false,
        window: None,
    };
    assert_eq!(next_event(eut, &*node), Event::CommissioningChanged(closed));
    assert_eq!(run(eut, node.commissioning_state()).unwrap(), closed);
    stop(eut, &*node);
}

/// Rule: opening and closing a basic window are reported as
/// [`Event::CommissioningChanged`]; closing a closed window is not a change and produces no
/// event.
pub fn basic_window_open_close_are_events(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let open = CommissioningState {
        commissioned: false,
        window: Some(Window::Basic),
    };
    let closed = CommissioningState {
        commissioned: false,
        window: None,
    };
    run(eut, node.close_commissioning_window()).unwrap();
    assert_eq!(next_event(eut, &*node), Event::CommissioningChanged(closed));
    // Closing again changes nothing: the next event is the one for the open below.
    run(eut, node.close_commissioning_window()).unwrap();
    run(
        eut,
        node.open_commissioning_window(Window::Basic, Duration::from_secs(300)),
    )
    .unwrap();
    assert_eq!(next_event(eut, &*node), Event::CommissioningChanged(open));
    assert_eq!(run(eut, node.commissioning_state()).unwrap(), open);
    stop(eut, &*node);
}

/// Rule: [`Running::pairing_info`](crate::Running::pairing_info) carries the QR text and the
/// manual code as [`Secret`]s whose `Debug` output is redacted, reports the identity's
/// discriminator, and answers the same values on every call while the window is unchanged.
pub fn pairing_info_redacted_and_stable(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    let info = run(eut, node.pairing_info()).unwrap();
    assert_eq!(info.discriminator, eut.identity().discriminator);
    assert!(
        info.qr_text.expose().starts_with("MT:"),
        "the QR text is an onboarding payload"
    );
    assert!(!info.manual_code.expose().is_empty());
    let output = format!("{info:?}");
    assert!(output.contains(REDACTED), "{output}");
    assert!(!output.contains(info.manual_code.expose()), "{output}");
    assert!(!output.contains(info.qr_text.expose()), "{output}");
    let passcode = eut.identity().passcode.expose().to_string();
    assert!(!output.contains(&passcode), "{output}");
    assert_eq!(run(eut, node.pairing_info()).unwrap(), info);
    stop(eut, &*node);
}

/// Rule: [`Running::factory_reset`](crate::Running::factory_reset) removes every fabric and
/// all persisted engine state and leaves the node running without fabrics; reopening a window
/// is the product's decision. The product's own keys stay.
///
/// The case runs on an uncommissioned node, so no fabric is removed; the engine may or may not
/// report a [`Event::CommissioningChanged`] for the reset itself when nothing about the window
/// changed, and the case accepts both. What it insists on: the engine's keys are gone, the
/// product's key is kept, the state reads as not commissioned, and the window opens again on
/// request with its event.
pub fn factory_reset_reopens_window_and_clears_engine_keys(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let recording = Arc::new(RecordingStorage::new(eut.storage()));
    recording.store("product/kept", b"1").unwrap();
    let storage: Arc<dyn Storage> = Arc::clone(&recording) as Arc<dyn Storage>;
    let node = start_on(eut, &*engine, storage);
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    run(eut, node.close_commissioning_window()).unwrap();
    assert!(matches!(
        next_event(eut, &*node),
        Event::CommissioningChanged(_)
    ));

    run(eut, node.factory_reset()).unwrap();
    assert_eq!(
        recording.present(),
        vec!["product/kept"],
        "only the product's key survives a factory reset"
    );
    assert_eq!(run(eut, node.fabrics()).unwrap(), vec![]);
    let state = run(eut, node.commissioning_state()).unwrap();
    assert!(!state.commissioned, "{state:?}");

    // The node keeps running: values can be set and the window reopened.
    run(
        eut,
        node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true)),
    )
    .unwrap();
    run(
        eut,
        node.open_commissioning_window(Window::Basic, Duration::from_secs(300)),
    )
    .unwrap();
    let open = CommissioningState {
        commissioned: false,
        window: Some(Window::Basic),
    };
    let mut event = next_event(eut, &*node);
    if event
        == Event::CommissioningChanged(CommissioningState {
            commissioned: false,
            window: None,
        })
    {
        // The engine reported the reset itself; the open follows.
        event = next_event(eut, &*node);
    }
    assert_eq!(event, Event::CommissioningChanged(open));
    stop(eut, &*node);
}

/// Rule of the storage contract: the engine keeps its state under keys that start with
/// `engine/` and touches nothing else, so that the product's keys never collide with it.
pub fn engine_keys_are_namespaced(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let recording = Arc::new(RecordingStorage::new(eut.storage()));
    let storage: Arc<dyn Storage> = Arc::clone(&recording) as Arc<dyn Storage>;
    let node = start_on(eut, &*engine, Arc::clone(&storage));
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    run(eut, node.close_commissioning_window()).unwrap();
    assert!(matches!(
        next_event(eut, &*node),
        Event::CommissioningChanged(_)
    ));
    run(
        eut,
        node.open_commissioning_window(Window::Basic, Duration::from_secs(300)),
    )
    .unwrap();
    assert!(matches!(
        next_event(eut, &*node),
        Event::CommissioningChanged(_)
    ));
    stop(eut, &*node);
    let node = start_on(eut, &*engine, storage);
    stop(eut, &*node);

    let outside: Vec<alloc::string::String> = recording
        .touched()
        .into_iter()
        .filter(|key| !key.starts_with("engine/"))
        .collect();
    assert!(
        outside.is_empty(),
        "the engine touched keys outside its namespace: {outside:?}"
    );
}

/// Rule: [`Event::Stopped`] is the last event; after it, and whenever the node is not running,
/// every method answers [`Error::NotRunning`] instead of waiting.
pub fn stop_ends_event_stream(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    stop(eut, &*node);
    assert_eq!(run(eut, node.next_event()), Err(Error::NotRunning));
    assert_eq!(run(eut, node.stop()), Err(Error::NotRunning));
    assert_eq!(
        run(eut, node.respond(Correlation::new(1), Response::success())),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(eut, node.add_endpoint(light(EndpointId::new(3)))),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(eut, node.remove_endpoint(LIGHT)),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(
            eut,
            node.set_attribute(LIGHT, ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(true))
        ),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(eut, node.set_attributes(vec![])),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(
            eut,
            node.emit_event(LIGHT, ON_OFF, EventId::new(0), Value::Null)
        ),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(eut, node.set_reachable(LIGHT, true)),
        Err(Error::NotRunning)
    );
    assert_eq!(run(eut, node.commissioning_state()), Err(Error::NotRunning));
    assert_eq!(
        run(
            eut,
            node.open_commissioning_window(Window::Basic, Duration::from_secs(300))
        ),
        Err(Error::NotRunning)
    );
    assert_eq!(
        run(eut, node.close_commissioning_window()),
        Err(Error::NotRunning)
    );
    assert_eq!(run(eut, node.fabrics()), Err(Error::NotRunning));
    assert_eq!(
        run(eut, node.remove_fabric(crate::id::FabricIndex::new(1))),
        Err(Error::NotRunning)
    );
    assert_eq!(run(eut, node.pairing_info()), Err(Error::NotRunning));
    assert_eq!(run(eut, node.factory_reset()), Err(Error::NotRunning));
}

/// Rule: every command and write is answered exactly once through its correlation; a
/// response to a correlation nothing waits for is [`Error::UnknownCorrelation`].
pub fn respond_unknown_correlation_rejected(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    let correlation = Correlation::new(42);
    assert_eq!(
        run(eut, node.respond(correlation, Response::success())),
        Err(Error::UnknownCorrelation(correlation))
    );
    stop(eut, &*node);
}

/// Rule: [`Running::set_reachable`](crate::Running::set_reachable) sets `Reachable` and emits
/// `ReachableChanged` of the Bridged Device Basic Information cluster in one request; the
/// endpoint must list that cluster with the attribute and the event, otherwise the request is
/// refused like a `set_attribute` or an `emit_event` for a path that is not listed.
pub fn set_reachable_on_bridged_endpoint(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let node = start_on(eut, &*engine, eut.storage());
    run(eut, node.add_endpoint(light(LIGHT))).unwrap();
    assert_error(
        &run(eut, node.set_reachable(LIGHT, false)),
        "set_reachable on an endpoint without the cluster",
        |error| {
            matches!(
                error,
                Error::UnknownAttribute { endpoint, cluster, .. } | Error::UnknownEvent { endpoint, cluster, .. }
                    if *endpoint == LIGHT && *cluster == bridged_device::CLUSTER
            )
        },
    );

    let without_event = EndpointId::new(3);
    run(eut, node.add_endpoint(bridged_light(without_event, false))).unwrap();
    assert_error(
        &run(eut, node.set_reachable(without_event, false)),
        "set_reachable on an endpoint without the event",
        |error| {
            matches!(
                error,
                Error::UnknownEvent { endpoint, cluster, event }
                    if *endpoint == without_event
                        && *cluster == bridged_device::CLUSTER
                        && *event == bridged_device::REACHABLE_CHANGED
            )
        },
    );

    let bridged = EndpointId::new(4);
    run(eut, node.add_endpoint(bridged_light(bridged, true))).unwrap();
    run(eut, node.set_reachable(bridged, false)).expect("reachable false");
    run(eut, node.set_reachable(bridged, true)).expect("reachable true");
    // The same two steps by hand.
    run(
        eut,
        node.set_attribute(
            bridged,
            bridged_device::CLUSTER,
            bridged_device::REACHABLE,
            Value::Bool(false),
        ),
    )
    .unwrap();
    run(
        eut,
        node.emit_event(
            bridged,
            bridged_device::CLUSTER,
            bridged_device::REACHABLE_CHANGED,
            Value::Struct(vec![(
                bridged_device::REACHABLE_NEW_VALUE_TAG,
                Value::Bool(false),
            )]),
        ),
    )
    .unwrap();
    stop(eut, &*node);
}

/// Rule: errors name ids, never secrets. An identity with a forbidden passcode is refused
/// with [`Error::InvalidIdentity`] whose text names the passcode as the reason without
/// containing it; the same for an oversized discriminator and a network configuration without
/// an address family. After a refused start the engine is free.
pub fn errors_never_contain_the_passcode(eut: &dyn EngineUnderTest) {
    let engine = eut.engine();
    let start_with = |identity: crate::NodeIdentity, network: NetworkConfig| {
        run(
            eut,
            engine.start(
                identity,
                network,
                eut.attestation(),
                NodeRole::Bridge {
                    aggregator: eut.aggregator(),
                },
                eut.storage(),
            ),
        )
        .map(|_| ())
    };

    let forbidden = 12_345_678_u32;
    let mut forbidden_passcode = eut.identity();
    forbidden_passcode.passcode = Secret::new(forbidden);
    let error = start_with(forbidden_passcode, eut.network()).unwrap_err();
    match &error {
        Error::InvalidIdentity { reason } => {
            assert!(reason.contains("passcode"), "{reason}");
            let texts = [reason.clone(), error.to_string(), format!("{error:?}")];
            for text in texts {
                assert!(
                    !text.contains(&forbidden.to_string()),
                    "the passcode leaked: {text}"
                );
            }
        }
        other => panic!("unexpected error {other:?}"),
    }

    let mut wide_discriminator = eut.identity();
    wide_discriminator.discriminator = 0x1000;
    let error = start_with(wide_discriminator, eut.network()).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidIdentity { reason } if reason.contains("discriminator")),
        "{error:?}"
    );

    let no_family = NetworkConfig {
        ipv4: false,
        ipv6: false,
        ..eut.network()
    };
    let error = start_with(eut.identity(), no_family).unwrap_err();
    assert!(
        matches!(&error, Error::InvalidIdentity { reason } if reason.contains("IPv4")),
        "{error:?}"
    );

    let node = start_on(eut, &*engine, eut.storage());
    stop(eut, &*node);
}
