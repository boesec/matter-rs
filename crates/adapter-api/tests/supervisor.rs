//! The supervisor against the fake engine, driven through the mock adapter.
//!
//! Every test runs on a paused tokio clock: backoff delays and stop timeouts take no real time,
//! and a condition that never comes true fails at its timeout instead of hanging the suite.

use std::sync::{Mutex, Once, PoisonError};
use std::time::Duration;

use matter_rs_adapter_api::testing::MockControl;
use matter_rs_adapter_api::{
    Adapter, AdapterContext, AdapterFactory, AdapterId, BoxFuture, Error, Health, Supervisor,
    SupervisorOptions, TokioTimer,
};
use matter_rs_bridge::{
    Attribute, Bridge, BridgeEvent, DeviceCommand, DeviceHandle, DeviceKey, DeviceKind, DeviceSpec,
    InitialValues, Options, PartSpec,
};
use matter_rs_engine::testing::FakeEngine;
use matter_rs_engine::{
    Attestation, AttributeId, ClusterDescription, ClusterId, CommandId, EndpointId, FabricIndex,
    FabricInfo, MemoryStorage, NetworkConfig, NodeIdentity, ProductId, REDACTED, Secret, Status,
    StopReason, Value, VendorId, bridged_device,
};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

const ON_OFF: ClusterId = ClusterId::new(0x0006);
const OFF: CommandId = CommandId::new(0x00);
const ON: CommandId = CommandId::new(0x01);
const LEVEL_CONTROL: ClusterId = ClusterId::new(0x0008);
const MOVE_TO_LEVEL: CommandId = CommandId::new(0x00);
const CURRENT_LEVEL: AttributeId = AttributeId::new(0x0000);
const VENDOR: ClusterId = ClusterId::new(0xFFF1_FC01);
const VENDOR_ATTRIBUTE: AttributeId = AttributeId::new(0);
const VENDOR_COMMAND: CommandId = CommandId::new(1);

