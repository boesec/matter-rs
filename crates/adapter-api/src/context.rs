//! What the host hands to a running adapter: its context.
//!
//! The context is the adapter's only connection to the product. It carries the adapter's
//! identity, the health cell the adapter reports into, the shutdown signal the host raises, and,
//! with the bridge role, the device registrar and the stream of commands for the adapter's own
//! devices. It is cheap to clone and every clone sees the same state, so an adapter can hand it
//! to the tasks it spawns.
//!
//! Everything here uses `core::future` only: the context works on any executor, from tokio to an
//! embedded one, and the host decides which.
//!
//! The host side (constructors, the shutdown request, the command queue, the ownership table) is
//! crate-private and used by the supervisor, which exists only with `std` and `role-bridge`; in
//! other builds it is compiled but unused, hence the allowance below.

#![cfg_attr(not(all(feature = "std", feature = "role-bridge")), allow(dead_code))]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::health::Health;
use crate::id::AdapterId;
use crate::sync::Lock;

#[cfg(feature = "role-bridge")]
pub use self::bridge::{CommandStream, DeviceRegistrar};
#[cfg(all(feature = "std", feature = "role-bridge"))]
pub(crate) use self::bridge::{Ownership, Push};

/// The context of one run of an adapter.
///
/// Created by the host for every run (a restarted adapter gets a fresh one), cloned freely by
/// the adapter. See the [module](self) documentation.
#[derive(Clone)]
pub struct AdapterContext {
    inner: Arc<Inner>,
}

struct Inner {
    id: AdapterId,
    run: Arc<RunState>,
    #[cfg(feature = "role-bridge")]
    devices: Arc<bridge::Devices>,
}

impl fmt::Debug for AdapterContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdapterContext")
            .field("id", &self.inner.id)
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}

impl AdapterContext {
    /// A context for a run without a bridge: the adapter can report health and wait for
    /// shutdown, nothing more. Hosts of the bridge role use the constructor of the bridge
    /// module instead.
    #[cfg(not(feature = "role-bridge"))]
    pub(crate) fn new(id: AdapterId, run: Arc<RunState>) -> Self {
        Self {
            inner: Arc::new(Inner { id, run }),
        }
    }

    /// The identity of this adapter instance.
    #[must_use]
    pub fn instance(&self) -> &AdapterId {
        &self.inner.id
    }

    /// Reports the health of the adapter; the host shows it to the operator.
    pub fn set_health(&self, health: Health) {
        self.inner.run.health.with(|current| *current = health);
    }

    /// The health as last reported through [`set_health`](Self::set_health);
    /// [`Health::Starting`] until the adapter reports.
    #[must_use]
    pub fn health(&self) -> Health {
        self.inner.run.health()
    }

    /// A future that resolves when the host asks the adapter to stop.
    ///
    /// The future is `Unpin` and can be polled from several places; a resolved signal stays
    /// resolved. After the signal the command stream delivers what is queued and then ends.
    pub fn shutdown(&self) -> Shutdown {
        Shutdown {
            run: Arc::clone(&self.inner.run),
        }
    }

    /// Whether the host has asked the adapter to stop.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.inner.run.is_shutdown_requested()
    }
}

/// Resolves when the host asks the adapter to stop; see [`AdapterContext::shutdown`].
#[must_use = "a shutdown signal does nothing until it is awaited"]
pub struct Shutdown {
    run: Arc<RunState>,
}

impl fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shutdown")
            .field("requested", &self.run.is_shutdown_requested())
            .finish()
    }
}

impl Future for Shutdown {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.run.shutdown.with(|signal| {
            if signal.requested {
                return Poll::Ready(());
            }
            if !signal.wakers.iter().any(|w| w.will_wake(cx.waker())) {
                signal.wakers.push(cx.waker().clone());
            }
            Poll::Pending
        })
    }
}

