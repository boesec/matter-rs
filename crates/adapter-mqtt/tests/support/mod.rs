//! Shared helpers of the integration tests: an in-process rumqttd broker, a TCP proxy that can
//! cut the connection, a plain MQTT client that plays the other adapters, a bridge on the fake
//! engine with the adapter under the supervisor, and a log capture.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use matter_rs_adapter_api::{AdapterId, Health, Supervisor, SupervisorOptions, TokioTimer};
use matter_rs_adapter_mqtt::{BridgeIdentity, Config, DeviceDefinition, MqttAdapter, NodeState};
use matter_rs_bridge::{Bridge, DeviceKey, DeviceKind, Options};
use matter_rs_engine::testing::FakeEngine;
use matter_rs_engine::{
    Attestation, AttributeId, ClusterId, EndpointId, MemoryStorage, NetworkConfig, NodeIdentity,
    ProductId, Secret, Value, VendorId,
};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub const VENDOR_ID: u16 = 0xFFF1;
pub const PRODUCT_ID: u16 = 0x8001;
pub const ON_OFF: ClusterId = ClusterId::new(0x0006);
pub const LEVEL_CONTROL: ClusterId = ClusterId::new(0x0008);
pub const TEMPERATURE: ClusterId = ClusterId::new(0x0402);
pub const BRIDGED_DEVICE: ClusterId = ClusterId::new(0x0039);
pub const REACHABLE: AttributeId = AttributeId::new(0x0011);
pub const MEASURED: AttributeId = AttributeId::new(0x0000);
pub const WAIT: Duration = Duration::from_secs(10);

/// A TCP port nobody listens on right now.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local address")
        .port()
}

/// Starts a rumqttd broker on a free port of the loopback interface and waits until it accepts
/// connections. The broker's threads live until the test process ends.
pub fn start_broker() -> u16 {
    let port = free_port();
    let listen: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut v4 = HashMap::new();
    v4.insert(
        String::from("1"),
        rumqttd::ServerSettings {
            name: String::from("v4-1"),
            listen,
            tls: None,
            next_connection_delay_ms: 1,
            connections: rumqttd::ConnectionSettings {
                connection_timeout_ms: 60_000,
                max_payload_size: 20_480,
                max_inflight_count: 100,
                auth: None,
                external_auth: None,
                dynamic_filters: true,
            },
        },
    );
    let config = rumqttd::Config {
        id: 0,
        router: rumqttd::RouterConfig {
            max_connections: 100,
            max_outgoing_packet_count: 200,
            max_segment_size: 104_857_600,
            max_segment_count: 10,
            ..rumqttd::RouterConfig::default()
        },
        v4: Some(v4),
        ..rumqttd::Config::default()
    };
    let mut broker = rumqttd::Broker::new(config);
    thread::Builder::new()
        .name(format!("rumqttd-{port}"))
        .spawn(move || {
            if let Err(error) = broker.start() {
                eprintln!("rumqttd stopped: {error}");
            }
        })
        .expect("spawn the broker thread");
    let deadline = Instant::now() + WAIT;
    while TcpStream::connect(listen).is_err() {
        assert!(Instant::now() < deadline, "the broker does not listen");
        thread::sleep(Duration::from_millis(20));
    }
    port
}

/// A TCP proxy in front of the broker: the adapter connects to the proxy, so a test can cut the
/// connection and refuse new ones (`cut`) and let them through again (`restore`).
pub struct Proxy {
    pub port: u16,
    enabled: Arc<AtomicBool>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Proxy {
    pub async fn start(target_port: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the proxy");
        let port = listener.local_addr().expect("proxy address").port();
        let enabled = Arc::new(AtomicBool::new(true));
        let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let accept = tokio::spawn({
            let enabled = Arc::clone(&enabled);
            let connections = Arc::clone(&connections);
            async move {
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        return;
                    };
                    if !enabled.load(Ordering::SeqCst) {
                        drop(client);
                        continue;
                    }
                    let Ok(mut upstream) =
                        TokioTcpStream::connect(("127.0.0.1", target_port)).await
                    else {
                        drop(client);
                        continue;
                    };
                    let task = tokio::spawn(async move {
                        let _ = copy_bidirectional(&mut client, &mut upstream).await;
                    });
                    connections.lock().unwrap().push(task);
                }
            }
        });
        Self {
            port,
            enabled,
            connections,
            accept,
        }
    }

    /// Drops every forwarded connection and refuses new ones.
    pub fn cut(&self) {
        self.enabled.store(false, Ordering::SeqCst);
        for task in self.connections.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    /// Lets connections through again.
    pub fn restore(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.cut();
        self.accept.abort();
    }
}

/// A received message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: bool,
}