fn identity() -> NodeIdentity {
    NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
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

fn id(instance: &str) -> AdapterId {
    AdapterId::new("mock", instance)
}

fn key(name: &str) -> DeviceKey {
    DeviceKey::new(name).unwrap()
}

fn ep(number: u16) -> EndpointId {
    EndpointId::new(number)
}

fn light(name: &str) -> (DeviceSpec, InitialValues) {
    (
        DeviceSpec::single(key(name), name, DeviceKind::OnOffLight),
        InitialValues::new().value(Attribute::OnOff, false),
    )
}

fn dimmable_light(name: &str) -> (DeviceSpec, InitialValues) {
    (
        DeviceSpec::single(key(name), name, DeviceKind::DimmableLight),
        // On, so that a level command is not ignored (`ExecuteIfOff` is off by default).
        InitialValues::new()
            .value(Attribute::OnOff, true)
            .value(Attribute::CurrentLevel, Value::U8(10)),
    )
}

/// An on/off light with a vendor cluster, whose commands and writes reach the adapter raw.
fn light_with_vendor_cluster(name: &str) -> (DeviceSpec, InitialValues) {
    let part = PartSpec::new(DeviceKind::OnOffLight).with_extra_cluster(ClusterDescription {
        id: VENDOR,
        revision: 1,
        feature_map: 0,
        attributes: vec![VENDOR_ATTRIBUTE],
        accepted_commands: vec![VENDOR_COMMAND],
        generated_commands: vec![],
        events: vec![],
    });
    (
        DeviceSpec {
            key: key(name),
            name: name.into(),
            parts: vec![part],
        },
        InitialValues::new()
            .value(Attribute::OnOff, false)
            .value(Attribute::Raw(VENDOR, VENDOR_ATTRIBUTE), Value::U8(0)),
    )
}

async fn start_bridge(fake: &FakeEngine) -> Bridge {
    Bridge::start_with(
        Box::new(fake.clone()),
        identity(),
        NetworkConfig::default(),
        Attestation::Test,
        std::sync::Arc::new(MemoryStorage::new()),
        Options {
            timer: Some(std::sync::Arc::new(TokioTimer)),
            ..Options::default()
        },
    )
    .await
    .expect("the bridge starts")
}

struct Harness {
    fake: FakeEngine,
    supervisor: Supervisor,
    task: JoinHandle<Result<(), Error>>,
}

impl Harness {
    async fn start(options: SupervisorOptions, adapters: &[(&str, &MockControl)]) -> Self {
        let fake = FakeEngine::new();
        let supervisor = Supervisor::new(start_bridge(&fake).await, options);
        for (instance, control) in adapters {
            supervisor.add(id(instance), control.factory()).unwrap();
        }
        let task = tokio::spawn({
            let supervisor = supervisor.clone();
            async move { supervisor.run().await }
        });
        Self {
            fake,
            supervisor,
            task,
        }
    }

    async fn shutdown(self) -> Result<(), Error> {
        self.supervisor.shutdown();
        self.task.await.expect("the supervisor task does not panic")
    }

    fn health(&self, index: usize) -> Health {
        self.supervisor.health()[index].1.clone()
    }

    /// Waits until the health of adapter `index` satisfies `condition`.
    async fn wait_health(&self, index: usize, condition: impl Fn(&Health) -> bool) -> Health {
        wait_until(|| {
            let health = self.health(index);
            condition(&health).then_some(health)
        })
        .await
    }

    async fn invoke(&self, endpoint: u16, cluster: ClusterId, command: CommandId) -> Status {
        self.invoke_with(endpoint, cluster, command, Value::Null)
            .await
    }

    async fn invoke_with(
        &self,
        endpoint: u16,
        cluster: ClusterId,
        command: CommandId,
        payload: Value,
    ) -> Status {
        self.fake
            .invoke(ep(endpoint), cluster, command, payload)
            .await
            .expect("the node runs")
            .status
    }

    fn read(&self, endpoint: u16, cluster: ClusterId, attribute: AttributeId) -> Option<Value> {
        self.fake.read(ep(endpoint), cluster, attribute)
    }
}

/// Polls `probe` until it yields a value; fails after a minute of virtual time.
async fn wait_until<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    timeout(Duration::from_secs(60), async {
        loop {
            if let Some(value) = probe() {
                return value;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the condition comes true")
}

/// Waits for a device handle to report unreachable.
async fn wait_unreachable(device: &DeviceHandle) {
    wait_until(|| (!device.is_reachable()).then_some(())).await;
}

async fn add_light(control: &MockControl, name: &str) -> DeviceHandle {
    let (spec, initial) = light(name);
    control
        .add_device(spec, initial)
        .await
        .expect("the device is added")
}

#[tokio::test(start_paused = true)]
async fn mock_adapter_drives_a_bridged_light() {
    let mock = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("lights", &mock)]).await;
    mock.wait_until_running().await;

    // The adapter adds a dimmable light: endpoint 2 behind the aggregator.
    let (spec, initial) = dimmable_light("lamp");
    let lamp = mock.add_device(spec, initial).await.expect("added");
    assert_eq!(lamp.endpoints().unwrap()[0].endpoint, ep(2));
    assert_eq!(
        harness.read(2, LEVEL_CONTROL, CURRENT_LEVEL),
        Some(Value::U8(10))
    );

    // The adapter reports a value through its own task.
    mock.set(&key("lamp"), 0, Attribute::CurrentLevel, Value::U8(100))
        .await
        .expect("the value is set");
    assert_eq!(
        harness.read(2, LEVEL_CONTROL, CURRENT_LEVEL),
        Some(Value::U8(100))
    );

    // A Matter client moves the level: the bridge answers, the adapter is told.
    let payload = Value::Struct(vec![
        (0, Value::U8(200)),
        (1, Value::U16(10)),
        (2, Value::U8(0)),
        (3, Value::U8(0)),
    ]);
    let status = harness
        .invoke_with(2, LEVEL_CONTROL, MOVE_TO_LEVEL, payload)
        .await;
    assert_eq!(status, Status::Success);
    let command = mock.next_command().await.expect("a command arrives");
    assert_eq!(command.key, key("lamp"));
    assert_eq!(command.part, Some(0));
    assert_eq!(
        command.command,
        DeviceCommand::SetLevel {
            level: 200,
            transition: Some(Duration::from_secs(1)),
            on_off: None,
        }
    );

    // The adapter answers by driving the device and confirming the level it reached.
    mock.set(&key("lamp"), 0, Attribute::CurrentLevel, Value::U8(200))
        .await
        .expect("the value is set");
    assert_eq!(lamp.value(0, Attribute::CurrentLevel), Some(Value::U8(200)));

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn commands_are_routed_by_ownership() {
    let a = MockControl::new();
    let b = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("a", &a), ("b", &b)]).await;
    a.wait_until_running().await;
    b.wait_until_running().await;
    add_light(&a, "lamp-a").await;
    add_light(&b, "lamp-b").await;
    assert_eq!(a.context().unwrap().devices().keys(), vec![key("lamp-a")]);
    assert_eq!(b.context().unwrap().devices().keys(), vec![key("lamp-b")]);

    assert_eq!(harness.invoke(2, ON_OFF, ON).await, Status::Success);
    assert_eq!(harness.invoke(3, ON_OFF, ON).await, Status::Success);
    assert_eq!(harness.invoke(3, ON_OFF, OFF).await, Status::Success);

    let command = a.next_command().await.unwrap();
    assert_eq!(command.key, key("lamp-a"));
    assert_eq!(command.command, DeviceCommand::SetOnOff { on: true });
    let command = b.next_command().await.unwrap();
    assert_eq!(command.key, key("lamp-b"));
    assert_eq!(command.command, DeviceCommand::SetOnOff { on: true });
    let command = b.next_command().await.unwrap();
    assert_eq!(command.key, key("lamp-b"));
    assert_eq!(command.command, DeviceCommand::SetOnOff { on: false });

    // Nothing of B's reached A.
    assert!(
        timeout(Duration::from_secs(1), a.next_command())
            .await
            .is_err()
    );

    // A cannot touch B's device.
    let (spec, initial) = light("lamp-b");
    let error = a.add_device(spec, initial).await.unwrap_err();
    assert!(matches!(error, Error::ForeignDevice { owner, .. } if owner == id("b")));

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn panicking_adapter_is_isolated_and_restarted() {
    let healthy = MockControl::new();
    let panicking = MockControl::new();
    let harness = Harness::start(
        SupervisorOptions::default(),
        &[("healthy", &healthy), ("panicking", &panicking)],
    )
    .await;
    healthy.wait_until_running().await;
    panicking.wait_until_running().await;
    let good = add_light(&healthy, "good").await;
    let bad = add_light(&panicking, "bad").await;

    // The panic takes only its own adapter down.
    panicking.panic("boom");
    panicking.wait_for_ended(1).await;
    let health = harness
        .wait_health(1, |h| matches!(h, Health::Restarting { .. }))
        .await;
    assert_eq!(
        health,
        Health::Restarting {
            attempt: 1,
            next_in: Duration::from_secs(1),
        }
    );
    wait_unreachable(&bad).await;
    assert!(good.is_reachable());

    // The healthy adapter keeps serving values and commands, promptly.
    let served = timeout(Duration::from_secs(1), async {
        healthy
            .set(&key("good"), 0, Attribute::OnOff, true)
            .await
            .expect("value set");
        assert_eq!(harness.invoke(2, ON_OFF, OFF).await, Status::Success);
        healthy.next_command().await.unwrap()
    })
    .await
    .expect("the healthy adapter answers within a second");
    assert_eq!(served.command, DeviceCommand::SetOnOff { on: false });

    // The panicking adapter is rebuilt and started again after the backoff; it re-adds its
    // device, which becomes reachable again with the same endpoint.
    panicking.wait_for_runs(2).await;
    assert_eq!(harness.health(1), Health::Starting);
    let bad_again = add_light(&panicking, "bad").await;
    assert_eq!(bad_again.endpoints().unwrap(), bad.endpoints().unwrap());
    assert!(bad_again.is_reachable());

    // The second failure doubles the delay.
    panicking.panic("boom again");
    let health = harness
        .wait_health(1, |h| matches!(h, Health::Restarting { .. }))
        .await;
    assert_eq!(
        health,
        Health::Restarting {
            attempt: 2,
            next_in: Duration::from_secs(2),
        }
    );
    panicking.wait_for_runs(3).await;

    // A long healthy run resets the backoff.
    sleep(Duration::from_secs(11 * 60)).await;
    panicking.fail(Error::Connection("lost".into()));
    let health = harness
        .wait_health(1, |h| matches!(h, Health::Restarting { .. }))
        .await;
    assert_eq!(
        health,
        Health::Restarting {
            attempt: 1,
            next_in: Duration::from_secs(1),
        }
    );

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn hanging_adapter_does_not_block_others() {
    let healthy = MockControl::new();
    let hanging = MockControl::new();
    let harness = Harness::start(
        SupervisorOptions::default(),
        &[("healthy", &healthy), ("hanging", &hanging)],
    )
    .await;
    healthy.wait_until_running().await;
    hanging.wait_until_running().await;
    add_light(&healthy, "good").await;
    let stuck = add_light(&hanging, "stuck").await;
    hanging.hang();

    // Values and commands of the healthy adapter flow as before.
    for round in 0..3_u8 {
        timeout(Duration::from_secs(1), async {
            healthy
                .set(&key("good"), 0, Attribute::OnOff, round % 2 == 0)
                .await
                .expect("value set");
            assert_eq!(harness.invoke(2, ON_OFF, ON).await, Status::Success);
            let command = healthy.next_command().await.unwrap();
            assert_eq!(command.command, DeviceCommand::SetOnOff { on: true });
        })
        .await
        .expect("the healthy adapter answers within a second");
    }

    // A command for the hanging adapter's device is queued, not lost; the bridge still answers
    // the client and the device stays reachable (a hang is not a failure).
    assert_eq!(harness.invoke(3, ON_OFF, ON).await, Status::Success);
    assert!(stuck.is_reachable());

    harness.shutdown().await.expect("orderly shutdown");
    assert_eq!(hanging.status().shutdowns, 0);
    assert_eq!(hanging.status().ended, 1);
}

#[tokio::test(start_paused = true)]
async fn hanging_adapter_is_aborted_at_shutdown() {
    let hanging = MockControl::new();
    let options = SupervisorOptions {
        stop_timeout: Duration::from_secs(10),
        ..SupervisorOptions::default()
    };
    let harness = Harness::start(options, &[("hanging", &hanging)]).await;
    hanging.wait_until_running().await;
    add_light(&hanging, "stuck").await;
    hanging.hang();
    // Give the adapter time to reach the hang before the shutdown is signalled.
    sleep(Duration::from_millis(10)).await;

    let before = Instant::now();
    harness.shutdown().await.expect("orderly shutdown");
    let elapsed = before.elapsed();
    assert!(
        elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(12),
        "the stop timeout bounds the shutdown: {elapsed:?}"
    );
    assert_eq!(hanging.status().ended, 1, "the hanging run was aborted");
    assert_eq!(hanging.status().shutdowns, 0, "it never saw the signal");
}

#[tokio::test(start_paused = true)]
async fn failed_adapter_devices_become_unreachable() {
    let failing = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("failing", &failing)]).await;
    failing.wait_until_running().await;
    let first = add_light(&failing, "first").await;
    let second = add_light(&failing, "second").await;
    assert!(first.is_reachable() && second.is_reachable());

    failing.fail(Error::Connection("broker gone".into()));
    wait_unreachable(&first).await;
    wait_unreachable(&second).await;
    assert_eq!(
        harness.read(2, bridged_device::CLUSTER, bridged_device::REACHABLE),
        Some(Value::Bool(false))
    );
    assert_eq!(
        harness.read(3, bridged_device::CLUSTER, bridged_device::REACHABLE),
        Some(Value::Bool(false))
    );
    let changed: Vec<EndpointId> = harness
        .fake
        .event_log()
        .into_iter()
        .filter(|e| e.event == bridged_device::REACHABLE_CHANGED)
        .map(|e| e.endpoint)
        .collect();
    assert_eq!(changed, vec![ep(2), ep(3)]);
    assert!(matches!(harness.health(0), Health::Restarting { .. }));

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn command_for_down_adapter_fails_cleanly() {
    let failing = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("failing", &failing)]).await;
    failing.wait_until_running().await;
    let (spec, initial) = light_with_vendor_cluster("lamp");
    let lamp = failing.add_device(spec, initial).await.expect("added");

    // While the adapter runs, a raw command reaches it and it answers.
    let pending = tokio::spawn({
        let fake = harness.fake.clone();
        async move {
            fake.invoke(ep(2), VENDOR, VENDOR_COMMAND, Value::Null)
                .await
        }
    });
    let command = failing.next_command().await.unwrap();
    let DeviceCommand::Raw { response, .. } = command.command else {
        panic!("a raw command was expected");
    };
    response.succeed().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap().status, Status::Success);

    // The adapter fails: a raw command and a raw write for its device are answered `Failure` by
    // the supervisor, a typed command is answered by the bridge as before and dropped.
    failing.fail(Error::Connection("gone".into()));
    wait_unreachable(&lamp).await;
    assert_eq!(
        harness.invoke(2, VENDOR, VENDOR_COMMAND).await,
        Status::Failure
    );
    let write = harness
        .fake
        .write(ep(2), VENDOR, VENDOR_ATTRIBUTE, Value::U8(7))
        .await
        .unwrap();
    assert_eq!(write.status, Status::Failure);
    assert_eq!(
        harness.read(2, VENDOR, VENDOR_ATTRIBUTE),
        Some(Value::U8(0))
    );
    assert_eq!(harness.invoke(2, ON_OFF, ON).await, Status::Success);
    assert!(matches!(harness.health(0), Health::Restarting { .. }));

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn commands_beyond_capacity_are_answered_busy() {
    let hanging = MockControl::new();
    let options = SupervisorOptions {
        command_capacity: 1,
        ..SupervisorOptions::default()
    };
    let harness = Harness::start(options, &[("hanging", &hanging)]).await;
    hanging.wait_until_running().await;
    let (spec, initial) = light_with_vendor_cluster("lamp");
    hanging.add_device(spec, initial).await.expect("added");
    hanging.hang();
    sleep(Duration::from_millis(10)).await;

    // The first raw command fills the queue and waits for the bridge's response timeout; the
    // second is refused at once.
    let first = tokio::spawn({
        let fake = harness.fake.clone();
        async move {
            fake.invoke(ep(2), VENDOR, VENDOR_COMMAND, Value::Null)
                .await
        }
    });
    sleep(Duration::from_millis(10)).await;
    assert_eq!(
        harness.invoke(2, VENDOR, VENDOR_COMMAND).await,
        Status::Busy
    );
    assert_eq!(first.await.unwrap().unwrap().status, Status::Failure);

    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn health_follows_adapter_state() {
    let mock = MockControl::new();
    let fake = FakeEngine::new();
    let supervisor = Supervisor::new(start_bridge(&fake).await, SupervisorOptions::default());
    supervisor.add(id("mock"), mock.factory()).unwrap();
    supervisor
        .add(id("broken"), || Err(Error::Config("no topic".into())))
        .unwrap();
    supervisor
        .add(id("flaky"), || Err(Error::Connection("not yet".into())))
        .unwrap();
    assert_eq!(
        supervisor.add(id("mock"), mock.factory()).unwrap_err(),
        Error::Config("adapter mock/mock is configured twice".into())
    );
    assert_eq!(supervisor.health()[0], (id("mock"), Health::Starting));

    let task = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run().await }
    });
    mock.wait_until_running().await;
    assert_eq!(
        supervisor.add(id("late"), mock.factory()).unwrap_err(),
        Error::Internal("adapters are added before the supervisor runs".into())
    );
    assert_eq!(
        supervisor.run().await.unwrap_err(),
        Error::Internal("the supervisor runs already".into())
    );

    // As reported by the adapter.
    assert_eq!(supervisor.health()[0].1, Health::Starting);
    mock.report(Health::Healthy);
    wait_until(|| (supervisor.health()[0].1 == Health::Healthy).then_some(())).await;
    mock.report(Health::Degraded("reconnecting".into()));
    wait_until(|| {
        (supervisor.health()[0].1 == Health::Degraded("reconnecting".into())).then_some(())
    })
    .await;

    // As observed by the supervisor: a factory that cannot build from its configuration is
    // failed for good, one that fails otherwise is retried with backoff.
    wait_until(|| (supervisor.health()[1].1 == Health::Failed("no topic".into())).then_some(()))
        .await;
    wait_until(|| matches!(supervisor.health()[2].1, Health::Restarting { .. }).then_some(()))
        .await;

    // A failure of the running adapter shows the restart, a configuration error ends it.
    mock.fail(Error::Connection("lost".into()));
    wait_until(|| matches!(supervisor.health()[0].1, Health::Restarting { .. }).then_some(()))
        .await;
    mock.wait_for_runs(2).await;
    assert_eq!(supervisor.health()[0].1, Health::Starting);
    mock.fail(Error::Config("bad mapping".into()));
    wait_until(|| (supervisor.health()[0].1 == Health::Failed("bad mapping".into())).then_some(()))
        .await;
    sleep(Duration::from_secs(10)).await;
    assert_eq!(
        mock.status().runs,
        2,
        "a configuration error is not retried"
    );

    supervisor.shutdown();
    task.await.unwrap().expect("orderly shutdown");
    let health = supervisor.health();
    assert_eq!(health[0].1, Health::Failed("bad mapping".into()));
    assert_eq!(health[1].1, Health::Failed("no topic".into()));
    assert_eq!(health[2].1, Health::Stopped);
}