/// The state the host and the adapter share for one run of the adapter.
///
/// The host keeps it to read the health, to signal shutdown and, with the bridge role, to route
/// commands; the context carries it to the adapter.
pub(crate) struct RunState {
    health: Lock<Health>,
    shutdown: Lock<Signal>,
    #[cfg(feature = "role-bridge")]
    commands: Lock<bridge::CommandQueue>,
}

struct Signal {
    requested: bool,
    wakers: Vec<Waker>,
}

impl RunState {
    /// A fresh run state. `command_capacity` bounds the queue of routed commands (bridge role).
    pub(crate) fn new(command_capacity: usize) -> Arc<Self> {
        #[cfg(not(feature = "role-bridge"))]
        let _ = command_capacity;
        Arc::new(Self {
            health: Lock::new(Health::Starting),
            shutdown: Lock::new(Signal {
                requested: false,
                wakers: Vec::new(),
            }),
            #[cfg(feature = "role-bridge")]
            commands: Lock::new(bridge::CommandQueue::new(command_capacity)),
        })
    }

    /// The health the adapter reported last.
    pub(crate) fn health(&self) -> Health {
        self.health.with(|health| health.clone())
    }

    /// Asks the adapter to stop: resolves every [`Shutdown`] future and closes the command
    /// queue, so that the command stream ends once it is drained.
    pub(crate) fn request_shutdown(&self) {
        let wakers = self.shutdown.with(|signal| {
            signal.requested = true;
            core::mem::take(&mut signal.wakers)
        });
        for waker in wakers {
            waker.wake();
        }
        #[cfg(feature = "role-bridge")]
        self.commands.with(bridge::CommandQueue::close);
    }

    /// Whether shutdown was requested.
    pub(crate) fn is_shutdown_requested(&self) -> bool {
        self.shutdown.with(|signal| signal.requested)
    }
}

impl fmt::Debug for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunState")
            .field("health", &self.health())
            .field("shutdown_requested", &self.is_shutdown_requested())
            .finish()
    }
}

#[cfg(feature = "role-bridge")]
mod bridge {
    //! The bridge-role half of the context: devices and commands.

    use alloc::collections::{BTreeMap, VecDeque};
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::fmt;
    use core::task::{Poll, Waker};

    use matter_rs_bridge::{
        Bridge, CommandEvent, DeviceHandle, DeviceKey, DeviceSpec, InitialValues,
    };

    use super::{AdapterContext, Inner, RunState};
    use crate::error::Error;
    use crate::id::AdapterId;
    use crate::sync::Lock;

    /// What the adapter needs to work with devices: the bridge, the shared ownership table and
    /// its own slot in it.
    pub(crate) struct Devices {
        pub(crate) bridge: Arc<Bridge>,
        pub(crate) ownership: Arc<Ownership>,
        pub(crate) slot: usize,
    }

    impl AdapterContext {
        /// A context for a run of the adapter in `slot` on `bridge`.
        pub(crate) fn new(
            id: AdapterId,
            run: Arc<RunState>,
            bridge: Arc<Bridge>,
            ownership: Arc<Ownership>,
            slot: usize,
        ) -> Self {
            Self {
                inner: Arc::new(Inner {
                    id,
                    run,
                    devices: Arc::new(Devices {
                        bridge,
                        ownership,
                        slot,
                    }),
                }),
            }
        }

        /// The registrar through which the adapter adds, removes and forgets its devices.
        #[must_use]
        pub fn devices(&self) -> DeviceRegistrar {
            DeviceRegistrar {
                id: self.inner.id.clone(),
                devices: Arc::clone(&self.inner.devices),
            }
        }

        /// The stream of commands for the adapter's own devices.
        ///
        /// Commands are routed by device ownership: an adapter sees only the commands of the
        /// devices it added. One task should pull the stream.
        #[must_use]
        pub fn commands(&self) -> CommandStream {
            CommandStream {
                run: Arc::clone(&self.inner.run),
            }
        }
    }