impl Message {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.payload)
            .unwrap_or_else(|error| panic!("{} is not JSON ({error}): {}", self.topic, self.text()))
    }

    /// The `val` of an object-form status, or the plain value.
    pub fn val(&self) -> serde_json::Value {
        let json = self.json();
        json.get("val").cloned().unwrap_or(json)
    }
}

/// An MQTT client that plays the source adapters and watches what the adapter publishes.
pub struct TestClient {
    client: AsyncClient,
    messages: mpsc::UnboundedReceiver<Message>,
    acks: mpsc::UnboundedReceiver<()>,
    task: JoinHandle<()>,
}

impl TestClient {
    pub async fn connect(port: u16, id: &str) -> Self {
        let mut options = MqttOptions::new(id, "127.0.0.1", port);
        options.set_keep_alive(Duration::from_secs(5));
        let (client, mut eventloop) = AsyncClient::new(options, 64);
        let (messages_tx, messages) = mpsc::unbounded_channel();
        let (acks_tx, mut acks) = mpsc::unbounded_channel();
        let (connected_tx, connected) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connected_tx = Some(connected_tx);
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                        if let Some(tx) = connected_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    Ok(Event::Incoming(Incoming::Publish(publish))) => {
                        let _ = messages_tx.send(Message {
                            topic: publish.topic,
                            payload: publish.payload.to_vec(),
                            retain: publish.retain,
                        });
                    }
                    Ok(Event::Incoming(Incoming::SubAck(_))) => {
                        let _ = acks_tx.send(());
                    }
                    Ok(_) => {}
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        });
        tokio::time::timeout(WAIT, connected)
            .await
            .expect("the test client connects")
            .expect("the client task runs");
        // The acks channel is drained on every subscribe.
        while acks.try_recv().is_ok() {}
        Self {
            client,
            messages,
            acks,
            task,
        }
    }

    pub async fn subscribe(&mut self, filter: &str) {
        self.client
            .subscribe(filter, QoS::AtMostOnce)
            .await
            .expect("subscribe");
        tokio::time::timeout(WAIT, self.acks.recv())
            .await
            .expect("the subscription is acknowledged");
    }

    pub async fn publish(&self, topic: &str, payload: &str, retain: bool) {
        self.client
            .publish(topic, QoS::AtMostOnce, retain, payload)
            .await
            .expect("publish");
    }

    /// The next message on `topic`; messages on other topics are skipped.
    pub async fn expect(&mut self, topic: &str) -> Message {
        self.expect_where(topic, |message| message.topic == topic)
            .await
    }

    /// The next message that satisfies `accept`; others are skipped. `what` names the
    /// expectation in the failure message.
    pub async fn expect_where(&mut self, what: &str, accept: impl Fn(&Message) -> bool) -> Message {
        let deadline = Instant::now() + WAIT;
        let mut skipped = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(message) = tokio::time::timeout(remaining, self.messages.recv()).await else {
                panic!("no message for {what} within {WAIT:?}; skipped {skipped:#?}");
            };
            let message = message.expect("the client task runs");
            if accept(&message) {
                return message;
            }
            skipped.push(message.topic);
        }
    }

    /// Everything that arrives within `window`.
    pub async fn collect(&mut self, window: Duration) -> Vec<Message> {
        let mut collected = Vec::new();
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, self.messages.recv()).await {
                Ok(Some(message)) => collected.push(message),
                _ => return collected,
            }
        }
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The bridge on the fake engine with one MQTT adapter under the supervisor.
pub struct Harness {
    pub fake: FakeEngine,
    pub supervisor: Supervisor,
    pub node_state: watch::Sender<Option<NodeState>>,
    run: JoinHandle<Result<(), matter_rs_adapter_api::Error>>,
}

pub fn identity() -> NodeIdentity {
    NodeIdentity {
        vendor_id: VendorId::new(VENDOR_ID),
        product_id: ProductId::new(PRODUCT_ID),
        vendor_name: "matter-rs".into(),
        product_name: "Bridge".into(),
        node_label: "Bridge".into(),
        serial_number: None,
        software_version: 1,
        software_version_string: "0.1.0".into(),
        unique_id: "bridge-1".into(),
        passcode: Secret::new(20_202_021),
        discriminator: 3840,
    }
}

/// The adapter section for a broker on `port`, with quick reconnects, plus `extra` keys.
pub fn config(port: u16, extra: &str) -> Config {
    toml::from_str(&format!(
        "url = \"mqtt://127.0.0.1:{port}\"\nreconnect = {{ initial = 0.05, max = 0.2 }}\n{extra}"
    ))
    .expect("the adapter config parses")
}

/// A single-part device definition from a kind and a mapping in TOML.
pub fn device(key: &str, kind: DeviceKind, mapping: &str) -> DeviceDefinition {
    DeviceDefinition {
        key: DeviceKey::new(key).expect("a valid key"),
        name: key.to_owned(),
        parts: vec![matter_rs_adapter_mqtt::PartDefinition { kind, label: None }],
        mapping: toml::from_str(mapping).expect("the mapping parses"),
    }
}

