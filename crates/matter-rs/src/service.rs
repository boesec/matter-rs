//! `matter-rs run`: the service itself.
//!
//! The service loads the configuration, installs the log, and then, on a tokio runtime:
//!
//! 1. installs the stop signal handlers ([`StopSignals`]);
//! 2. opens the state directory and builds the node identity, the network settings and the
//!    attestation (the test credentials) from the configuration;
//! 3. starts the rs-matter engine (on its own thread) with the bridge role on it;
//! 4. starts the adapters on the bridge under the adapter supervisor ([`start_adapters`], the
//!    wiring point of the adapters: one instance per `[adapters.<id>]` table, each with the
//!    `[devices.<id>]` tables that name it);
//! 5. pulls the node's events from the supervisor in one task, logs the node's lifecycle and
//!    keeps the adapters informed about the node's state;
//! 6. waits for a stop signal, then stops the adapters and the bridge, in that order, and drops
//!    the supervisor, which owns the bridge and so the engine. The engine flushes its deferred
//!    persistence before its thread ends, and the drop waits for that thread, so the state
//!    directory is complete when the process exits.
//!
//! A second stop signal during the shutdown ends the process at once with exit code 1, as does a
//! shutdown that takes longer than [`SHUTDOWN_TIMEOUT`].

use std::fmt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use matter_rs_config::{Config, Error as ConfigError, SystemEnvironment, load};
use matter_rs_engine::{
    Attestation, DirectoryStorage, NetworkConfig, NodeIdentity, ProductId, StopReason, Storage,
    VendorId,
};
use matter_rs_engine_rsmatter::RsMatterEngine;
use tracing::{debug, error, info, warn};

use crate::logging::{self, Output};
use crate::signals::StopSignals;
use crate::{EXIT_FAILURE, error_chain};

#[cfg(feature = "role-bridge")]
use matter_rs_adapter_api::{AdapterId, Supervisor, SupervisorOptions, TokioTimer};
#[cfg(all(feature = "role-bridge", feature = "adapter-mqtt"))]
use matter_rs_adapter_mqtt::{BridgeIdentity, MqttAdapter, NodeState};
#[cfg(feature = "role-bridge")]
use matter_rs_bridge::{Bridge, BridgeEvent, Options};
#[cfg(feature = "role-bridge")]
use matter_rs_config::AdapterConfig;
#[cfg(all(feature = "role-bridge", feature = "adapter-mqtt"))]
use tokio::sync::watch;
#[cfg(feature = "role-bridge")]
use tokio::task::JoinHandle;

/// How long the shutdown may take before the process gives up and exits with code 1. Shorter
/// than the stop timeout of the systemd unit (20 seconds), so that the service reports the
/// failure itself instead of being killed.
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// The storage key of the node's unique id, which the service generates once and keeps in the
/// state directory.
const UNIQUE_ID_KEY: &str = "service/unique-id";

/// A start-up error: the reason the service could not start, without any secret.
#[derive(Debug)]
pub(crate) struct StartError(String);

impl StartError {
    fn new(context: &str, error: &dyn std::error::Error) -> Self {
        Self(format!("{context}: {}", error_chain(error)))
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runs the service with the configuration at `path` until a stop signal; the exit code is 0
/// after a clean stop and 1 when the service could not start or stop cleanly.
pub(crate) fn run(path: &Path) -> ExitCode {
    let loaded = match load(path, &SystemEnvironment) {
        Ok(loaded) => loaded,
        Err(ConfigError::Invalid(report)) => {
            eprint!("{report}");
            return fail_early(&format!("{} is not valid", path.display()));
        }
        Err(error) => return fail_early(&error_chain(&error)),
    };
    let output = match logging::init(&loaded.config.log) {
        Ok(output) => output,
        Err(message) => return fail_early(&message),
    };
    for warning in loaded.report.warnings() {
        warn!("{warning}");
    }
    info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %path.display(),
        "matter-rs starting"
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("matter-rs")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return fail(
                output,
                &StartError::new("cannot start the async runtime", &error),
            );
        }
    };
    let outcome = runtime.block_on(serve(loaded.config));
    // The engine thread has been joined inside `serve` (through the supervisor's drop); nothing
    // that still runs on the runtime holds state that must reach the disk.
    runtime.shutdown_timeout(Duration::from_secs(1));
    match outcome {
        Ok(()) => {
            info!("matter-rs stopped");
            ExitCode::SUCCESS
        }
        Err(error) => fail(output, &error),
    }
}