    /// Adds, removes and forgets the devices of one adapter instance; see
    /// [`AdapterContext::devices`].
    ///
    /// Every device belongs to the adapter that added it: commands for it reach that adapter
    /// alone, and no other adapter may touch it ([`Error::ForeignDevice`]). Ownership survives a
    /// restart of the adapter: a device the previous run added is still there, marked
    /// unreachable, and [`add`](Self::add) with the same key replaces it (the endpoint numbers
    /// stay the same).
    pub struct DeviceRegistrar {
        id: AdapterId,
        devices: Arc<Devices>,
    }

    impl fmt::Debug for DeviceRegistrar {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("DeviceRegistrar")
                .field("adapter", &self.id)
                .field("devices", &self.keys())
                .finish()
        }
    }

    impl DeviceRegistrar {
        /// Adds a device to the bridge and claims it for this adapter.
        ///
        /// A device of the same key that this adapter added in an earlier run is replaced; the
        /// bridge gives it its old endpoint numbers back.
        ///
        /// # Errors
        ///
        /// [`Error::ForeignDevice`] when another adapter owns the key; the bridge's errors,
        /// wrapped, when the specification or the initial values are refused (nothing is then
        /// claimed).
        pub async fn add(
            &self,
            spec: DeviceSpec,
            initial: InitialValues,
        ) -> Result<DeviceHandle, Error> {
            let key = spec.key.clone();
            let newly_claimed = self.claim(&key)?;
            let result = self.replace(spec, initial).await;
            if result.is_err() && newly_claimed {
                self.devices.ownership.release(&key, self.devices.slot);
            }
            result
        }

        async fn replace(
            &self,
            spec: DeviceSpec,
            initial: InitialValues,
        ) -> Result<DeviceHandle, Error> {
            let bridge = &self.devices.bridge;
            if bridge.device(&spec.key).is_some() {
                bridge.remove_device(&spec.key).await?;
            }
            Ok(bridge.add_device(spec, initial).await?)
        }

        /// Removes a device from the bridge. The device stays owned by this adapter and keeps
        /// its endpoint numbers, so that [`add`](Self::add) brings it back unchanged.
        ///
        /// # Errors
        ///
        /// [`Error::ForeignDevice`] when another adapter owns the key; the bridge's errors,
        /// wrapped, among them `UnknownDevice` when the device is not added.
        pub async fn remove(&self, key: &DeviceKey) -> Result<(), Error> {
            self.check_owner(key)?;
            Ok(self.devices.bridge.remove_device(key).await?)
        }

        /// Forgets a device: removes it if it is added, releases its endpoint numbers, deletes
        /// its persisted values and gives up the ownership. A device added later under the same
        /// key is a new device.
        ///
        /// # Errors
        ///
        /// [`Error::ForeignDevice`] when another adapter owns the key; the bridge's errors,
        /// wrapped.
        pub async fn forget(&self, key: &DeviceKey) -> Result<(), Error> {
            self.check_owner(key)?;
            self.devices.bridge.forget_device(key).await?;
            self.devices.ownership.release(key, self.devices.slot);
            Ok(())
        }

        /// A handle on a device this adapter owns and that is added right now.
        #[must_use]
        pub fn get(&self, key: &DeviceKey) -> Option<DeviceHandle> {
            let owned = self
                .devices
                .ownership
                .owner_of(key)
                .is_some_and(|owner| owner.slot == self.devices.slot);
            owned.then(|| self.devices.bridge.device(key)).flatten()
        }

        /// The keys of the devices this adapter owns, added or removed, in order.
        #[must_use]
        pub fn keys(&self) -> Vec<DeviceKey> {
            self.devices.ownership.keys_of(self.devices.slot)
        }

        /// Claims `key` for this adapter; `Ok(true)` when it was unowned before.
        fn claim(&self, key: &DeviceKey) -> Result<bool, Error> {
            let owner = Owner {
                slot: self.devices.slot,
                id: self.id.clone(),
            };
            match self.devices.ownership.claim(key, owner) {
                Claim::New => Ok(true),
                Claim::Own => Ok(false),
                Claim::Foreign(owner) => Err(Error::ForeignDevice {
                    key: key.clone(),
                    owner: owner.id,
                }),
            }
        }

        fn check_owner(&self, key: &DeviceKey) -> Result<(), Error> {
            match self.devices.ownership.owner_of(key) {
                Some(owner) if owner.slot != self.devices.slot => Err(Error::ForeignDevice {
                    key: key.clone(),
                    owner: owner.id,
                }),
                _ => Ok(()),
            }
        }
    }

    /// The commands routed to one adapter; see [`AdapterContext::commands`].
    pub struct CommandStream {
        run: Arc<RunState>,
    }

    impl fmt::Debug for CommandStream {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("CommandStream")
                .field("queued", &self.run.commands.with(|queue| queue.items.len()))
                .finish()
        }
    }

    impl CommandStream {
        /// Waits for the next command for one of the adapter's devices.
        ///
        /// `None` once the host has asked the adapter to stop and the queue is drained; the
        /// stream then stays ended.
        pub async fn next(&mut self) -> Option<CommandEvent> {
            core::future::poll_fn(|cx| {
                self.run.commands.with(|queue| {
                    if let Some(event) = queue.items.pop_front() {
                        return Poll::Ready(Some(event));
                    }
                    if queue.closed {
                        return Poll::Ready(None);
                    }
                    queue.waker = Some(cx.waker().clone());
                    Poll::Pending
                })
            })
            .await
        }
    }

    /// The bounded queue of commands routed to one run of an adapter.
    pub(crate) struct CommandQueue {
        items: VecDeque<CommandEvent>,
        capacity: usize,
        closed: bool,
        waker: Option<Waker>,
    }

    /// The outcome of queueing a command.
    pub(crate) enum Push {
        /// The adapter will pull it.
        Queued,
        /// The queue holds `capacity` commands the adapter has not pulled yet.
        Full(CommandEvent),
        /// The run was asked to stop; no command is queued any more.
        Closed(CommandEvent),
    }

    impl CommandQueue {
        pub(crate) fn new(capacity: usize) -> Self {
            Self {
                items: VecDeque::new(),
                capacity,
                closed: false,
                waker: None,
            }
        }

        pub(crate) fn close(&mut self) {
            self.closed = true;
            if let Some(waker) = self.waker.take() {
                waker.wake();
            }
        }
    }

    impl RunState {
        /// Queues a command for the adapter of this run.
        pub(crate) fn push_command(&self, event: CommandEvent) -> Push {
            let (outcome, waker) = self.commands.with(|queue| {
                if queue.closed {
                    return (Push::Closed(event), None);
                }
                if queue.items.len() >= queue.capacity {
                    return (Push::Full(event), None);
                }
                queue.items.push_back(event);
                (Push::Queued, queue.waker.take())
            });
            if let Some(waker) = waker {
                waker.wake();
            }
            outcome
        }
    }

    /// Which adapter instance owns which device key, shared by every context of a host.
    pub(crate) struct Ownership {
        map: Lock<BTreeMap<DeviceKey, Owner>>,
    }

    /// The owner of a device: the slot of the adapter at its host and its id.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Owner {
        pub(crate) slot: usize,
        pub(crate) id: AdapterId,
    }

    /// The outcome of claiming a key.
    pub(crate) enum Claim {
        /// The key was unowned and is now the claimant's.
        New,
        /// The claimant owned the key already.
        Own,
        /// Another adapter owns the key.
        Foreign(Owner),
    }

    impl Ownership {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                map: Lock::new(BTreeMap::new()),
            })
        }

        pub(crate) fn owner_of(&self, key: &DeviceKey) -> Option<Owner> {
            self.map.with(|map| map.get(key).cloned())
        }

        pub(crate) fn claim(&self, key: &DeviceKey, owner: Owner) -> Claim {
            self.map.with(|map| match map.get(key) {
                None => {
                    map.insert(key.clone(), owner);
                    Claim::New
                }
                Some(existing) if existing.slot == owner.slot => Claim::Own,
                Some(existing) => Claim::Foreign(existing.clone()),
            })
        }

        /// Releases `key` if `slot` owns it.
        pub(crate) fn release(&self, key: &DeviceKey, slot: usize) {
            self.map.with(|map| {
                if map.get(key).is_some_and(|owner| owner.slot == slot) {
                    map.remove(key);
                }
            });
        }

        pub(crate) fn keys_of(&self, slot: usize) -> Vec<DeviceKey> {
            self.map.with(|map| {
                map.iter()
                    .filter(|(_, owner)| owner.slot == slot)
                    .map(|(key, _)| key.clone())
                    .collect()
            })
        }
    }

    impl fmt::Debug for Ownership {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.map
                .with(|map| f.debug_map().entries(map.iter()).finish())
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::task::Wake;
    use core::future::Future;
    use core::pin::pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};

    use super::{AdapterContext, RunState};
    use crate::health::Health;
    use crate::id::AdapterId;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn context(run: &Arc<RunState>) -> AdapterContext {
        #[cfg(not(feature = "role-bridge"))]
        {
            AdapterContext::new(AdapterId::new("mock", "a"), Arc::clone(run))
        }
        #[cfg(feature = "role-bridge")]
        {
            let fake = matter_rs_engine::testing::FakeEngine::new();
            let bridge = matter_rs_engine::testing::block_on(matter_rs_bridge::Bridge::start(
                alloc::boxed::Box::new(fake),
                matter_rs_engine::NodeIdentity {
                    vendor_id: matter_rs_engine::VendorId::new(0xFFF1),
                    product_id: matter_rs_engine::ProductId::new(0x8001),
                    vendor_name: "matter-rs".into(),
                    product_name: "Bridge".into(),
                    node_label: "Bridge".into(),
                    serial_number: None,
                    software_version: 1,
                    software_version_string: "0.1.0".into(),
                    unique_id: "bridge-1".into(),
                    passcode: matter_rs_engine::Secret::new(20_202_021),
                    discriminator: 3840,
                },
                matter_rs_engine::NetworkConfig::default(),
                matter_rs_engine::Attestation::Test,
                Arc::new(matter_rs_engine::MemoryStorage::new()),
            ))
            .expect("the bridge starts");
            AdapterContext::new(
                AdapterId::new("mock", "a"),
                Arc::clone(run),
                Arc::new(bridge),
                super::bridge::Ownership::new(),
                0,
            )
        }
    }

    #[test]
    fn context_is_clone_send_and_sync() {
        fn assert_bounds<T: Clone + Send + Sync + 'static>() {}
        assert_bounds::<AdapterContext>();
    }

    #[test]
    fn health_starts_at_starting_and_follows_the_report() {
        let run = RunState::new(4);
        let ctx = context(&run);
        assert_eq!(ctx.health(), Health::Starting);
        assert_eq!(run.health(), Health::Starting);
        ctx.set_health(Health::Healthy);
        assert_eq!(ctx.clone().health(), Health::Healthy);
        assert_eq!(run.health(), Health::Healthy);
        assert_eq!(ctx.instance(), &AdapterId::new("mock", "a"));
        assert!(alloc::format!("{ctx:?}").contains("Healthy"));
    }

    #[test]
    fn shutdown_wakes_every_waiter_once_requested() {
        let run = RunState::new(4);
        let ctx = context(&run);
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut cx = Context::from_waker(&waker);

        let mut first = pin!(ctx.shutdown());
        let mut second = pin!(ctx.shutdown());
        assert_eq!(first.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(first.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(second.as_mut().poll(&mut cx), Poll::Pending);
        assert!(!ctx.is_shutting_down());

        run.request_shutdown();
        assert!(ctx.is_shutting_down());
        // The two futures share the signal; the same waker is registered once.
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(first.as_mut().poll(&mut cx), Poll::Ready(()));
        assert_eq!(second.as_mut().poll(&mut cx), Poll::Ready(()));
        // A future created after the request resolves at once.
        assert_eq!(pin!(ctx.shutdown()).poll(&mut cx), Poll::Ready(()));
        assert!(alloc::format!("{:?}", ctx.shutdown()).contains("true"));
    }

    #[cfg(feature = "role-bridge")]
    mod bridge {
        use alloc::sync::Arc;
        use core::future::Future;
        use core::pin::pin;
        use core::task::{Context, Poll};

        use matter_rs_bridge::{
            Attribute, CommandEvent, DeviceCommand, DeviceKey, DeviceKind, DeviceSpec,
            InitialValues,
        };
        use matter_rs_engine::testing::block_on;

        use super::super::bridge::{Claim, Owner, Ownership, Push};
        use super::super::{AdapterContext, RunState};
        use super::context;
        use crate::error::Error;
        use crate::id::AdapterId;

        fn key(name: &str) -> DeviceKey {
            DeviceKey::new(name).unwrap()
        }

        fn event(name: &str) -> CommandEvent {
            CommandEvent {
                key: key(name),
                part: Some(0),
                command: DeviceCommand::SetOnOff { on: true },
            }
        }

        fn noop_context() -> Context<'static> {
            Context::from_waker(core::task::Waker::noop())
        }

        #[test]
        fn commands_are_queued_in_order_and_bounded() {
            let run = RunState::new(2);
            let ctx = context(&run);
            let mut stream = ctx.commands();
            let mut cx = noop_context();
            assert_eq!(pin!(stream.next()).poll(&mut cx), Poll::Pending);

            assert!(matches!(run.push_command(event("a")), Push::Queued));
            assert!(matches!(run.push_command(event("b")), Push::Queued));
            let Push::Full(rejected) = run.push_command(event("c")) else {
                panic!("the third command must be rejected");
            };
            assert_eq!(rejected.key.as_str(), "c");

            let first = pin!(stream.next()).poll(&mut cx);
            assert!(matches!(first, Poll::Ready(Some(e)) if e.key.as_str() == "a"));
            let second = pin!(stream.next()).poll(&mut cx);
            assert!(matches!(second, Poll::Ready(Some(e)) if e.key.as_str() == "b"));
            assert_eq!(pin!(stream.next()).poll(&mut cx), Poll::Pending);
            assert!(alloc::format!("{stream:?}").contains("queued: 0"));
        }

        #[test]
        fn shutdown_closes_the_stream_after_draining() {
            let run = RunState::new(4);
            let ctx = context(&run);
            let mut stream = ctx.commands();
            assert!(matches!(run.push_command(event("a")), Push::Queued));
            run.request_shutdown();
            let Push::Closed(_) = run.push_command(event("b")) else {
                panic!("a closed queue rejects");
            };
            let mut cx = noop_context();
            assert!(matches!(
                pin!(stream.next()).poll(&mut cx),
                Poll::Ready(Some(e)) if e.key.as_str() == "a"
            ));
            assert_eq!(pin!(stream.next()).poll(&mut cx), Poll::Ready(None));
            assert_eq!(pin!(stream.next()).poll(&mut cx), Poll::Ready(None));
        }

        #[test]
        fn ownership_claims_releases_and_lists() {
            let ownership = Ownership::new();
            let a = Owner {
                slot: 0,
                id: AdapterId::new("mock", "a"),
            };
            let b = Owner {
                slot: 1,
                id: AdapterId::new("mock", "b"),
            };
            assert!(matches!(ownership.claim(&key("x"), a.clone()), Claim::New));
            assert!(matches!(ownership.claim(&key("x"), a.clone()), Claim::Own));
            assert!(matches!(
                ownership.claim(&key("x"), b.clone()),
                Claim::Foreign(owner) if owner == a
            ));
            assert!(matches!(ownership.claim(&key("y"), b), Claim::New));
            assert_eq!(ownership.keys_of(0), alloc::vec![key("x")]);
            assert_eq!(ownership.keys_of(1), alloc::vec![key("y")]);
            // Only the owner releases.
            ownership.release(&key("x"), 1);
            assert_eq!(ownership.owner_of(&key("x")), Some(a));
            ownership.release(&key("x"), 0);
            assert_eq!(ownership.owner_of(&key("x")), None);
            assert!(alloc::format!("{ownership:?}").contains("\"y\""));
        }

        fn light(name: &str) -> (DeviceSpec, InitialValues) {
            (
                DeviceSpec::single(key(name), name, DeviceKind::OnOffLight),
                InitialValues::new().value(Attribute::OnOff, false),
            )
        }

        #[test]
        fn registrar_adds_replaces_removes_and_forgets() {
            let run = RunState::new(4);
            let ctx = context(&run);
            let devices = ctx.devices();
            let (spec, initial) = light("lamp");
            let handle = block_on(devices.add(spec, initial)).expect("added");
            let numbers = handle.endpoints().unwrap();
            assert_eq!(devices.keys(), alloc::vec![key("lamp")]);
            assert!(devices.get(&key("lamp")).is_some());
            assert!(alloc::format!("{devices:?}").contains("lamp"));

            // Adding the same key again replaces the device and keeps its numbers.
            let (spec, initial) = light("lamp");
            let again = block_on(devices.add(spec, initial)).expect("replaced");
            assert_eq!(again.endpoints().unwrap(), numbers);
            // Handles are by key, so the old one serves the replacement too.
            block_on(handle.set(0, Attribute::OnOff, true)).expect("the old handle still works");
            assert_eq!(
                again.value(0, Attribute::OnOff),
                Some(matter_rs_engine::Value::Bool(true))
            );

            block_on(devices.remove(&key("lamp"))).expect("removed");
            assert!(devices.get(&key("lamp")).is_none());
            assert!(matches!(
                handle.set(0, Attribute::OnOff, false).now_or_never_err(),
                Some(matter_rs_bridge::Error::UnknownDevice(_))
            ));
            assert_eq!(devices.keys(), alloc::vec![key("lamp")]);
            assert!(matches!(
                block_on(devices.remove(&key("lamp"))),
                Err(Error::Bridge(matter_rs_bridge::Error::UnknownDevice(_)))
            ));

            block_on(devices.forget(&key("lamp"))).expect("forgotten");
            assert!(devices.keys().is_empty());
        }

        #[test]
        fn registrar_refuses_foreign_devices_and_keeps_no_failed_claim() {
            let run = RunState::new(4);
            let ctx = context(&run);
            let devices = ctx.devices();
            let (spec, initial) = light("lamp");
            block_on(devices.add(spec, initial)).expect("added");

            // A second adapter on the same bridge and ownership table.
            let other = AdapterContext::new(
                AdapterId::new("mock", "b"),
                RunState::new(4),
                Arc::clone(&ctx.inner.devices.bridge),
                Arc::clone(&ctx.inner.devices.ownership),
                1,
            );
            let foreign = other.devices();
            let (spec, initial) = light("lamp");
            let error = block_on(foreign.add(spec, initial)).unwrap_err();
            assert!(matches!(
                &error,
                Error::ForeignDevice { key, owner } if key.as_str() == "lamp" && owner.instance == "a"
            ));
            assert!(matches!(
                block_on(foreign.remove(&key("lamp"))),
                Err(Error::ForeignDevice { .. })
            ));
            assert!(matches!(
                block_on(foreign.forget(&key("lamp"))),
                Err(Error::ForeignDevice { .. })
            ));
            assert!(foreign.get(&key("lamp")).is_none());

            // A refused specification leaves no claim behind.
            let spec = DeviceSpec::single(key("bad"), "Bad", DeviceKind::OnOffLight);
            let error = block_on(foreign.add(spec, InitialValues::new())).unwrap_err();
            assert!(matches!(error, Error::Bridge(_)));
            assert!(foreign.keys().is_empty());
        }

        /// Polls a future once and returns its error if it is ready with one.
        trait NowOrNever: Future + Sized {
            fn now_or_never_err<T, E>(self) -> Option<E>
            where
                Self: Future<Output = Result<T, E>>,
            {
                match pin!(self).poll(&mut noop_context()) {
                    Poll::Ready(Err(error)) => Some(error),
                    _ => None,
                }
            }
        }

        impl<F: Future + Sized> NowOrNever for F {}
    }
}
