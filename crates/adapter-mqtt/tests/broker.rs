//! The adapter against an in-process rumqttd broker: values in both directions, both payload
//! forms, reachability, the bridge instance's own topics, broker loss and recovery, and the
//! secrecy of the password.
//!
//! Every test starts its own broker on a free loopback port, its own bridge on the fake engine
//! and its own adapter under the supervisor, so the tests run in parallel. A test client plays
//! the source adapter (`hue`) and watches what the adapter publishes.

mod support;

use std::time::Duration;

use matter_rs_adapter_api::Health;
use matter_rs_adapter_mqtt::NodeState;
use matter_rs_bridge::DeviceKind;
use matter_rs_engine::{CommandId, EndpointId, Status, Value};
use serde_json::json;

use support::{
    BRIDGED_DEVICE, Harness, LEVEL_CONTROL, MEASURED, ON_OFF, Proxy, REACHABLE, TEMPERATURE,
    TestClient, config, device, fields, start_broker, two_devices,
};

const LAMP: u16 = 2;
const THERMOMETER: u16 = 3;
const ON: CommandId = CommandId::new(0x01);
const MOVE_TO_LEVEL_WITH_ON_OFF: CommandId = CommandId::new(0x04);

/// Starts the broker, a source client with `hue/connected` = 2 retained, and the adapter with
/// the two devices; waits until the adapter is healthy.
async fn lamp_and_thermometer(extra: &str) -> (u16, TestClient, Harness) {
    let port = start_broker();
    let source = TestClient::connect(port, "hue").await;
    source.publish("hue/connected", "2", true).await;
    let harness = Harness::start(config(port, extra), two_devices()).await;
    harness.wait_health(&Health::Healthy).await;
    (port, source, harness)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_to_attribute() {
    let (_port, source, harness) = lamp_and_thermometer("").await;
    // The devices exist, unreachable and with placeholders, before any value arrived.
    assert_eq!(harness.reachable(LAMP), Some(Value::Bool(false)));
    assert_eq!(
        harness
            .fake
            .read(EndpointId::new(LAMP), LEVEL_CONTROL, MEASURED),
        Some(Value::Null)
    );

    source.publish("hue/status/lamp/on_off", "true", true).await;
    source
        .publish(
            "hue/status/lamp/level",
            r#"{"val": 50, "ts": 1, "lc": 1}"#,
            true,
        )
        .await;
    harness
        .wait_value(LAMP, ON_OFF, MEASURED, &Value::Bool(true))
        .await;
    // 50 % of 1..=254 is 127.5, rounded half up.
    harness
        .wait_value(LAMP, LEVEL_CONTROL, MEASURED, &Value::U8(128))
        .await;
    // Both required attributes have values and the source is connected: reachable.
    harness
        .wait_value(LAMP, BRIDGED_DEVICE, REACHABLE, &Value::Bool(true))
        .await;
    // A value the mapping cannot read is ignored, the last good value stands.
    source
        .publish("hue/status/lamp/level", "bright", false)
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        harness
            .fake
            .read(EndpointId::new(LAMP), LEVEL_CONTROL, MEASURED),
        Some(Value::U8(128))
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_to_set_without_echo() {
    let (port, source, harness) = lamp_and_thermometer("").await;
    source
        .publish("hue/status/lamp/on_off", "false", true)
        .await;
    source.publish("hue/status/lamp/level", "10", true).await;
    harness
        .wait_value(LAMP, BRIDGED_DEVICE, REACHABLE, &Value::Bool(true))
        .await;
    let mut watcher = TestClient::connect(port, "watcher").await;
    watcher.subscribe("hue/#").await;
    // The retained status items arrive first; they are the source's, not the adapter's.
    let _ = watcher.collect(Duration::from_millis(300)).await;

    let response = harness
        .fake
        .invoke(EndpointId::new(LAMP), ON_OFF, ON, Value::Null)
        .await
        .expect("the command is answered");
    assert_eq!(response.status, Status::Success);
    let set = watcher.expect("hue/set/lamp/on_off").await;
    assert_eq!(set.text(), "true");
    assert!(!set.retain, "a set is never retained");

    let response = harness
        .fake
        .invoke(
            EndpointId::new(LAMP),
            LEVEL_CONTROL,
            MOVE_TO_LEVEL_WITH_ON_OFF,
            fields(&[
                (0, Value::U8(254)),
                (1, Value::Null),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        )
        .await
        .expect("the command is answered");
    assert_eq!(response.status, Status::Success);
    // The inverse of the percentage scale: 254 is 100.
    let level = watcher.expect("hue/set/lamp/level").await;
    assert_eq!(level.text(), "100");
    let on_off = watcher.expect("hue/set/lamp/on_off").await;
    assert_eq!(on_off.text(), "true");

    // No status echo: nothing else appears under the source's topics.
    let rest = watcher.collect(Duration::from_millis(500)).await;
    assert!(
        rest.iter().all(|m| !m.topic.starts_with("hue/status/")),
        "status echoed: {rest:?}"
    );
    // The bridge still serves the values the commands set.
    assert_eq!(
        harness
            .fake
            .read(EndpointId::new(LAMP), LEVEL_CONTROL, MEASURED),
        Some(Value::U8(254))
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_json_wraps_the_value() {
    let (port, source, harness) = lamp_and_thermometer("set_json = true\n").await;
    source
        .publish("hue/status/lamp/on_off", "false", true)
        .await;
    let mut watcher = TestClient::connect(port, "watcher").await;
    watcher.subscribe("hue/set/#").await;
    harness
        .wait_value(LAMP, ON_OFF, MEASURED, &Value::Bool(false))
        .await;
    harness
        .fake
        .invoke(EndpointId::new(LAMP), ON_OFF, ON, Value::Null)
        .await
        .expect("answered");
    let set = watcher.expect("hue/set/lamp/on_off").await;
    assert_eq!(set.json(), json!({"val": true}));
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_payload_forms_accepted() {
    let (_port, source, harness) = lamp_and_thermometer("").await;
    source
        .publish("hue/status/thermometer/temperature", "21.5", true)
        .await;
    harness
        .wait_value(THERMOMETER, TEMPERATURE, MEASURED, &Value::I16(2150))
        .await;
    source
        .publish(
            "hue/status/thermometer/temperature",
            r#"{"val": 22.25, "ts": 1700000000000, "lc": 1700000000000, "hue": {"battery": 90}}"#,
            true,
        )
        .await;
    harness
        .wait_value(THERMOMETER, TEMPERATURE, MEASURED, &Value::I16(2225))
        .await;
    // An empty payload clears a nullable measurement.
    source
        .publish("hue/status/thermometer/temperature", "", true)
        .await;
    harness
        .wait_value(THERMOMETER, TEMPERATURE, MEASURED, &Value::Null)
        .await;
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reachable_follows_source_connected() {
    let (_port, source, harness) = lamp_and_thermometer("").await;
    source
        .publish("hue/status/thermometer/temperature", "20", true)
        .await;
    harness
        .wait_value(THERMOMETER, BRIDGED_DEVICE, REACHABLE, &Value::Bool(true))
        .await;
    source.publish("hue/connected", "0", true).await;
    harness
        .wait_value(THERMOMETER, BRIDGED_DEVICE, REACHABLE, &Value::Bool(false))
        .await;
    // `1` means the source adapter runs without its hardware: still unreachable.
    source.publish("hue/connected", "1", true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.reachable(THERMOMETER), Some(Value::Bool(false)));
    source.publish("hue/connected", "2", true).await;
    harness
        .wait_value(THERMOMETER, BRIDGED_DEVICE, REACHABLE, &Value::Bool(true))
        .await;
    // The lamp never got a level: it stays unreachable throughout.
    assert_eq!(harness.reachable(LAMP), Some(Value::Bool(false)));
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reachable_follows_online_item() {
    let port = start_broker();
    let source = TestClient::connect(port, "hue").await;
    source.publish("hue/connected", "2", true).await;
    source.publish("hue/status/plug/on_off", "on", true).await;
    let plug = device(
        "plug",
        DeviceKind::OnOffPlugInUnit,
        "source = \"hue/plug\"\nonline = \"online\"\n",
    );
    let harness = Harness::start(config(port, ""), vec![plug]).await;
    harness.wait_health(&Health::Healthy).await;
    harness
        .wait_value(LAMP, ON_OFF, MEASURED, &Value::Bool(true))
        .await;
    // Connected and valued, but the online item has not said anything: unreachable.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(harness.reachable(LAMP), Some(Value::Bool(false)));
    source.publish("hue/status/plug/online", "true", true).await;
    harness
        .wait_value(LAMP, BRIDGED_DEVICE, REACHABLE, &Value::Bool(true))
        .await;
    source
        .publish("hue/status/plug/online", r#"{"val": false}"#, true)
        .await;
    harness
        .wait_value(LAMP, BRIDGED_DEVICE, REACHABLE, &Value::Bool(false))
        .await;
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_instance_topics() {
    let port = start_broker();
    let mut watcher = TestClient::connect(port, "watcher").await;
    watcher.subscribe("matter/#").await;
    let harness = Harness::start(config(port, ""), two_devices()).await;
    harness.wait_health(&Health::Healthy).await;

    let connected = watcher.expect("matter/connected").await;
    assert_eq!(connected.text(), "2");

    let info = watcher.expect("matter/info").await.json();
    assert_eq!(info["name"], "matter-rs");
    assert_eq!(info["spec"], "2.0");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["maintenance"], false);
    assert!(info["pid"].is_number());
    assert!(info["started"].as_str().unwrap().ends_with('Z'));
    assert_eq!(info["matter"]["vendor_id"], 0xFFF1);
    assert_eq!(info["matter"]["product_id"], 0x8001);

    let object = watcher
        .expect("matter/status/commissioning/commissioned")
        .await
        .json();
    assert_eq!(object["val"], false);
    assert!(object["ts"].is_number() && object["lc"].is_number());
    assert_eq!(
        watcher
            .expect("matter/status/commissioning/window_open")
            .await
            .val(),
        json!(false)
    );
    assert_eq!(
        watcher
            .expect("matter/status/commissioning/fabric_count")
            .await
            .val(),
        json!(0)
    );
    let lamp = watcher.expect("matter/status/devices/lamp/reachable").await;
    assert_eq!(lamp.val(), json!(false));
    watcher
        .expect("matter/status/devices/thermometer/reachable")
        .await;

    // A consumer that starts now finds every item retained (a live delivery never carries the
    // retain flag, a delivery from the broker's store does).
    let mut late = TestClient::connect(port, "late").await;
    late.subscribe("matter/#").await;
    let stored = late.collect(Duration::from_secs(1)).await;
    for topic in [
        "matter/connected",
        "matter/info",
        "matter/status/commissioning/commissioned",
        "matter/status/commissioning/window_open",
        "matter/status/commissioning/fabric_count",
        "matter/status/devices/lamp/reachable",
        "matter/status/devices/thermometer/reachable",
    ] {
        let message = stored
            .iter()
            .find(|message| message.topic == topic)
            .unwrap_or_else(|| panic!("{topic} is not retained; stored: {stored:?}"));
        assert!(message.retain, "{topic} came live, not from the store");
    }
    assert_eq!(stored.len(), 7, "{stored:?}");

    // The host reports a fabric: the commissioning items follow.
    harness
        .node_state
        .send(Some(NodeState {
            commissioned: true,
            window_open: true,
            fabric_count: 1,
        }))
        .unwrap();
    assert_eq!(
        watcher
            .expect("matter/status/commissioning/commissioned")
            .await
            .val(),
        json!(true)
    );
    assert_eq!(
        watcher
            .expect("matter/status/commissioning/window_open")
            .await
            .val(),
        json!(true)
    );
    assert_eq!(
        watcher
            .expect("matter/status/commissioning/fabric_count")
            .await
            .val(),
        json!(1)
    );
    // No pairing code or QR text anywhere.
    let rest = watcher.collect(Duration::from_millis(300)).await;
    for message in &rest {
        assert!(!message.text().contains("MT:"), "{message:?}");
    }

    // A clean stop says goodbye explicitly.
    harness.shutdown().await;
    let goodbye = watcher.expect("matter/connected").await;
    assert_eq!(goodbye.text(), "0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_payloads_on_request() {
    let port = start_broker();
    let mut watcher = TestClient::connect(port, "watcher").await;
    watcher.subscribe("bridge/#").await;
    let harness = Harness::start(
        config(port, "name = \"bridge\"\njson_payloads = false\n"),
        Vec::new(),
    )
    .await;
    harness.wait_health(&Health::Healthy).await;
    assert_eq!(watcher.expect("bridge/connected").await.text(), "2");
    watcher.expect("bridge/info").await;
    assert_eq!(
        watcher
            .expect("bridge/status/commissioning/commissioned")
            .await
            .text(),
        "false"
    );
    assert_eq!(
        watcher
            .expect("bridge/status/commissioning/fabric_count")
            .await
            .text(),
        "0"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lwt_is_zero() {
    let broker = start_broker();
    let proxy = Proxy::start(broker).await;
    let mut watcher = TestClient::connect(broker, "watcher").await;
    watcher.subscribe("matter/connected").await;
    let harness = Harness::start(config(proxy.port, ""), Vec::new()).await;
    harness.wait_health(&Health::Healthy).await;
    assert_eq!(watcher.expect("matter/connected").await.text(), "2");

    // The connection dies without a goodbye: the broker publishes the will, retained.
    proxy.cut();
    let will = watcher.expect("matter/connected").await;
    assert_eq!(will.text(), "0");
    let mut late = TestClient::connect(broker, "late").await;
    late.subscribe("matter/connected").await;
    let stored = late.expect("matter/connected").await;
    assert_eq!(stored.text(), "0");
    assert!(stored.retain);
    harness.wait_degraded().await;
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_restores_subscriptions_and_state() {
    let broker = start_broker();
    let proxy = Proxy::start(broker).await;
    let source = TestClient::connect(broker, "hue").await;
    source.publish("hue/connected", "2", true).await;
    let mut watcher = TestClient::connect(broker, "watcher").await;
    watcher.subscribe("matter/#").await;
    let harness = Harness::start(config(proxy.port, ""), two_devices()).await;
    harness.wait_health(&Health::Healthy).await;
    assert_eq!(watcher.expect("matter/connected").await.text(), "2");
    watcher.expect("matter/info").await;
    source
        .publish("hue/status/thermometer/temperature", "20", true)
        .await;
    harness
        .wait_value(THERMOMETER, TEMPERATURE, MEASURED, &Value::I16(2000))
        .await;
    let _ = watcher.collect(Duration::from_millis(300)).await;

    // The broker goes away: degraded, the will fires, the devices keep their last state.
    proxy.cut();
    assert_eq!(watcher.expect("matter/connected").await.text(), "0");
    harness.wait_degraded().await;
    assert_eq!(harness.reachable(THERMOMETER), Some(Value::Bool(true)));
    // Several reconnect attempts fail meanwhile (backoff 50 ms, 100 ms, 200 ms, ...).
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(matches!(harness.health(), Health::Degraded(_)));

    // The broker is back: `connected` and `info` are published anew and the subscriptions
    // work again.
    proxy.restore();
    harness.wait_health(&Health::Healthy).await;
    assert_eq!(watcher.expect("matter/connected").await.text(), "2");
    watcher.expect("matter/info").await;
    source
        .publish("hue/status/thermometer/temperature", "23", true)
        .await;
    harness
        .wait_value(THERMOMETER, TEMPERATURE, MEASURED, &Value::I16(2300))
        .await;
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_never_contain_the_password() {
    let capture = support::LogCapture::install();
    let port = start_broker();
    let harness = Harness::start(
        config(
            port,
            "username = \"bridge\"\npassword = \"hunter2-very-secret\"\n",
        ),
        two_devices(),
    )
    .await;
    harness.wait_health(&Health::Healthy).await;
    harness.shutdown().await;

    // A second instance with the credentials in the URL and a broker that is not there, so the
    // failure path logs too.
    let mut failing: matter_rs_adapter_mqtt::Config = toml::from_str(&format!(
        "url = \"mqtt://other:url-secret-9@127.0.0.1:{}\"\nreconnect = {{ initial = 0.05, max = 0.1 }}\n",
        support::free_port()
    ))
    .unwrap();
    failing.name = String::from("second");
    let harness = Harness::start(failing, Vec::new()).await;
    harness.wait_degraded().await;
    harness.shutdown().await;

    let lines = capture.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("connected to mqtt://127.0.0.1")),
        "the adapter logged nothing: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("failed")),
        "{lines:#?}"
    );
    for line in &lines {
        assert!(!line.contains("hunter2"), "password in the log: {line}");
        assert!(
            !line.contains("url-secret"),
            "URL password in the log: {line}"
        );
        assert!(
            !line.contains("other:"),
            "URL credentials in the log: {line}"
        );
    }
}
