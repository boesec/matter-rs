//! The engine thread: builds the rs-matter node from the facade's inputs and drives it with a
//! single-threaded executor until the product stops it.
//!
//! Everything rs-matter owns lives on this thread's stack for the life of the node: the
//! `Matter` object, the interaction model, the responder, the sockets and the endpoint table.
//! The product side reaches them only through the request queue in [`Shared`], which the
//! service loop below drains next to rs-matter's own jobs (transport, mDNS, responders,
//! interaction model).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::{Ipv4Addr, UdpSocket};
use std::num::NonZeroU8;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use async_io::Async;
use embassy_time::Timer;
use matter_rs_engine::{
    Attestation, AttributeValue, CommissioningState, EndpointId, Error, Event, FabricIndex,
    FabricInfo, NetworkConfig, NodeIdentity, NodeRole, StopReason, Storage, Value, VendorId,
    Window, bridged_device,
};
use rs_matter::Matter;
use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::decl::operational_credentials;
use rs_matter::dm::endpoints::{EthSysHandlerBuilder, ROOT_ENDPOINT_ID};
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{AttrChangeNotifier, DataModel, EventEmitter, HandlerContext};
use rs_matter::fabric::FabricPersist;
use rs_matter::im::encoding::{EventPriority, IMBuffer};
use rs_matter::im::events::EVENT_DATA_TAG;
use rs_matter::im::{InteractionModel, InteractionModelState};
use rs_matter::persist::KvBlobStoreAccess;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::{
    CommWindowState, MAX_COMM_WINDOW_TIMEOUT_SECS, MIN_COMM_WINDOW_TIMEOUT_SECS,
};
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::transport::network::MatterLocalService;
use rs_matter::transport::network::mdns::builtin::{BuiltinMdns, Host};
use rs_matter::utils::futures::{Coalesce, Either, select, select4};
use rs_matter::utils::storage::pooled::Buffers;

use crate::attestation::NodeAttestation;
use crate::channel::{Reply, Request, Shared};
use crate::error::facade_error;
use crate::handler::{DynamicHandler, TableMetadata};
use crate::network;
use crate::pairing;
use crate::store::{FacadeKvStore, KeyIndex};
use crate::table::Table;
use crate::tlv::write_event_fields;

/// How often the service loop compares the commissioning state and the fabric table with the
/// last snapshot to report changes as events.
const POLL_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_millis(250);

/// How long the interaction model keeps running after the shut-down event, so that subscribers
/// receive it and deferred persistence can flush.
const SHUTDOWN_GRACE: embassy_time::Duration = embassy_time::Duration::from_millis(500);

/// The size of the engine thread's stack: rs-matter's objects live on it.
pub(crate) const THREAD_STACK_SIZE: usize = 16 * 1024 * 1024;

/// The longest node label Basic Information stores, in bytes.
const MAX_NODE_LABEL_LEN: usize = 32;

/// Everything the engine thread needs to build the node.
pub(crate) struct NodeConfig {
    pub(crate) identity: NodeIdentity,
    pub(crate) network: NetworkConfig,
    pub(crate) attestation: Attestation,
    pub(crate) role: NodeRole,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) response_timeout: Duration,
    pub(crate) endpoint_capacity: usize,
}

/// The body of the engine thread.
pub(crate) fn run_thread(
    config: NodeConfig,
    shared: Arc<Shared>,
    started: Reply<Result<(), Error>>,
) {
    let mut started = Some(started);
    let mut stop_reply = None;
    let outcome = run_node(config, &shared, &mut started, &mut stop_reply);
    match outcome {
        Ok(()) => {
            log::info!("node stopped");
            shared.finish(StopReason::Requested);
        }
        Err(error) => match started.take() {
            Some(started) => {
                log::error!("node failed to start: {error}");
                shared.abort();
                started.complete(Err(error));
            }
            None => {
                log::error!("node failed: {error}");
                shared.finish(StopReason::Failed(error.to_string()));
            }
        },
    }
    if let Some(reply) = stop_reply {
        reply.complete(Ok(()));
    }
}