/// An error before the log is installed: standard error only.
fn fail_early(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(EXIT_FAILURE)
}

/// An error after the log is installed: always in the log, and on standard error as well when
/// the log goes elsewhere.
fn fail(output: Output, error: &StartError) -> ExitCode {
    error!("{error}");
    if !output.is_stderr() {
        eprintln!("error: {error}");
    }
    ExitCode::from(EXIT_FAILURE)
}

/// The node identity of the configuration. The unique id comes from the state directory.
pub(crate) fn identity(config: &Config, unique_id: String) -> NodeIdentity {
    let node = &config.node;
    NodeIdentity {
        vendor_id: VendorId::new(node.vendor_id),
        product_id: ProductId::new(node.product_id),
        vendor_name: node.vendor_name.clone(),
        product_name: node.product_name.clone(),
        node_label: node.node_label.clone(),
        serial_number: node.serial_number.clone(),
        // The numeric software version is not derived from the crate version yet; the text
        // carries it.
        software_version: 0,
        software_version_string: String::from(env!("CARGO_PKG_VERSION")),
        unique_id,
        passcode: node.passcode.clone(),
        discriminator: node.discriminator,
    }
}

/// The network settings of the configuration.
pub(crate) fn network(config: &Config) -> NetworkConfig {
    NetworkConfig {
        interface: config.network.interface.clone(),
        ipv4: config.network.ipv4,
        ipv6: config.network.ipv6,
        port: config.network.port,
    }
}

/// The unique id of the node: generated once (16 random bytes in hexadecimal) and kept in the
/// state directory, so that it is stable across restarts and distinct between installations.
/// The engine derives the node's mDNS host name from it.
pub(crate) fn unique_id(storage: &dyn Storage) -> Result<String, StartError> {
    if let Some(bytes) = storage
        .load(UNIQUE_ID_KEY)
        .map_err(|error| StartError::new("cannot read the unique id", &error))?
    {
        return String::from_utf8(bytes)
            .ok()
            .filter(|id| !id.is_empty() && id.len() <= 32 && id.is_ascii())
            .ok_or_else(|| {
                StartError(format!(
                    "the stored unique id `{UNIQUE_ID_KEY}` is corrupt; remove it to get a new one"
                ))
            });
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| StartError(format!("cannot generate a unique id: {error}")))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    storage
        .store(UNIQUE_ID_KEY, id.as_bytes())
        .map_err(|error| StartError::new("cannot store the unique id", &error))?;
    Ok(id)
}

/// The adapters running on the bridge, under the supervisor.
///
/// This is the wiring point of the adapters: [`start_adapters`] adds one adapter instance per
/// `[adapters.<id>]` table to the supervisor through the adapter's factory, with the
/// `[devices.<id>]` tables that name the instance and a channel that tells the adapter the
/// node's state; [`Adapters::run`] runs the supervisor, [`Adapters::stop`] stops the adapters
/// and the bridge.
#[cfg(feature = "role-bridge")]
pub(crate) struct Adapters {
    supervisor: Supervisor,
    run: Option<JoinHandle<Result<(), matter_rs_adapter_api::Error>>>,
    node_state: NodeStateFeed,
}

