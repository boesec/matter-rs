//! A node with one bridged On/Off light, driven through the engine facade only.
//!
//! This is the counterpart of the local interop scenario (`tests/interop-local/`) and a
//! reference for how a product talks to the facade: it starts the engine, adds an endpoint
//! with its initial values, then answers the commands and writes the engine reports as events.
//! The cluster semantics (what `Toggle` does, which labels are acceptable) live here, not in
//! the engine.
//!
//! Usage:
//!
//! ```text
//! onoff_node [--state-dir DIR] [--interface NAME] [--port N] [--discriminator N]
//!            [--passcode N] [--ipv4-only] [--print-pairing]
//! ```
//!
//! `--print-pairing` prints the manual pairing code and the QR text on standard output; both
//! are secrets and are printed only on this explicit request. The state directory keeps the
//! engine's fabrics and the light's own state (on/off, label) across restarts. Every event is
//! reported on standard output as a line starting with `onoff_node:`; the log goes to standard
//! error. A `stop` line on standard input stops the node; otherwise it runs until it is killed.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use matter_rs_engine::testing::block_on;
use matter_rs_engine::{
    Attestation, AttributeId, ClusterDescription, ClusterId, CommandId, DeviceType, DeviceTypeId,
    DirectoryStorage, EndpointDescription, EndpointId, Engine, Event, NetworkConfig, NodeIdentity,
    NodeRole, ProductId, Response, Running, Secret, Status, Storage, Value, VendorId,
    bridged_device,
};
use matter_rs_engine_rsmatter::RsMatterEngine;

const AGGREGATOR: EndpointId = EndpointId::new(1);
const LIGHT: EndpointId = EndpointId::new(2);

const ON_OFF: ClusterId = ClusterId::new(0x0006);
const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0x0000);
const OFF: CommandId = CommandId::new(0x00);
const ON: CommandId = CommandId::new(0x01);
const TOGGLE: CommandId = CommandId::new(0x02);
const NODE_LABEL: AttributeId = AttributeId::new(0x0005);
const UNIQUE_ID: AttributeId = AttributeId::new(0x0012);

const DEVICE_TYPE_ON_OFF_LIGHT: DeviceTypeId = DeviceTypeId::new(0x0100);
const DEVICE_TYPE_BRIDGED_NODE: DeviceTypeId = DeviceTypeId::new(0x0013);

/// The longest label this light accepts; shorter than the cluster's 32 so that a client can
/// send a label the device refuses with `ConstraintError`.
const MAX_LABEL_LEN: usize = 16;

/// The product's own storage keys.
const KEY_ON: &str = "bridge/onoff-node/on";
const KEY_LABEL: &str = "bridge/onoff-node/label";

struct Options {
    state_dir: PathBuf,
    interface: Option<String>,
    port: u16,
    discriminator: u16,
    passcode: u32,
    ipv4_only: bool,
    print_pairing: bool,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        state_dir: std::env::temp_dir().join("matter-rs-onoff-node"),
        interface: None,
        port: 5540,
        discriminator: 3840,
        passcode: 20_202_021,
        ipv4_only: false,
        print_pairing: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--state-dir" => options.state_dir = PathBuf::from(value("--state-dir")?),
            "--interface" => options.interface = Some(value("--interface")?),
            "--port" => {
                options.port = value("--port")?
                    .parse()
                    .map_err(|e| format!("--port: {e}"))?;
            }
            "--discriminator" => {
                options.discriminator = value("--discriminator")?
                    .parse()
                    .map_err(|e| format!("--discriminator: {e}"))?;
            }
            "--passcode" => {
                options.passcode = value("--passcode")?
                    .parse()
                    .map_err(|e| format!("--passcode: {e}"))?;
            }
            "--ipv4-only" => options.ipv4_only = true,
            "--print-pairing" => options.print_pairing = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

fn light(label: &str, on: bool) -> EndpointDescription {
    EndpointDescription {
        id: LIGHT,
        device_types: vec![
            DeviceType {
                id: DEVICE_TYPE_ON_OFF_LIGHT,
                revision: 3,
            },
            DeviceType {
                id: DEVICE_TYPE_BRIDGED_NODE,
                revision: 1,
            },
        ],
        clusters: vec![
            ClusterDescription {
                id: ON_OFF,
                revision: 6,
                feature_map: 0,
                attributes: vec![ON_OFF_ATTRIBUTE],
                accepted_commands: vec![OFF, ON, TOGGLE],
                generated_commands: vec![],
                events: vec![],
            },
            ClusterDescription {
                id: bridged_device::CLUSTER,
                revision: 6,
                feature_map: 0,
                attributes: vec![NODE_LABEL, bridged_device::REACHABLE, UNIQUE_ID],
                accepted_commands: vec![],
                generated_commands: vec![],
                events: vec![bridged_device::REACHABLE_CHANGED],
            },
        ],
        parent: None,
        initial_values: vec![
            (ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(on)),
            (bridged_device::CLUSTER, NODE_LABEL, Value::from(label)),
            (
                bridged_device::CLUSTER,
                bridged_device::REACHABLE,
                Value::Bool(true),
            ),
            (
                bridged_device::CLUSTER,
                UNIQUE_ID,
                Value::from("onoff-node-light-1"),
            ),
        ],
    }
}

/// The light's persisted state, or its defaults.
fn load_state(storage: &dyn Storage) -> (bool, String) {
    let on = storage
        .load(KEY_ON)
        .ok()
        .flatten()
        .is_some_and(|bytes| bytes == b"1");
    let label = storage
        .load(KEY_LABEL)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| String::from("Light"));
    (on, label)
}