#[tokio::test(start_paused = true)]
async fn adapter_that_stops_on_its_own_is_not_restarted() {
    let mock = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("mock", &mock)]).await;
    mock.wait_until_running().await;
    let lamp = add_light(&mock, "lamp").await;
    mock.stop();
    harness.wait_health(0, |h| *h == Health::Stopped).await;
    wait_unreachable(&lamp).await;
    sleep(Duration::from_secs(10)).await;
    assert_eq!(mock.status().runs, 1);
    harness.shutdown().await.expect("orderly shutdown");
}

#[tokio::test(start_paused = true)]
async fn shutdown_is_orderly() {
    let a = MockControl::new();
    let b = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("a", &a), ("b", &b)]).await;
    a.wait_until_running().await;
    b.wait_until_running().await;
    add_light(&a, "lamp-a").await;
    add_light(&b, "lamp-b").await;
    let events = harness.supervisor.events();

    let before = Instant::now();
    harness.shutdown().await.expect("orderly shutdown");
    assert!(before.elapsed() < Duration::from_secs(10));

    // Both adapters saw the signal and returned, then the bridge stopped.
    assert_eq!(a.status().shutdowns, 1);
    assert_eq!(b.status().shutdowns, 1);
    assert!(!a.status().is_running() && !b.status().is_running());
    assert!(matches!(
        events.next().await,
        Some(BridgeEvent::Stopped(StopReason::Requested))
    ));
}

