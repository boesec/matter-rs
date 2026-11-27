# Writing an adapter

An adapter brings devices from somewhere else (an MQTT broker, a serial bus, a cloud API) into
the matter-rs bridge and carries the commands of Matter controllers back out. This guide explains
what an adapter is, the contract it implements, how it is wired into the configuration, how the
supervisor runs it and how to test it. Every code block compiles against the current crates.

The crates involved:

| Crate | What you use from it |
|---|---|
| `matter-rs-adapter-api` | The contract: `Adapter`, `AdapterContext`, `AdapterFactory`, `Health`, `Error`; the `Supervisor` and the mock adapter for tests |
| `matter-rs-bridge` | The typed device API: `DeviceSpec`, `DeviceKind`, `Attribute`, `InitialValues`, `DeviceHandle`, `DeviceCommand`, `DeviceEvent` |
| `matter-rs-engine` | Values and ids (`Value`, `ClusterId`), `Secret<T>` for anything that must not be logged, the fake engine for tests |

An adapter crate is named `matter-rs-adapter-<name>`, lives in `crates/adapter-<name>`, depends
on the three crates above and on nothing above them, and is compiled into the service by the
Cargo feature `adapter-<name>`. Adapters are never loaded at run time.

## 1. The model

- **Instances.** One adapter kind can run several times, each with its own configuration: every
  `[adapters.<instance>]` table of the configuration file is one instance. The instance knows
  itself as an `AdapterId { kind, instance }`.
- **The adapter owns a task.** `Adapter::run` consumes the adapter and returns a future; the
  supervisor drives that future in a tokio task of its own. Inside, the adapter connects, adds
  its devices, loops over incoming values and commands, and returns when it is asked to stop.
- **Devices belong to the adapter that added them.** Commands for a device reach only that
  adapter; another adapter cannot touch it.
- **Failure is isolated.** An error, a panic or a hang in one adapter never delays another. A
  failed adapter's devices are marked unreachable at once; the adapter is rebuilt from its
  factory and restarted with exponential backoff (1 s, 2 s, 4 s, ... up to 5 min, reset after 10
  minutes of running). A configuration error is final: the supervisor shows the instance as
  failed and does not retry.

## 2. The trait and the context

```rust
use matter_rs_adapter_api::{Adapter, AdapterContext, BoxFuture, Error, Health};

struct Nothing;

impl Adapter for Nothing {
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            // Connect, add devices, then serve until the host asks to stop.
            ctx.set_health(Health::Healthy);
            ctx.shutdown().await;
            Ok(())
        })
    }
}
```

`AdapterContext` is `Clone + Send + Sync`; hand clones to the tasks you spawn. It offers:

| Method | Purpose |
|---|---|
| `instance()` | The `AdapterId` of this instance, for log lines |
| `devices()` | The `DeviceRegistrar`: `add`, `remove`, `forget`, `get`, `keys` |
| `commands()` | The `CommandStream`: `next().await` yields the next `CommandEvent` for one of your devices, `None` after shutdown |
| `set_health(Health)` | Report `Starting`, `Healthy`, `Degraded(reason)` or `Failed(reason)` |
| `shutdown()` | A future that resolves when the host asks you to stop; `is_shutting_down()` is the same as a flag |

Logging goes through the `log` facade; the service installs the logger. Never log a password, a
token or a passcode: keep them in `Secret<T>` from `matter-rs-engine`, whose `Display` and
`Debug` print `[REDACTED]`, and the same holds for error messages you return.

### Rules

1. **Never block.** Your future shares an executor thread with the bridge's event pump and other
   adapters. Use asynchronous I/O; move a blocking library to `tokio::task::spawn_blocking` or a
   thread of your own.
2. **Honour shutdown.** When `ctx.shutdown()` resolves, close your connections and return
   `Ok(())` promptly. After the stop timeout (10 s by default) the supervisor aborts you.
3. **Every value comes from the device.** The bridge invents no attribute value: `add` needs an
   initial value for every attribute the device kind requires, and you report changes with
   `DeviceHandle::set`. If the device is unreachable, say so with `set_reachable(false)`.
4. **Return errors you cannot recover from.** `Err(Error::Connection(..))` makes the supervisor
   mark your devices unreachable and restart you with backoff; `Err(Error::Config(..))` means the
   configuration is unusable and you are not retried. A connection you can restore yourself is
   better reported as `Health::Degraded` while you reconnect.
5. **State lives in `self` or in the device.** A restarted adapter is a new instance built by the
   factory; it adds its devices again (same keys, same endpoint numbers) and resyncs their values
   from the device.

## 3. Devices, values and commands

