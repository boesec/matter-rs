//! Feeds arbitrary bytes to everything the MQTT adapter parses from outside: a broker payload
//! through every transform and attribute type, a `mapping` table from the configuration, and a
//! broker URL. None of it may panic.
//!
//! Run with `cargo fuzz run mqtt_payload` from `crates/adapter-mqtt` (nightly toolchain, Linux).
//!
//! The crate cannot forbid unsafe code like the workspace crates do: the `fuzz_target!` macro
//! expands to the `#[unsafe(no_mangle)]` entry point that libFuzzer calls.

#![no_main]

use libfuzzer_sys::fuzz_target;
use matter_rs_adapter_mqtt::mapping::{DeviceDefinition, PartDefinition, resolve};
use matter_rs_adapter_mqtt::{BrokerUrl, Mapping, Transform, ValueType, attributes, payload};
use matter_rs_bridge::{DeviceKey, DeviceKind};

/// The transforms the payload runs through, besides the identity.
fn transforms() -> Vec<Transform> {
    vec![
        Transform::default(),
        Transform {
            scale: Some(matter_rs_adapter_mqtt::transform::Scale {
                min: 0.0,
                max: 100.0,
            }),
            invert: true,
            ..Transform::default()
        },
        Transform {
            factor: Some(100.0),
            ..Transform::default()
        },
        Transform {
            path: Some(String::from("/state/on")),
            ..Transform::default()
        },
        Transform {
            map: vec![
                (String::from("ON"), serde_json_true()),
                (String::from("1"), serde_json_one()),
            ],
            ..Transform::default()
        },
    ]
}

fn serde_json_true() -> serde_json::Value {
    serde_json::Value::Bool(true)
}

fn serde_json_one() -> serde_json::Value {
    serde_json::Value::from(1)
}

fuzz_target!(|data: &[u8]| {
    // 1. The bytes as a status payload, through every transform and attribute type.
    let parsed = payload::parse(data);
    for transform in transforms() {
        for attribute in attributes::ALL {
            let _ = transform.inbound(attribute.value_type, parsed.clone());
        }
        let _ = transform.inbound(ValueType::Bool, parsed.clone());
    }
    if let Some(value) = &parsed {
        let _ = payload::to_bool(value);
        let _ = payload::to_f64(value);
        let _ = payload::scalar_text(value);
        let _ = payload::encode_plain(value);
    }

    // 2. The bytes as text: a mapping table and a broker URL.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = BrokerUrl::parse(text);
    if let Ok(mapping) = toml::from_str::<Mapping>(text) {
        for kind in DeviceKind::ALL {
            let definition = DeviceDefinition {
                key: DeviceKey::new("fuzz").expect("a valid key"),
                name: String::from("fuzz"),
                parts: vec![PartDefinition { kind, label: None }],
                mapping: mapping.clone(),
            };
            if let Ok(resolved) = resolve(&definition) {
                for item in &resolved.items {
                    let _ = item
                        .transform
                        .inbound(item.attribute.value_type, parsed.clone());
                }
            }
        }
    }
});