/// Runs a future to completion on this thread, parking it between polls.
fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWaker(Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

fn io_error(what: &str, error: &std::io::Error) -> Error {
    Error::Engine(format!("{what}: {error}"))
}

/// Builds and runs the node. Returns when the product stops it, or with the error that ended
/// it.
fn run_node(
    config: NodeConfig,
    shared: &Arc<Shared>,
    started: &mut Option<Reply<Result<(), Error>>>,
    stop_reply: &mut Option<Reply<Result<(), Error>>>,
) -> Result<(), Error> {
    let NodeConfig {
        identity,
        network,
        attestation,
        role,
        storage,
        response_timeout,
        endpoint_capacity,
    } = config;
    let NodeRole::Bridge { aggregator } = role else {
        return Err(Error::Unsupported {
            operation: "this node role",
        });
    };
    let attestation = NodeAttestation::from_facade(attestation)?;
    let interface = network::select_interface(&network)?;

    let serial_number = identity.serial_number.clone().unwrap_or_default();
    let basic_info = BasicInfoConfig {
        vendor_name: &identity.vendor_name,
        vid: identity.vendor_id.raw(),
        product_name: &identity.product_name,
        pid: identity.product_id.raw(),
        sw_ver: identity.software_version,
        sw_ver_str: &identity.software_version_string,
        serial_no: &serial_number,
        unique_id: &identity.unique_id,
        device_name: &identity.product_name,
        ..BasicInfoConfig::new()
    };
    let matter = Matter::new(
        &basic_info,
        pairing::commissioning_data(&identity),
        attestation.as_dyn(),
        network.port,
    );

    let index = Arc::new(KeyIndex::load(Arc::clone(&storage)).map_err(Error::Storage)?);
    let kv = matter.kv(FacadeKvStore::new(Arc::clone(&storage), Arc::clone(&index)));
    matter.startup(&kv).map_err(|e| facade_error(&e))?;
    apply_node_label(&matter, &identity.node_label);

    let crypto = default_crypto(rand::rng(), attestation.private_key());
    let buffers: MatterBuffers = MatterBuffers::new();
    let im_state: InteractionModelState = InteractionModelState::new();

    let table = Rc::new(RefCell::new(Table::new(aggregator, endpoint_capacity)));
    let handler = DynamicHandler::new(Rc::clone(&table), Arc::clone(shared), response_timeout);
    let rand = crypto.rand().map_err(|e| facade_error(&e))?;
    let data_model = (
        TableMetadata(Rc::clone(&table)),
        EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(|endpoint, _| endpoint != ROOT_ENDPOINT_ID, handler),
    );
    let im = InteractionModel::new(&matter, &crypto, &buffers, data_model, &kv, &im_state);
    block_on(im.startup()).map_err(|e| facade_error(&e))?;
    let responder = DefaultResponder::new(&im);

    let matter_socket = network::bind_matter_socket(&network)
        .map_err(|e| io_error("binding the Matter socket", &e))?;
    let matter_socket: Async<UdpSocket> = Async::new_nonblocking(matter_socket)
        .map_err(|e| io_error("registering the Matter socket", &e))?;
    let (mdns_socket, ipv6_joined) = network::bind_mdns_socket(&network, &interface)
        .map_err(|e| io_error("binding the mDNS socket", &e))?;
    let mdns_socket: Async<UdpSocket> = Async::new_nonblocking(mdns_socket)
        .map_err(|e| io_error("registering the mDNS socket", &e))?;
    let hostname = network::hostname(&identity.unique_id);
    let host = Host {
        hostname: &hostname,
        ip: interface.ipv4.unwrap_or(Ipv4Addr::UNSPECIFIED),
        ipv6: &interface.ipv6,
    };
    let mut mdns = BuiltinMdns::new();

    if !matter.has_fabrics() {
        im.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS)
            .map_err(|e| facade_error(&e))?;
    }
    log::info!(
        "node started on interface {} (IPv4 {}, IPv6 {}), port {}, {} fabric(s)",
        interface.name,
        interface
            .ipv4
            .map_or_else(|| String::from("none"), |ip| ip.to_string()),
        if interface.ipv6.is_empty() {
            String::from("none")
        } else {
            format!("{} address(es)", interface.ipv6.len())
        },
        network.port,
        matter.with_state(|state| state.fabrics.iter().count()),
    );
    if let Some(started) = started.take() {
        started.complete(Ok(()));
    }

    let mut transport = pin!(matter.run(&crypto, &matter_socket, &matter_socket, &matter_socket));
    let mut mdns_job = pin!(mdns.run(
        &mdns_socket,
        &mdns_socket,
        &host,
        interface.ipv4,
        ipv6_joined.then_some(interface.index),
        &matter,
        &crypto,
    ));
    let mut respond = pin!(responder.run::<4, 4>());
    let mut im_job = pin!(im.run());
    let mut service = pin!(service_loop(
        shared, &im, &matter, &kv, &table, &index, &identity, stop_reply
    ));
    let jobs = select4(&mut transport, &mut mdns_job, &mut respond, &mut im_job).coalesce();
    let result = block_on(select(jobs, &mut service));

    match result {
        Either::Second(Ok(())) => Ok(()),
        Either::Second(Err(error)) | Either::First(Err(error)) => Err(facade_error(&error)),
        Either::First(Ok(())) => Err(Error::Engine(String::from(
            "an rs-matter job ended unexpectedly",
        ))),
    }
}