A device is a `DeviceSpec`: a stable `DeviceKey` (the id of its `[devices.<id>]` section, unique
across the configuration), a name and one or more parts, each a `DeviceKind`. The kinds of the
first release are lights (on/off, dimmable, colour temperature, extended colour), the on/off plug,
contact, temperature, humidity, pressure, light and occupancy sensors, and the generic switch; the
rustdoc of `DeviceKind` lists their clusters and the attributes you must supply.

```rust
use matter_rs_bridge::{Attribute, DeviceKey, DeviceKind, DeviceSpec, InitialValues, PartSpec};
use matter_rs_engine::Value;

// A dimmable light: on/off and level are required.
let lamp = DeviceSpec::single(DeviceKey::new("lamp").unwrap(), "Lamp", DeviceKind::DimmableLight);
let lamp_values = InitialValues::new()
    .value(Attribute::OnOff, false)
    .value(Attribute::CurrentLevel, Value::U8(10));

// A composed device: a power strip with two sockets and a temperature sensor. Part indexes are
// 0, 1, 2 in this order.
let strip = DeviceSpec {
    key: DeviceKey::new("strip").unwrap(),
    name: "Power strip".into(),
    parts: vec![
        PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Left"),
        PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Right"),
        PartSpec::new(DeviceKind::TemperatureSensor),
    ],
};
let strip_values = InitialValues::new()
    .with(0, Attribute::OnOff, false)
    .with(1, Attribute::OnOff, true)
    .with(2, Attribute::Temperature, Value::I16(2150));
assert!(strip.is_composed());
assert_eq!(lamp_values.get(0, Attribute::CurrentLevel), Some(&Value::U8(10)));
```

`ctx.devices().add(spec, initial).await` returns a `DeviceHandle`:

- `set(part, attribute, value)` reports a new value; the bridge checks type and range and
  notifies subscribed controllers.
- `set_reachable(bool)` toggles the `Reachable` flag controllers show as "unavailable".
- `emit(part, event)` reports switch presses (`DeviceEvent::InitialPress`, `ShortRelease`, ...)
  for a generic switch.
- `value(part, attribute)` reads what the bridge currently serves, for a resync after restart.

Commands arrive on `ctx.commands()` as `CommandEvent { key, part, command }`. The typed variants
of `DeviceCommand` carry *target values*: the bridge has already set the attributes and answered
the controller, and tells you what the device should do now: `SetOnOff { on }`, `SetLevel {
level, transition, on_off }`, `SetColorTemperature { mireds, transition }`, `SetHue`,
`SetSaturation`, `SetHueAndSaturation`, `SetColorXy`, `Identify { duration }`,
`TriggerEffect`, `Rename(label)`, `Written { attribute, value }`. Two variants still wait for an
answer, because they concern clusters the bridge does not interpret (the `extra_clusters` of a
part): `Raw { cluster, command, payload, response }` and `Write { attribute, value, response }`.
Answer them through the `ResponseHandle` (`succeed()`, `fail(status)`, `respond(response)`);
an unanswered one is answered `Failure` by the bridge after its response timeout. The enum is
`#[non_exhaustive]`: always keep a `_ => {}` arm.

## 4. The factory and the configuration

Every adapter crate implements `AdapterFactory` once. The configuration crate composes one enum
of adapter sections, tagged by `kind`, with one variant per adapter compiled in; the variant
carries your `Config` type, so it needs serde's `Deserialize` and schemars' `JsonSchema`. The
service calls `build` for every `[adapters.<instance>]` table of your kind, at start and again
before every restart, so `build` must be cheap and must open no connection.

```rust
use matter_rs_adapter_api::{Adapter, AdapterContext, AdapterFactory, AdapterId, BoxFuture, Error, Health};

/// The keys of `[adapters.<instance>]` besides `kind`.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where the devices come from.
    pub url: String,
    /// How often to poll, in seconds.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    30
}

pub struct Poller {
    config: Config,
}

impl Adapter for Poller {
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            log::info!("{} polls {} every {} s", ctx.instance(), self.config.url, self.config.interval);
            ctx.set_health(Health::Healthy);
            ctx.shutdown().await;
            Ok(())
        })
    }
}

pub struct PollerFactory;

impl AdapterFactory for PollerFactory {
    const KIND: &'static str = "poller";
    type Config = Config;

    fn build(_id: AdapterId, config: Config) -> Result<Box<dyn Adapter>, Error> {
        if config.interval == 0 {
            return Err(Error::Config("interval must be at least 1 second".into()));
        }
        Ok(Box::new(Poller { config }))
    }
}

let config: Config = toml::from_str("url = \"http://example.invalid\"").unwrap();
assert_eq!(config.interval, 30);
assert!(PollerFactory::build(AdapterId::new(PollerFactory::KIND, "main"), config).is_ok());
```

The matching section of the configuration file:

```toml
[adapters.main]
kind = "poller"
url = "http://example.invalid"
interval = 60
```

