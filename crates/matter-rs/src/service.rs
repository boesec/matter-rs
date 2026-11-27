//! `matter-rs run`: the service itself.
//!
//! The service loads the configuration, installs the log, and then, on a tokio runtime:
//!
//! 1. installs the stop signal handlers ([`StopSignals`]);
//! 2. opens the state directory and builds the node identity, the network settings and the
//!    attestation (the test credentials) from the configuration;
//! 3. starts the rs-matter engine (on its own thread) with the bridge role on it;
//! 4. starts the adapters on the bridge ([`start_adapters`], the wiring point of the adapter
//!    supervisor);
//! 5. pulls the bridge's events in one task, logs the node's lifecycle and hands device commands
//!    to the adapters;
//! 6. waits for a stop signal, then stops the adapters and the bridge, in that order, and drops
//!    the bridge, which owns the engine. The engine flushes its deferred persistence before its
//!    thread ends, and the drop waits for that thread, so the state directory is complete when
//!    the process exits.
//!
//! A second stop signal during the shutdown ends the process at once with exit code 1, as does a
//! shutdown that takes longer than [`SHUTDOWN_TIMEOUT`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use matter_rs_config::{AdapterConfig, Config, Error as ConfigError, SystemEnvironment, load};
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
use matter_rs_bridge::{Bridge, BridgeEvent, CommandEvent};

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
    // The engine thread has been joined inside `serve` (through the bridge's drop); nothing that still runs on the runtime
    // holds state that must reach the disk.
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

/// The adapters running on the bridge.
///
/// This is the wiring point of the adapter supervisor: [`start_adapters`] starts one adapter per
/// configured instance on the bridge, [`Adapters::dispatch`] hands each device command to the
/// adapter that owns the device, and [`Adapters::stop`] stops them all before the bridge stops.
/// This build starts no adapters yet; configured instances are reported and left alone.
#[derive(Debug, Default)]
pub(crate) struct Adapters {
    _private: (),
}

impl Adapters {
    /// Hands a device command to the adapter that owns the device.
    #[cfg(feature = "role-bridge")]
    fn dispatch(&self, command: &CommandEvent) {
        // No adapter adds devices yet, so no command can arrive; one that does is dropped and
        // the bridge answers the client.
        debug!(
            device = command.key.as_str(),
            "device command without an adapter, dropped"
        );
    }

    /// Stops every adapter; the bridge is still running while they stop.
    #[allow(clippy::unused_async)]
    async fn stop(&self) {
        debug!("adapters stopped");
    }
}

/// Starts the adapters configured in `instances` on the bridge.
///
/// # Errors
///
/// A start-up error when an adapter cannot start.
#[cfg(feature = "role-bridge")]
pub(crate) async fn start_adapters(
    _bridge: &Arc<Bridge>,
    instances: &BTreeMap<String, AdapterConfig>,
) -> Result<Adapters, StartError> {
    for (id, instance) in instances {
        warn!(
            adapter = id.as_str(),
            kind = instance.kind(),
            "adapter instance configured, but this build does not start adapters yet"
        );
    }
    Ok(Adapters::default())
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
    let bridge = Bridge::start(
        Box::new(RsMatterEngine::new()),
        identity,
        network,
        Attestation::Test,
        storage,
    )
    .await
    .map_err(|error| StartError::new("the node did not start", &error));
    let bridge = match bridge {
        Ok(bridge) => Arc::new(bridge),
        Err(error) => return Err(error),
    };

    let adapters = match start_adapters(&bridge, &config.adapters).await {
        Ok(adapters) => Arc::new(adapters),
        Err(error) => {
            let _ = bridge.stop().await;
            drop_bridge(bridge).await;
            return Err(error);
        }
    };
    let mut events = tokio::spawn(pump_events(Arc::clone(&bridge), Arc::clone(&adapters)));

    match bridge.commissioning_state().await {
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
            drop_bridge(bridge).await;
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
        if let Err(error) = bridge.stop().await {
            warn!(error = %error_chain(&error), "the bridge did not stop cleanly");
        }
        match events.await {
            Ok(reason) => debug!(?reason, "event stream ended"),
            Err(error) => warn!(%error, "the event task failed"),
        }
        drop_bridge(bridge).await;
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

/// Drops the last reference to the bridge on a blocking thread. The bridge owns the engine, and
/// dropping the rs-matter engine stops a node that still runs and joins the engine thread after
/// its persistence has flushed; the service waits for that before it exits.
#[cfg(feature = "role-bridge")]
async fn drop_bridge(bridge: Arc<Bridge>) {
    if let Err(error) = tokio::task::spawn_blocking(move || drop(bridge)).await {
        warn!(%error, "joining the engine thread failed");
    }
}

/// Pulls the bridge's events until the node stops and returns why it stopped.
#[cfg(feature = "role-bridge")]
async fn pump_events(bridge: Arc<Bridge>, adapters: Arc<Adapters>) -> StopReason {
    loop {
        match bridge.next_event().await {
            Ok(BridgeEvent::Command(command)) => adapters.dispatch(&command),
            Ok(BridgeEvent::CommissioningChanged(state)) => info!(
                commissioned = state.commissioned,
                window = ?state.window,
                "commissioning state changed"
            ),
            Ok(BridgeEvent::FabricAdded(fabric)) => info!(
                fabric = fabric.index.raw(),
                vendor_id = fabric.vendor_id.raw(),
                label = fabric.label.as_str(),
                "joined a fabric"
            ),
            Ok(BridgeEvent::FabricRemoved(index)) => {
                info!(fabric = index.raw(), "left a fabric");
            }
            Ok(BridgeEvent::Stopped(reason)) => {
                info!(?reason, "node stopped");
                return reason;
            }
            Ok(other) => debug!(?other, "bridge event"),
            Err(error) => {
                return StopReason::Failed(error_chain(&error));
            }
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