/// What the adapters learn about the node: whether it is commissioned, whether a commissioning
/// window is open and how many fabrics it has. Fed from the node's events; read by the adapters
/// that publish it (the MQTT adapter's bridge instance).
#[cfg(feature = "role-bridge")]
#[derive(Clone)]
struct NodeStateFeed {
    #[cfg(feature = "adapter-mqtt")]
    sender: Arc<watch::Sender<Option<NodeState>>>,
}

#[cfg(feature = "role-bridge")]
impl NodeStateFeed {
    fn new() -> Self {
        Self {
            #[cfg(feature = "adapter-mqtt")]
            sender: Arc::new(watch::channel(None).0),
        }
    }

    /// Reads the node's state from the bridge and passes it on.
    #[allow(clippy::unused_async, unused_variables)]
    async fn refresh(&self, bridge: &Bridge) {
        #[cfg(feature = "adapter-mqtt")]
        {
            let (commissioning, fabrics) =
                tokio::join!(bridge.commissioning_state(), bridge.fabrics());
            match (commissioning, fabrics) {
                (Ok(state), Ok(fabrics)) => {
                    self.sender.send_replace(Some(NodeState {
                        commissioned: state.commissioned,
                        window_open: state.window.is_some(),
                        fabric_count: u32::try_from(fabrics.len()).unwrap_or(u32::MAX),
                    }));
                }
                (Err(error), _) | (_, Err(error)) => {
                    warn!(error = %error_chain(&error), "node state unknown");
                }
            }
        }
    }
}

#[cfg(feature = "role-bridge")]
impl Adapters {
    /// Runs the supervisor: every adapter starts, the bridge's events are pumped.
    fn run(&mut self) {
        let supervisor = self.supervisor.clone();
        self.run = Some(tokio::spawn(async move { supervisor.run().await }));
        for (id, health) in self.supervisor.health() {
            debug!(adapter = %id, health = %health, "adapter added");
        }
    }

    /// Stops every adapter, then the bridge; the supervisor's run ends.
    async fn stop(&mut self) {
        self.supervisor.shutdown();
        match self.run.take() {
            Some(run) => match run.await {
                Ok(Ok(())) => debug!("adapters and bridge stopped"),
                Ok(Err(error)) => {
                    warn!(error = %error_chain(&error), "the supervisor did not stop cleanly");
                }
                Err(error) => warn!(%error, "the supervisor task failed"),
            },
            None => {
                if let Err(error) = self.supervisor.bridge().stop().await {
                    warn!(error = %error_chain(&error), "the bridge did not stop cleanly");
                }
            }
        }
    }
}

/// Adds the configured adapter instances to the supervisor, each with its devices.
///
/// # Errors
///
/// A start-up error when an instance cannot be built: its configuration or one of its device
/// mappings is not usable.
#[cfg(feature = "role-bridge")]
#[cfg_attr(not(feature = "adapter-mqtt"), allow(unused_variables))]
pub(crate) fn start_adapters(adapters: &Adapters, config: &Config) -> Result<(), StartError> {
    for (id, instance) in &config.adapters {
        let adapter_id = AdapterId::new(instance_kind(instance), id.clone());
        match instance {
            #[cfg(feature = "adapter-mqtt")]
            AdapterConfig::Mqtt(mqtt) => {
                let devices = config.mqtt_devices(id);
                let identity = BridgeIdentity {
                    vendor_id: config.node.vendor_id,
                    product_id: config.node.product_id,
                };
                let node_state = adapters.node_state.sender.subscribe();
                // Build once now so that a configuration error is a start-up error, not a
                // failed instance in the health report.
                MqttAdapter::build(
                    adapter_id.clone(),
                    mqtt.clone(),
                    devices.clone(),
                    Some(identity),
                    node_state.clone(),
                )
                .map_err(|error| {
                    StartError::new(&format!("adapter `{id}` cannot start"), &error)
                })?;
                let (mqtt, factory_id) = (mqtt.clone(), adapter_id.clone());
                let device_count = devices.len();
                adapters
                    .supervisor
                    .add(adapter_id, move || {
                        MqttAdapter::build(
                            factory_id.clone(),
                            mqtt.clone(),
                            devices.clone(),
                            Some(identity),
                            node_state.clone(),
                        )
                    })
                    .map_err(|error| {
                        StartError::new(&format!("adapter `{id}` cannot be added"), &error)
                    })?;
                info!(
                    adapter = id.as_str(),
                    kind = instance.kind(),
                    devices = device_count,
                    "adapter instance added"
                );
            }
            #[allow(unreachable_patterns)]
            other => {
                let _ = adapter_id;
                return Err(StartError(format!(
                    "adapter `{id}` is of kind `{}`, which this build cannot start",
                    other.kind()
                )));
            }
        }
    }
    Ok(())
}