Wiring the kind into the product takes three small additions outside your crate, described in
the rustdoc of the configuration crate's `adapters` module: a variant `Poller(Config)` of the
adapter config enum behind `#[cfg(feature = "adapter-poller")]`, an entry in the list of known
kinds (so that a build without the feature reports "not compiled in" instead of "unknown
kind"), and the feature `adapter-poller` on the service binary that selects your crate and the
config variant.

## 5. The supervisor

The service runs every instance under one `Supervisor`, built on the bridge:

```rust
use std::sync::Arc;

use matter_rs_adapter_api::{AdapterId, Supervisor, SupervisorOptions, TokioTimer};
use matter_rs_adapter_api::testing::MockControl;
use matter_rs_bridge::{Bridge, Options};
use matter_rs_engine::testing::FakeEngine;
use matter_rs_engine::{
    Attestation, MemoryStorage, NetworkConfig, NodeIdentity, ProductId, Secret, VendorId,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let fake = FakeEngine::new();
    let identity = NodeIdentity {
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
    };
    // The bridge's response timer must be the runtime's clock.
    let options = Options { timer: Some(Arc::new(TokioTimer)), ..Options::default() };
    let bridge = Bridge::start_with(
        Box::new(fake.clone()), identity, NetworkConfig::default(), Attestation::Test,
        Arc::new(MemoryStorage::new()), options,
    )
    .await
    .unwrap();

    let supervisor = Supervisor::new(bridge, SupervisorOptions::default());
    // One factory closure per instance; a real service calls `YourFactory::build(id, config)`.
    let mock = MockControl::new();
    supervisor.add(AdapterId::new("mock", "demo"), mock.factory()).unwrap();

    let running = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run().await }
    });
    mock.wait_until_running().await;
    println!("{:?}", supervisor.health());

    supervisor.shutdown();
    running.await.unwrap().unwrap();
}
```

What the supervisor guarantees:

- **One task per adapter**, panics caught; a failed adapter's devices are unreachable before the
  restart timer starts.
- **Restart with backoff** as described in section 1; `health()` shows `Restarting { attempt,
  next_in }` meanwhile, `Failed(reason)` when it gives up on a configuration error, `Stopped`
  when the adapter returned on its own or after shutdown.
- **Routing by ownership**: a command for a device reaches the adapter that added it. While that
  adapter is down, raw commands and writes are answered `Failure`; when its queue is full,
  `Busy`. Typed commands were answered by the bridge already and are dropped, the device shows
  unreachable.
- **Node events** that are not commands (commissioning changes, fabrics added or removed, the
  stop) are available on `events()`.
- **Orderly shutdown**: `shutdown()` signals every adapter, waits up to the stop timeout, aborts
  what is left, then stops the bridge; `run()` returns afterwards.

`SupervisorOptions` holds the backoff bounds, the reset period, the stop timeout and the queue
capacities, all with the defaults named above.

## 6. Testing

Test an adapter without a broker and without a Matter network: start a `Bridge` on the
`FakeEngine` of `matter-rs-engine` (feature `testing`), run your adapter under the `Supervisor`,
and play the controller with the fake's hooks: `invoke` sends a command and waits for the
response, `write` writes an attribute, `read` looks at the value the node serves, `add_fabric`
simulates commissioning, `attribute_log` and `event_log` record what the node reported.

The mock adapter (`MockControl`, feature `testing` of `matter-rs-adapter-api`) stands in for an
adapter when the thing under test is the wiring around it: it adds devices, sets values, reports
health, fails, panics or hangs on request, and forwards the commands it receives. The supervisor's
own tests use it, and so can a service test before a real adapter exists.

```rust
use std::sync::Arc;

use matter_rs_adapter_api::testing::MockControl;
use matter_rs_adapter_api::{AdapterId, Health, Supervisor, SupervisorOptions, TokioTimer};
use matter_rs_bridge::{Attribute, Bridge, DeviceCommand, DeviceKey, DeviceKind, DeviceSpec, InitialValues, Options};
use matter_rs_engine::testing::FakeEngine;
use matter_rs_engine::{
    Attestation, ClusterId, CommandId, EndpointId, MemoryStorage, NetworkConfig, NodeIdentity,
    ProductId, Secret, Status, Value, VendorId,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let fake = FakeEngine::new();
    let identity = NodeIdentity {
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
    };
    let bridge = Bridge::start_with(
        Box::new(fake.clone()), identity, NetworkConfig::default(), Attestation::Test,
        Arc::new(MemoryStorage::new()),
        Options { timer: Some(Arc::new(TokioTimer)), ..Options::default() },
    )
    .await
    .unwrap();
    let supervisor = Supervisor::new(bridge, SupervisorOptions::default());
    let mock = MockControl::new();
    supervisor.add(AdapterId::new("mock", "lights"), mock.factory()).unwrap();
    let running = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.run().await }
    });

    // The adapter adds a light (endpoint 2) and reports healthy.
    mock.wait_until_running().await;
    let spec = DeviceSpec::single(DeviceKey::new("lamp").unwrap(), "Lamp", DeviceKind::OnOffLight);
    let lamp = mock
        .add_device(spec, InitialValues::new().value(Attribute::OnOff, false))
        .await
        .unwrap();
    mock.report(Health::Healthy);

    // A controller switches it on: the bridge answers, the adapter gets the typed command.
    let on_off = ClusterId::new(0x0006);
    let response = fake.invoke(EndpointId::new(2), on_off, CommandId::new(0x01), Value::Null).await.unwrap();
    assert_eq!(response.status, Status::Success);
    let command = mock.next_command().await.unwrap();
    assert_eq!(command.command, DeviceCommand::SetOnOff { on: true });
    assert_eq!(lamp.value(0, Attribute::OnOff), Some(Value::Bool(true)));

    // The adapter fails: its light is unreachable until the restarted instance adds it again.
    mock.fail(matter_rs_adapter_api::Error::Connection("lost".into()));
    mock.wait_for_ended(1).await;
    mock.wait_for_runs(2).await;
    assert!(!lamp.is_reachable());

    supervisor.shutdown();
    running.await.unwrap().unwrap();
}
```

Use `#[tokio::test(start_paused = true)]` in your tests: the clock advances automatically while
every task waits, so a 5-minute backoff or a 10-second stop timeout costs no real time.

## 7. A complete minimal adapter

A crate `matter-rs-adapter-clock` with one instance type: it adds one contact sensor whose state
flips every `period` seconds, and logs every rename a controller sends. Everything an adapter
needs is here: the config type, the factory, the device, values, commands, health and shutdown.

```rust
use std::time::Duration;

use matter_rs_adapter_api::{Adapter, AdapterContext, AdapterFactory, AdapterId, BoxFuture, Error, Health};
use matter_rs_bridge::{Attribute, DeviceCommand, DeviceKey, DeviceKind, DeviceSpec, InitialValues};

/// `[adapters.<instance>]` with `kind = "clock"`.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The key of the contact sensor the adapter adds.
    pub device: String,
    /// Seconds between two flips of the contact.
    #[serde(default = "default_period")]
    pub period: u64,
}

fn default_period() -> u64 {
    60
}

pub struct ClockFactory;

impl AdapterFactory for ClockFactory {
    const KIND: &'static str = "clock";
    type Config = Config;

    fn build(_id: AdapterId, config: Config) -> Result<Box<dyn Adapter>, Error> {
        let key = DeviceKey::new(config.device.as_str()).map_err(|e| Error::Config(e.to_string()))?;
        if config.period == 0 {
            return Err(Error::Config("period must be at least 1 second".into()));
        }
        Ok(Box::new(Clock {
            key,
            period: Duration::from_secs(config.period),
        }))
    }
}

pub struct Clock {
    key: DeviceKey,
    period: Duration,
}

impl Adapter for Clock {
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let spec = DeviceSpec::single(self.key.clone(), "Clock contact", DeviceKind::ContactSensor);
            let contact = ctx
                .devices()
                .add(spec, InitialValues::new().value(Attribute::StateValue, false))
                .await?;
            ctx.set_health(Health::Healthy);

            let mut commands = ctx.commands();
            let mut shutdown = ctx.shutdown();
            let mut ticks = tokio::time::interval(self.period);
            let mut closed = false;
            loop {
                tokio::select! {
                    () = &mut shutdown => return Ok(()),
                    _ = ticks.tick() => {
                        closed = !closed;
                        contact.set(0, Attribute::StateValue, closed).await?;
                    }
                    command = commands.next() => {
                        let Some(command) = command else { return Ok(()) };
                        match command.command {
                            DeviceCommand::Rename(label) => {
                                log::info!("{}: controller renamed {} to {label}", ctx.instance(), command.key);
                            }
                            DeviceCommand::Identify { duration } => {
                                log::info!("{}: identify for {duration:?}", ctx.instance());
                            }
                            _ => {}
                        }
                    }
                }
            }
        })
    }
}

let config: Config = toml::from_str("device = \"clock\"\nperiod = 5").unwrap();
let adapter = ClockFactory::build(AdapterId::new(ClockFactory::KIND, "demo"), config);
assert!(adapter.is_ok());
```

To ship it: add the crate under `crates/adapter-clock`, the feature `adapter-clock` on the
service binary, the config enum variant and the kind entry in the configuration crate, and a test
along the lines of section 6. The architecture document in the repository root lists the layering
rules the dependency test enforces.