fn main() {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("onoff_node: {message}");
            std::process::exit(2);
        }
    };
    if let Err(message) = run(options) {
        eprintln!("onoff_node: {message}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), String> {
    let identity = NodeIdentity {
        vendor_id: VendorId::new(0xFFF1),
        product_id: ProductId::new(0x8001),
        vendor_name: "matter-rs".into(),
        product_name: "On/Off node".into(),
        node_label: "On/Off node".into(),
        serial_number: None,
        software_version: 1,
        software_version_string: env!("CARGO_PKG_VERSION").into(),
        unique_id: "onoff-node".into(),
        passcode: Secret::new(options.passcode),
        discriminator: options.discriminator,
    };
    let network = NetworkConfig {
        interface: options.interface.clone(),
        ipv4: true,
        ipv6: !options.ipv4_only,
        port: options.port,
    };
    let storage: Arc<dyn Storage> = Arc::new(
        DirectoryStorage::open(&options.state_dir).map_err(|e| format!("state directory: {e}"))?,
    );
    let (on, label) = load_state(&*storage);

    if options.print_pairing {
        let info = matter_rs_engine_rsmatter::pairing_info_for(&identity)
            .map_err(|e| format!("pairing info: {e}"))?;
        // The one place this example shows the codes, on explicit request.
        println!("onoff_node: pairing code {}", info.manual_code.expose());
        println!("onoff_node: qr text {}", info.qr_text.expose());
    }

    let engine = RsMatterEngine::new();
    let node = block_on(engine.start(
        identity,
        network,
        Attestation::Test,
        NodeRole::Bridge {
            aggregator: AGGREGATOR,
        },
        Arc::clone(&storage),
    ))
    .map_err(|e| format!("start: {e}"))?;
    let node: Arc<dyn Running> = Arc::from(node);
    block_on(node.add_endpoint(light(&label, on))).map_err(|e| format!("add endpoint: {e}"))?;
    let state = block_on(node.commissioning_state()).map_err(|e| format!("state: {e}"))?;
    println!(
        "onoff_node: ready on={on} label={label:?} commissioned={} window={:?}",
        state.commissioned, state.window
    );

    // A `stop` line on standard input stops the node gracefully; end of file does not, so
    // that the node runs detached (without a standard input) until it is killed.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) if line.trim() == "stop" => {
                    let _ = stop_tx.send(());
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let stopper = {
        let node = Arc::clone(&node);
        thread::spawn(move || {
            if stop_rx.recv().is_ok() {
                let _ = block_on(node.stop());
            }
        })
    };

    let mut on = on;
    loop {
        let event = match block_on(node.next_event()) {
            Ok(event) => event,
            Err(error) => {
                eprintln!("onoff_node: event stream ended: {error}");
                break;
            }
        };
        match event {
            Event::CommandInvoked {
                correlation,
                endpoint,
                cluster,
                command,
                ..
            } if endpoint == LIGHT && cluster == ON_OFF => {
                let new_state = match command {
                    OFF => Some(false),
                    ON => Some(true),
                    TOGGLE => Some(!on),
                    _ => None,
                };
                let response = match new_state {
                    Some(value) => {
                        on = value;
                        block_on(node.set_attribute(
                            LIGHT,
                            ON_OFF,
                            ON_OFF_ATTRIBUTE,
                            Value::Bool(on),
                        ))
                        .map_err(|e| format!("set attribute: {e}"))?;
                        let _ = storage.store(KEY_ON, if on { b"1" } else { b"0" });
                        println!("onoff_node: command {command} -> on={on}");
                        Response::success()
                    }
                    None => Response::status(Status::UnsupportedCommand),
                };
                block_on(node.respond(correlation, response))
                    .map_err(|e| format!("respond: {e}"))?;
            }
            Event::CommandInvoked { correlation, .. } => {
                block_on(node.respond(correlation, Response::status(Status::UnsupportedCommand)))
                    .map_err(|e| format!("respond: {e}"))?;
            }
            Event::WriteRequested {
                correlation,
                endpoint,
                cluster,
                attribute,
                value,
                ..
            } => {
                let status = if endpoint == LIGHT
                    && cluster == bridged_device::CLUSTER
                    && attribute == NODE_LABEL
                {
                    match String::try_from(value) {
                        Ok(label) if label.chars().count() <= MAX_LABEL_LEN => {
                            let _ = storage.store(KEY_LABEL, label.as_bytes());
                            println!("onoff_node: label -> {label:?}");
                            Status::Success
                        }
                        Ok(label) => {
                            println!(
                                "onoff_node: label {label:?} refused, longer than {MAX_LABEL_LEN}"
                            );
                            Status::ConstraintError
                        }
                        Err(_) => Status::InvalidDataType,
                    }
                } else {
                    Status::UnsupportedWrite
                };
                block_on(node.respond(correlation, Response::status(status)))
                    .map_err(|e| format!("respond: {e}"))?;
            }
            Event::CommissioningChanged(state) => {
                println!(
                    "onoff_node: commissioning commissioned={} window={:?}",
                    state.commissioned, state.window
                );
            }
            Event::FabricAdded(fabric) => {
                println!(
                    "onoff_node: fabric added index={} label={:?}",
                    fabric.index, fabric.label
                );
            }
            Event::FabricRemoved(index) => println!("onoff_node: fabric removed index={index}"),
            Event::Stopped(reason) => {
                println!("onoff_node: stopped {reason:?}");
                break;
            }
            _ => {}
        }
    }
    drop(stopper);
    Ok(())
}