/// The kind of an adapter instance as a static string, for its id.
#[cfg(feature = "role-bridge")]
fn instance_kind(instance: &AdapterConfig) -> &'static str {
    instance.kind()
}

/// Starts the node and runs it until a stop signal.
async fn serve(config: Config) -> Result<(), StartError> {
    let signals = StopSignals::install()
        .map_err(|error| StartError::new("cannot install the signal handlers", &error))?;

    #[cfg(feature = "role-bridge")]
    {
        if config.bridge.as_ref().is_some_and(|bridge| bridge.enabled) {
            return serve_bridge(config, signals).await;
        }
        Err(StartError(String::from(
            "no role is enabled: add a `[bridge]` section to the configuration (or set \
             `bridge.enabled = true`)",
        )))
    }
    #[cfg(not(feature = "role-bridge"))]
    {
        let _ = (config, signals);
        Err(StartError(String::from(
            "this binary contains no role; build it with the Cargo feature `role-bridge`",
        )))
    }
}

#[cfg(feature = "role-bridge")]
async fn serve_bridge(config: Config, mut signals: StopSignals) -> Result<(), StartError> {
    let state_dir = config.state.dir.clone();
    let storage: Arc<dyn Storage> =
        Arc::new(DirectoryStorage::open(&state_dir).map_err(|error| {
            StartError::new(
                &format!("cannot open the state directory {}", state_dir.display()),
                &error,
            )
        })?);
    let identity = identity(&config, unique_id(&*storage)?);
    let network = network(&config);
    let port = network.port;
    let bridge = Bridge::start_with(
        Box::new(RsMatterEngine::new()),
        identity,
        network,
        Attestation::Test,
        storage,
        Options {
            timer: Some(Arc::new(TokioTimer)),
            ..Options::default()
        },
    )
    .await
    .map_err(|error| StartError::new("the node did not start", &error))?;

    let mut adapters = Adapters {
        supervisor: Supervisor::new(bridge, SupervisorOptions::default()),
        run: None,
        node_state: NodeStateFeed::new(),
    };
    adapters
        .node_state
        .refresh(adapters.supervisor.bridge())
        .await;
    if let Err(error) = start_adapters(&adapters, &config) {
        let _ = adapters.supervisor.bridge().stop().await;
        drop_adapters(adapters).await;
        return Err(error);
    }
    adapters.run();
    let mut events = tokio::spawn(pump_events(
        adapters.supervisor.clone(),
        adapters.node_state.clone(),
    ));

    match adapters.supervisor.bridge().commissioning_state().await {
        Ok(state) => {
            info!(
                port,
                state_dir = %state_dir.display(),
                commissioned = state.commissioned,
                window = ?state.window,
                "node is running"
            );
            if !state.commissioned {
                info!(
                    "the node is not commissioned yet; `matter-rs pairing-info` prints the pairing \
                     codes"
                );
            }
        }
        Err(error) => warn!(error = %error_chain(&error), "commissioning state unknown"),
    }

    let signal = tokio::select! {
        signal = signals.next() => signal,
        ended = &mut events => {
            // The node stopped on its own: the engine failed.
            let reason = match ended {
                Ok(StopReason::Failed(reason)) => reason,
                Ok(other) => format!("{other:?}"),
                Err(error) => format!("the event task ended: {error}"),
            };
            adapters.stop().await;
            drop_adapters(adapters).await;
            return Err(StartError(format!("the node stopped unexpectedly: {reason}")));
        }
    };
    // A second signal ends the process at once. The handlers stay installed, so a signal that
    // arrives before this task first runs is not lost.
    tokio::spawn(async move {
        let second = signals.next().await;
        error!(
            signal = second,
            "second stop signal during the shutdown, exiting without a clean stop"
        );
        std::process::exit(i32::from(EXIT_FAILURE));
    });
    info!(signal, "stop signal received, shutting down");

    let shutdown = async move {
        adapters.stop().await;
        match events.await {
            Ok(reason) => debug!(?reason, "event stream ended"),
            Err(error) => warn!(%error, "the event task failed"),
        }
        drop_adapters(adapters).await;
    };
    tokio::time::timeout(SHUTDOWN_TIMEOUT, shutdown)
        .await
        .map_err(|_| {
            StartError(format!(
                "the shutdown did not finish within {} seconds",
                SHUTDOWN_TIMEOUT.as_secs()
            ))
        })
}