#[tokio::test(start_paused = true)]
async fn node_events_are_exposed_and_the_bridge_stop_ends_the_run() {
    let mock = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("mock", &mock)]).await;
    mock.wait_until_running().await;
    let events = harness.supervisor.events();

    harness
        .fake
        .add_fabric(FabricInfo {
            index: FabricIndex::new(1),
            fabric_id: 7,
            node_id: 42,
            vendor_id: VendorId::new(0xFFF1),
            label: "Home".into(),
        })
        .unwrap();
    assert!(matches!(
        events.next().await,
        Some(BridgeEvent::FabricAdded(fabric)) if fabric.fabric_id == 7
    ));
    assert!(matches!(
        events.next().await,
        Some(BridgeEvent::CommissioningChanged(state)) if state.commissioned
    ));

    // The bridge stops on its own: the supervisor stops its adapters and returns.
    harness.supervisor.bridge().stop().await.unwrap();
    harness.task.await.unwrap().expect("the run ends");
    assert!(matches!(
        events.next().await,
        Some(BridgeEvent::Stopped(StopReason::Requested))
    ));
    assert_eq!(mock.status().shutdowns, 1);
    assert_eq!(harness.supervisor.health()[0].1, Health::Stopped);
}

/// The factory of a test adapter kind with a serde and schemars configuration.
struct EchoFactory;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoConfig {
    /// The name of the one light the adapter adds.
    light: String,
    /// Whether the light starts on.
    #[serde(default)]
    on: bool,
}