/// Puts the configured node label into Basic Information when nothing persisted overrides it.
fn apply_node_label(matter: &Matter<'_>, label: &str) {
    matter.with_state(|state| {
        if !state.basic_info_settings.node_label.is_empty() {
            return;
        }
        let mut end = label.len().min(MAX_NODE_LABEL_LEN);
        while !label.is_char_boundary(end) {
            end -= 1;
        }
        if end < label.len() {
            log::warn!("node label truncated to {MAX_NODE_LABEL_LEN} bytes");
        }
        // The label fits the bound by construction.
        let _ = state.basic_info_settings.node_label.push_str(&label[..end]);
    });
}

/// What the product sees of the node's commissioning and fabrics, for change detection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    state: CommissioningState,
    fabrics: BTreeMap<u8, FabricInfo>,
}

fn snapshot(matter: &Matter<'_>) -> Snapshot {
    let window = match matter.comm_window_state() {
        CommWindowState::Closed => None,
        CommWindowState::Open { .. } => {
            let mut enhanced = false;
            let _ = matter.mdns_services(|service| {
                if let MatterLocalService::Commissionable { enhanced: e, .. } = service {
                    enhanced = e;
                }
                Ok(())
            });
            Some(if enhanced {
                Window::Enhanced
            } else {
                Window::Basic
            })
        }
    };
    let fabrics: BTreeMap<u8, FabricInfo> = matter.with_state(|state| {
        state
            .fabrics
            .iter()
            .map(|fabric| {
                let index = fabric.fab_idx().get();
                (
                    index,
                    FabricInfo {
                        index: FabricIndex::new(index),
                        vendor_id: VendorId::new(fabric.vendor_id()),
                        fabric_id: fabric.fabric_id(),
                        node_id: fabric.node_id(),
                        label: fabric.label().to_owned(),
                    },
                )
            })
            .collect()
    });
    Snapshot {
        state: CommissioningState {
            commissioned: !fabrics.is_empty(),
            window,
        },
        fabrics,
    }
}

/// Reports the differences between two snapshots as events.
fn report_changes(shared: &Shared, before: &Snapshot, after: &Snapshot) {
    for (index, info) in &after.fabrics {
        if !before.fabrics.contains_key(index) {
            log::info!("fabric {index} added");
            shared.push_event(Event::FabricAdded(info.clone()));
        }
    }
    for index in before.fabrics.keys() {
        if !after.fabrics.contains_key(index) {
            log::info!("fabric {index} removed");
            shared.push_event(Event::FabricRemoved(FabricIndex::new(*index)));
        }
    }
    if before.state != after.state {
        log::info!(
            "commissioning state: commissioned {}, window {:?}",
            after.state.commissioned,
            after.state.window
        );
        shared.push_event(Event::CommissioningChanged(after.state));
    }
}