/// Drops the last reference to the supervisor, and with it the bridge, on a blocking thread.
/// The bridge owns the engine, and dropping the rs-matter engine stops a node that still runs
/// and joins the engine thread after its persistence has flushed; the service waits for that
/// before it exits.
#[cfg(feature = "role-bridge")]
async fn drop_adapters(adapters: Adapters) {
    if let Err(error) = tokio::task::spawn_blocking(move || drop(adapters)).await {
        warn!(%error, "joining the engine thread failed");
    }
}

/// Pulls the node's events from the supervisor until the node stops and returns why it stopped.
/// Device commands never arrive here: the supervisor routes them to the adapters. Every change
/// of the commissioning state or the fabrics is passed on to the adapters.
#[cfg(feature = "role-bridge")]
async fn pump_events(supervisor: Supervisor, node_state: NodeStateFeed) -> StopReason {
    let events = supervisor.events();
    loop {
        match events.next().await {
            Some(BridgeEvent::CommissioningChanged(state)) => {
                info!(
                    commissioned = state.commissioned,
                    window = ?state.window,
                    "commissioning state changed"
                );
                node_state.refresh(supervisor.bridge()).await;
            }
            Some(BridgeEvent::FabricAdded(fabric)) => {
                info!(
                    fabric = fabric.index.raw(),
                    vendor_id = fabric.vendor_id.raw(),
                    label = fabric.label.as_str(),
                    "joined a fabric"
                );
                node_state.refresh(supervisor.bridge()).await;
            }
            Some(BridgeEvent::FabricRemoved(index)) => {
                info!(fabric = index.raw(), "left a fabric");
                node_state.refresh(supervisor.bridge()).await;
            }
            Some(BridgeEvent::Stopped(reason)) => {
                info!(?reason, "node stopped");
                return reason;
            }
            Some(other) => debug!(?other, "bridge event"),
            None => return StopReason::Failed(String::from("the event stream ended")),
        }
    }
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::{MemoryStorage, Storage};

    use super::{UNIQUE_ID_KEY, unique_id};

    #[test]
    fn unique_id_is_generated_once_and_kept() {
        let storage = MemoryStorage::new();
        let first = unique_id(&storage).unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(unique_id(&storage).unwrap(), first);
        assert_ne!(unique_id(&MemoryStorage::new()).unwrap(), first);
    }

    #[test]
    fn corrupt_unique_id_is_a_start_up_error() {
        let storage = MemoryStorage::new();
        storage.store(UNIQUE_ID_KEY, &[0xFF, 0xFE]).unwrap();
        let error = unique_id(&storage).unwrap_err();
        assert!(error.to_string().contains("corrupt"), "{error}");
    }
}