/// A dimmable light `lamp` from source `hue/lamp` whose level is a percentage, and a temperature
/// sensor `thermometer` from `hue/thermometer` reporting degrees.
pub fn two_devices() -> Vec<DeviceDefinition> {
    vec![
        device(
            "lamp",
            DeviceKind::DimmableLight,
            "source = \"hue/lamp\"\n[items.level]\nscale = [0, 100]\n",
        ),
        device(
            "thermometer",
            DeviceKind::TemperatureSensor,
            "source = \"hue/thermometer\"\n[items.temperature]\nfactor = 100\n",
        ),
    ]
}

impl Harness {
    /// Starts the bridge and the adapter; the node runs (the instance publishes `connected` 2).
    pub async fn start(config: Config, devices: Vec<DeviceDefinition>) -> Self {
        let fake = FakeEngine::new();
        let bridge = Bridge::start_with(
            Box::new(fake.clone()),
            identity(),
            NetworkConfig::default(),
            Attestation::Test,
            Arc::new(MemoryStorage::new()),
            Options {
                timer: Some(Arc::new(TokioTimer)),
                ..Options::default()
            },
        )
        .await
        .expect("the bridge starts");
        let supervisor = Supervisor::new(bridge, SupervisorOptions::default());
        let (node_state, receiver) = watch::channel(Some(NodeState::default()));
        let id = AdapterId::new("mqtt", "test");
        supervisor
            .add(id.clone(), move || {
                MqttAdapter::build(
                    id.clone(),
                    config.clone(),
                    devices.clone(),
                    Some(BridgeIdentity {
                        vendor_id: VENDOR_ID,
                        product_id: PRODUCT_ID,
                    }),
                    receiver.clone(),
                )
            })
            .expect("the adapter is added");
        let run = tokio::spawn({
            let supervisor = supervisor.clone();
            async move { supervisor.run().await }
        });
        Self {
            fake,
            supervisor,
            node_state,
            run,
        }
    }

    pub fn health(&self) -> Health {
        self.supervisor.health()[0].1.clone()
    }

    /// Waits until the adapter reports `health`.
    pub async fn wait_health(&self, wanted: &Health) {
        self.wait_for(|| &self.health() == wanted).await;
        assert_eq!(&self.health(), wanted);
    }

    /// Waits until the adapter is degraded (for any reason).
    pub async fn wait_degraded(&self) {
        self.wait_for(|| matches!(self.health(), Health::Degraded(_)))
            .await;
        assert!(
            matches!(self.health(), Health::Degraded(_)),
            "{}",
            self.health()
        );
    }

    /// Polls `condition` until it holds or the wait runs out.
    pub async fn wait_for(&self, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !condition() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the engine serves `expected` for an attribute.
    pub async fn wait_value(
        &self,
        endpoint: u16,
        cluster: ClusterId,
        attribute: AttributeId,
        expected: &Value,
    ) {
        let read = || {
            self.fake
                .read(EndpointId::new(endpoint), cluster, attribute)
        };
        self.wait_for(|| read().as_ref() == Some(expected)).await;
        assert_eq!(
            read().as_ref(),
            Some(expected),
            "endpoint {endpoint} {cluster}"
        );
    }

    pub fn reachable(&self, endpoint: u16) -> Option<Value> {
        self.fake
            .read(EndpointId::new(endpoint), BRIDGED_DEVICE, REACHABLE)
    }

    pub async fn shutdown(self) {
        self.supervisor.shutdown();
        tokio::time::timeout(WAIT, self.run)
            .await
            .expect("the supervisor stops in time")
            .expect("the run task did not panic")
            .expect("the supervisor stopped cleanly");
    }
}

/// Captures every log record of the process, so that a test can search the lines.
pub struct LogCapture {
    lines: Mutex<Vec<String>>,
}

static CAPTURE: OnceLock<LogCapture> = OnceLock::new();

impl log::Log for LogCapture {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        self.lines.lock().unwrap().push(format!(
            "{} {} {}",
            record.level(),
            record.target(),
            record.args()
        ));
    }

    fn flush(&self) {}
}

impl LogCapture {
    /// Installs the capture (once per process) and returns it.
    pub fn install() -> &'static Self {
        let capture = CAPTURE.get_or_init(|| Self {
            lines: Mutex::new(Vec::new()),
        });
        let _ = log::set_logger(capture);
        log::set_max_level(log::LevelFilter::Trace);
        capture
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

/// A command payload: a struct of tagged fields.
pub fn fields(fields: &[(u64, Value)]) -> Value {
    Value::Struct(fields.to_vec())
}