struct Echo {
    config: EchoConfig,
}

impl Adapter for Echo {
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let spec = DeviceSpec::single(
                DeviceKey::new(self.config.light.as_str())?,
                self.config.light.as_str(),
                DeviceKind::OnOffLight,
            );
            ctx.devices()
                .add(
                    spec,
                    InitialValues::new().value(Attribute::OnOff, self.config.on),
                )
                .await?;
            ctx.set_health(Health::Healthy);
            ctx.shutdown().await;
            Ok(())
        })
    }
}

impl AdapterFactory for EchoFactory {
    const KIND: &'static str = "echo";
    type Config = EchoConfig;

    fn build(_id: AdapterId, config: Self::Config) -> Result<Box<dyn Adapter>, Error> {
        if config.light.is_empty() {
            return Err(Error::Config("light is empty".into()));
        }
        Ok(Box::new(Echo { config }))
    }
}

#[tokio::test(start_paused = true)]
async fn factory_builds_from_config() {
    let config: EchoConfig = serde_json::from_str(r#"{"light": "porch", "on": true}"#).unwrap();
    let schema = serde_json::to_value(schemars::schema_for!(EchoConfig)).unwrap();
    assert!(schema["properties"]["light"].is_object());
    assert_eq!(
        EchoFactory::build(
            AdapterId::new(EchoFactory::KIND, "x"),
            EchoConfig {
                light: String::new(),
                on: false,
            }
        )
        .err(),
        Some(Error::Config("light is empty".into()))
    );

    let fake = FakeEngine::new();
    let supervisor = Supervisor::new(start_bridge(&fake).await, SupervisorOptions::default());
    let id = AdapterId::new(EchoFactory::KIND, "porch");
    supervisor
        .add(id.clone(), move || {
            EchoFactory::build(id.clone(), config.clone())
        })
        .unwrap();
    let task = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run().await }
    });
    wait_until(|| (supervisor.health()[0].1 == Health::Healthy).then_some(())).await;
    assert_eq!(
        fake.read(ep(2), ON_OFF, AttributeId::new(0)),
        Some(Value::Bool(true))
    );
    supervisor.shutdown();
    task.await.unwrap().expect("orderly shutdown");
}