/// Drains the product's requests and watches the node for changes, next to rs-matter's jobs.
/// Returns when the product stops the node.
#[allow(clippy::too_many_arguments)]
async fn service_loop<C, B, T, K, const NS: usize, const NE: usize>(
    shared: &Arc<Shared>,
    im: &InteractionModel<'_, C, B, T, K, (), NS, NE>,
    matter: &Matter<'_>,
    kv: K,
    table: &Rc<RefCell<Table>>,
    index: &KeyIndex,
    identity: &NodeIdentity,
    stop_reply: &mut Option<Reply<Result<(), Error>>>,
) -> Result<(), rs_matter::error::Error>
where
    C: Crypto,
    B: Buffers<IMBuffer>,
    T: DataModel,
    K: KvBlobStoreAccess + Copy,
{
    let mut last = snapshot(matter);
    loop {
        let request = match select(shared.next_request(), Timer::after(POLL_INTERVAL)).await {
            Either::First(request) => Some(request),
            Either::Second(()) => None,
        };
        if let Some(request) = request {
            match request {
                Request::Stop(reply) => {
                    if let Err(error) = im.emit_shut_down() {
                        log::debug!("shut-down event not emitted: {error:?}");
                    }
                    Timer::after(SHUTDOWN_GRACE).await;
                    *stop_reply = Some(reply);
                    return Ok(());
                }
                other => {
                    handle_request(other, shared, im, matter, kv, table, index, identity).await;
                }
            }
        }
        let now = snapshot(matter);
        report_changes(shared, &last, &now);
        last = now;
    }
}

/// Handles one request and completes its reply.
#[allow(clippy::too_many_arguments)]
async fn handle_request<C, B, T, K, const NS: usize, const NE: usize>(
    request: Request,
    shared: &Arc<Shared>,
    im: &InteractionModel<'_, C, B, T, K, (), NS, NE>,
    matter: &Matter<'_>,
    kv: K,
    table: &Rc<RefCell<Table>>,
    index: &KeyIndex,
    identity: &NodeIdentity,
) where
    C: Crypto,
    B: Buffers<IMBuffer>,
    T: DataModel,
    K: KvBlobStoreAccess + Copy,
{
    match request {
        Request::AddEndpoint(description, reply) => {
            let id = description.id;
            let result = table.borrow_mut().add(description);
            reply.complete(result.map(|changed| {
                log::info!("endpoint {id} added");
                notify_endpoints(im, &changed);
            }));
        }
        Request::RemoveEndpoint(endpoint, reply) => {
            let result = table.borrow_mut().remove(endpoint);
            reply.complete(result.map(|changed| {
                log::info!("endpoint {endpoint} removed");
                notify_endpoints(im, &changed);
            }));
        }
        Request::SetAttributes(values, reply) => {
            let result = table.borrow_mut().set_values(&values);
            reply.complete(result.map(|()| notify_values(im, &values)));
        }
        Request::EmitEvent {
            endpoint,
            cluster,
            event,
            payload,
            reply,
        } => {
            let result = table
                .borrow()
                .check_event(endpoint, cluster, event)
                .and_then(|()| emit(im, endpoint, cluster, event, &payload));
            reply.complete(result);
        }
        Request::SetReachable(endpoint, reachable, reply) => {
            let value = AttributeValue {
                endpoint,
                cluster: bridged_device::CLUSTER,
                attribute: bridged_device::REACHABLE,
                value: Value::Bool(reachable),
            };
            // The event check covers the cluster; the attribute check is the one
            // `set_attribute` makes.
            let result = table
                .borrow()
                .check_event(
                    endpoint,
                    bridged_device::CLUSTER,
                    bridged_device::REACHABLE_CHANGED,
                )
                .and_then(|()| table.borrow_mut().set_values(std::slice::from_ref(&value)))
                .and_then(|()| {
                    notify_values(im, std::slice::from_ref(&value));
                    emit(
                        im,
                        endpoint,
                        bridged_device::CLUSTER,
                        bridged_device::REACHABLE_CHANGED,
                        &Value::Struct(vec![(
                            bridged_device::REACHABLE_NEW_VALUE_TAG,
                            Value::Bool(reachable),
                        )]),
                    )
                });
            reply.complete(result);
        }
        Request::CommissioningState(reply) => reply.complete(Ok(snapshot(matter).state)),
        Request::OpenWindow(window, timeout, reply) => {
            let result = match window {
                Window::Basic => {
                    let seconds = u16::try_from(timeout.as_secs())
                        .unwrap_or(u16::MAX)
                        .clamp(MIN_COMM_WINDOW_TIMEOUT_SECS, MAX_COMM_WINDOW_TIMEOUT_SECS);
                    im.open_basic_comm_window(seconds)
                        .map_err(|e| facade_error(&e))
                }
                _ => Err(Error::Unsupported {
                    operation: "enhanced commissioning window",
                }),
            };
            reply.complete(result);
        }
        Request::CloseWindow(reply) => {
            reply.complete(
                im.close_comm_window()
                    .map(|_| ())
                    .map_err(|e| facade_error(&e)),
            );
        }
        Request::Fabrics(reply) => {
            reply.complete(Ok(snapshot(matter).fabrics.into_values().collect()));
        }
        Request::RemoveFabric(fabric, reply) => {
            reply.complete(remove_fabric(im, matter, kv, fabric));
        }
        Request::PairingInfo(reply) => {
            reply.complete(
                pairing::pairing_info_for(identity).map_err(|error| match error {
                    crate::Error::Engine(inner) => inner,
                    other => Error::Engine(other.to_string()),
                }),
            );
        }
        Request::FactoryReset(reply) => {
            log::info!("factory reset");
            let result = matter
                .factory_reset(kv)
                .map_err(|e| facade_error(&e))
                .and(im.factory_reset().await.map_err(|e| facade_error(&e)))
                .and(index.remove_all().map_err(Error::Storage));
            reply.complete(result);
        }
        Request::Stop(reply) => {
            // Handled by the service loop; a stop that lands here would be a logic error.
            reply.complete(Err(Error::NotRunning));
        }
    }
    let _ = shared;
}

/// Notifies the interaction model that the metadata of `endpoints` changed, and bumps the
/// node's configuration version because its fixed-quality surface changed.
fn notify_endpoints<C, B, T, K, const NS: usize, const NE: usize>(
    im: &InteractionModel<'_, C, B, T, K, (), NS, NE>,
    endpoints: &[EndpointId],
) where
    C: Crypto,
    B: Buffers<IMBuffer>,
    T: DataModel,
    K: KvBlobStoreAccess,
{
    for endpoint in endpoints {
        im.notify_endpoint_changed(endpoint.raw());
    }
    if let Err(error) = im.bump_configuration_version() {
        log::warn!("configuration version not bumped: {error:?}");
    }
}

fn notify_values(notifier: &dyn AttrChangeNotifier, values: &[AttributeValue]) {
    for value in values {
        notifier.notify_attr_changed(
            value.endpoint.raw(),
            value.cluster.raw(),
            value.attribute.raw(),
        );
    }
}

fn emit<E: EventEmitter>(
    emitter: &E,
    endpoint: EndpointId,
    cluster: matter_rs_engine::ClusterId,
    event: matter_rs_engine::EventId,
    payload: &Value,
) -> Result<(), Error> {
    emitter
        .emit_event(
            endpoint.raw(),
            cluster.raw(),
            event.raw(),
            EventPriority::Info,
            |mut tw| write_event_fields(payload, &EVENT_DATA_TAG, &mut tw),
        )
        .map(|_| ())
        .map_err(|e| facade_error(&e))
}

/// Removes a fabric the way rs-matter's `RemoveFabric` command does, as far as its public
/// API allows: the fabric table entry, the sessions on it, the persisted record and the next
/// fabric index; then the handlers are told and the Operational Credentials cluster reported
/// as changed.
fn remove_fabric<C, B, T, K, const NS: usize, const NE: usize>(
    im: &InteractionModel<'_, C, B, T, K, (), NS, NE>,
    matter: &Matter<'_>,
    kv: K,
    fabric: FabricIndex,
) -> Result<(), Error>
where
    C: Crypto,
    B: Buffers<IMBuffer>,
    T: DataModel,
    K: KvBlobStoreAccess + Copy,
{
    let index = NonZeroU8::new(fabric.raw()).ok_or(Error::UnknownFabric(fabric))?;
    let mut persist = FabricPersist::strict(kv);
    let removed = matter.with_state(|state| {
        if state.fabrics.remove(index).is_err() {
            return Ok(false);
        }
        state
            .sessions
            .remove_where(|session| session.get_local_fabric_idx() == index.get());
        matter.transport().notify_session_removed();
        persist
            .store_next_fab_idx(state.fabrics.next_fab_idx())
            .and(persist.remove(index))
            .map(|()| true)
    });
    match removed {
        Ok(true) => {}
        Ok(false) => return Err(Error::UnknownFabric(fabric)),
        Err(error) => return Err(facade_error(&error)),
    }
    persist.run().map_err(|e| facade_error(&e))?;
    log::info!("fabric {fabric} removed by the product");
    im.notify_fabric_removed(index);
    im.notify_cluster_changed(ROOT_ENDPOINT_ID, operational_credentials::FULL_CLUSTER.id);
    Ok(())
}