struct CaptureLogger;

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INSTALL: Once = Once::new();

impl log::Log for CaptureLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        LINES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{} {}", record.level(), record.args()));
    }

    fn flush(&self) {}
}

fn captured_lines() -> Vec<String> {
    LINES.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[tokio::test(start_paused = true)]
async fn failure_logs_redact() {
    INSTALL.call_once(|| {
        log::set_logger(&CaptureLogger).expect("no other logger in this test binary");
        log::set_max_level(log::LevelFilter::Trace);
    });
    let leaky = MockControl::new();
    let harness = Harness::start(SupervisorOptions::default(), &[("leaky", &leaky)]).await;
    leaky.wait_until_running().await;
    let password = Secret::new("hunter2");
    leaky.fail(Error::Connection(format!(
        "broker refused password {password}"
    )));
    harness
        .wait_health(0, |h| matches!(h, Health::Restarting { .. }))
        .await;
    leaky.wait_for_runs(2).await;
    leaky.panic(format!("token {password} rejected"));
    leaky.wait_for_runs(3).await;

    let lines = captured_lines();
    let about_leaky: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("mock/leaky failed"))
        .collect();
    assert_eq!(about_leaky.len(), 2, "{lines:#?}");
    assert!(about_leaky.iter().all(|line| line.contains(REDACTED)));
    assert!(about_leaky[0].contains("connection failed"));
    assert!(about_leaky[1].contains("panicked"));
    assert!(
        lines.iter().all(|line| !line.contains("hunter2")),
        "{lines:#?}"
    );

    harness.shutdown().await.expect("orderly shutdown");
}
