//! The supervisor: runs adapter instances in isolation on tokio.
//!
//! The supervisor owns the bridge and every adapter instance of the product. Each adapter runs in
//! its own tokio task, so a panic, an error or a hang in one adapter never reaches another: a
//! failed adapter's devices are marked unreachable at once, the failure is logged, and the
//! adapter is rebuilt through its factory and restarted with exponential backoff. One task pumps
//! the bridge's events: commands are routed to the adapter that owns the device, everything else
//! (commissioning, fabrics, the stop) is published on [`Supervisor::events`].
//!
//! This is the one library crate on the product side that spawns tasks; the contract itself
//! stays runtime-neutral.

use std::any::Any;
use std::boxed::Box;
use std::future::Future;
use std::pin::Pin;
use std::string::{String, ToString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;
use std::vec::Vec;

use matter_rs_bridge::{Bridge, BridgeEvent, CommandEvent, DeviceCommand, Timer};
use matter_rs_engine::{BoxFuture, Status};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::{Instant, sleep, timeout};

use crate::adapter::Adapter;
use crate::context::{AdapterContext, Ownership, Push, RunState};
use crate::error::Error;
use crate::health::Health;
use crate::id::AdapterId;

/// How the supervisor restarts, stops and buffers. Start from [`Default`] and override what
/// differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorOptions {
    /// The delay before the first restart of a failed adapter; every further failure doubles it.
    pub initial_backoff: Duration,
    /// The longest delay between restarts.
    pub max_backoff: Duration,
    /// How long an adapter must have run for its next failure to count as the first again.
    pub healthy_after: Duration,
    /// How long an adapter has to return after it was asked to stop before it is aborted.
    pub stop_timeout: Duration,
    /// How many commands may wait for one adapter; further commands are answered `Busy`.
    pub command_capacity: usize,
    /// How many node events may wait on [`Supervisor::events`]; further events are dropped with
    /// a log line.
    pub event_capacity: usize,
}

impl Default for SupervisorOptions {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(5 * 60),
            healthy_after: Duration::from_secs(10 * 60),
            stop_timeout: Duration::from_secs(10),
            command_capacity: 256,
            event_capacity: 64,
        }
    }
}

/// Builds a fresh instance of an adapter; called before every start.
pub type Factory = Box<dyn Fn() -> Result<Box<dyn Adapter>, Error> + Send + Sync>;

/// Runs adapters on a bridge; see the [module](self) documentation.
///
/// Cloning gives another handle on the same supervisor, so that one task can
/// [`run`](Self::run) it while another asks for [`health`](Self::health) or calls
/// [`shutdown`](Self::shutdown).
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Inner>,
}

struct Inner {
    bridge: Arc<Bridge>,
    options: SupervisorOptions,
    ownership: Arc<Ownership>,
    slots: Mutex<Vec<Arc<Slot>>>,
    shutdown: watch::Sender<bool>,
    events_tx: mpsc::Sender<BridgeEvent>,
    events_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<BridgeEvent>>>,
    running: AtomicBool,
}

struct Slot {
    index: usize,
    id: AdapterId,
    factory: Factory,
    state: Mutex<SlotState>,
}

enum SlotState {
    /// Not started yet.
    Idle,
    /// A run is active; the adapter reports its health through the run state.
    Running(Arc<RunState>),
    /// Failed; waiting for the next attempt.
    Restarting { attempt: u32, next_in: Duration },
    /// Given up: the configuration is unusable.
    Failed(String),
    /// Ended, by shutdown or on its own.
    Stopped,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    fn set_state(&self, state: SlotState) {
        *lock(&self.state) = state;
    }

    fn running(&self) -> Option<Arc<RunState>> {
        match &*lock(&self.state) {
            SlotState::Running(run) => Some(Arc::clone(run)),
            _ => None,
        }
    }

    fn health(&self) -> Health {
        match &*lock(&self.state) {
            SlotState::Idle => Health::Starting,
            SlotState::Running(run) => run.health(),
            SlotState::Restarting { attempt, next_in } => Health::Restarting {
                attempt: *attempt,
                next_in: *next_in,
            },
            SlotState::Failed(reason) => Health::Failed(reason.clone()),
            SlotState::Stopped => Health::Stopped,
        }
    }
}

impl core::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Supervisor")
            .field("options", &self.inner.options)
            .field("adapters", &self.health())
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// A supervisor for `bridge`. Adapters are added with [`add`](Self::add), then
    /// [`run`](Self::run) starts them.
    #[must_use]
    pub fn new(bridge: Bridge, options: SupervisorOptions) -> Self {
        let (shutdown, _) = watch::channel(false);
        let (events_tx, events_rx) = mpsc::channel(options.event_capacity.max(1));
        Self {
            inner: Arc::new(Inner {
                bridge: Arc::new(bridge),
                options,
                ownership: Ownership::new(),
                slots: Mutex::new(Vec::new()),
                shutdown,
                events_tx,
                events_rx: Arc::new(tokio::sync::Mutex::new(events_rx)),
                running: AtomicBool::new(false),
            }),
        }
    }

    /// The bridge the adapters feed.
    #[must_use]
    pub fn bridge(&self) -> &Bridge {
        &self.inner.bridge
    }

    /// Adds an adapter instance. `factory` builds it before every start, so that a restart
    /// begins from a fresh instance; for an adapter crate that is a call of its
    /// [`AdapterFactory::build`](crate::AdapterFactory::build) with the instance's config.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when an adapter with this id is added already; [`Error::Internal`]
    /// when the supervisor runs already.
    pub fn add<F>(&self, id: AdapterId, factory: F) -> Result<(), Error>
    where
        F: Fn() -> Result<Box<dyn Adapter>, Error> + Send + Sync + 'static,
    {
        if self.inner.running.load(Ordering::SeqCst) {
            return Err(Error::Internal(String::from(
                "adapters are added before the supervisor runs",
            )));
        }
        let mut slots = lock(&self.inner.slots);
        if slots.iter().any(|slot| slot.id == id) {
            return Err(Error::Config(std::format!(
                "adapter {id} is configured twice"
            )));
        }
        let index = slots.len();
        slots.push(Arc::new(Slot {
            index,
            id,
            factory: Box::new(factory),
            state: Mutex::new(SlotState::Idle),
        }));
        Ok(())
    }

    /// The node's events that are not commands: commissioning changes, fabrics, the stop.
    ///
    /// Every call returns a handle on the same stream; an event is delivered to one reader.
    #[must_use]
    pub fn events(&self) -> EventStream {
        EventStream {
            receiver: Arc::clone(&self.inner.events_rx),
        }
    }

    /// The health of every adapter, in the order they were added: what the adapter reports
    /// while it runs, what the supervisor observes otherwise.
    #[must_use]
    pub fn health(&self) -> Vec<(AdapterId, Health)> {
        lock(&self.inner.slots)
            .iter()
            .map(|slot| (slot.id.clone(), slot.health()))
            .collect()
    }

    /// Asks the supervisor to stop: [`run`](Self::run) then shuts the adapters down, aborts
    /// those that do not return within the stop timeout, and stops the bridge.
    pub fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
    }

    /// Runs the adapters and the bridge's event pump until [`shutdown`](Self::shutdown) is
    /// called or the bridge stops on its own.
    ///
    /// Shutdown is orderly: every adapter's context signals shutdown, each adapter gets the stop
    /// timeout to return and is aborted otherwise, then the bridge is stopped; its stop event is
    /// the last one on [`events`](Self::events).
    ///
    /// # Errors
    ///
    /// [`Error::Internal`] when the supervisor runs already or ran before; the bridge's error
    /// when it cannot be stopped.
    pub async fn run(&self) -> Result<(), Error> {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Err(Error::Internal(String::from("the supervisor runs already")));
        }
        let inner = &self.inner;
        let slots: Vec<Arc<Slot>> = lock(&inner.slots).clone();
        let mut adapters = JoinSet::new();
        for slot in slots {
            adapters.spawn(supervise(Arc::clone(inner), slot));
        }
        let mut pump = AbortOnDrop(tokio::spawn(pump(Arc::clone(inner))));
        let mut shutdown = inner.shutdown.subscribe();
        let bridge_stopped = tokio::select! {
            _ = wait_shutdown(&mut shutdown) => false,
            _ = &mut pump => true,
        };
        log::info!("supervisor stops the adapters");
        inner.shutdown.send_replace(true);
        // Every supervising task stops or aborts its own adapter within the stop timeout; the
        // margin covers the task itself.
        let grace = inner.options.stop_timeout + Duration::from_secs(1);
        let all_done = async { while adapters.join_next().await.is_some() {} };
        if timeout(grace, all_done).await.is_err() {
            adapters.abort_all();
            while adapters.join_next().await.is_some() {}
        }
        if bridge_stopped {
            return Ok(());
        }
        inner.bridge.stop().await?;
        // The pump ends with the bridge's stop event.
        let _ = timeout(inner.options.stop_timeout, &mut pump).await;
        Ok(())
    }
}

impl Inner {
    /// Hands a command to the adapter that owns the device, or answers it when it cannot.
    async fn route(&self, command: CommandEvent) {
        let owner = self.ownership.owner_of(&command.key);
        let run = owner.as_ref().and_then(|owner| {
            lock(&self.slots)
                .get(owner.slot)
                .and_then(|slot| slot.running())
        });
        let (command, status) = match run {
            Some(run) => match run.push_command(command) {
                Push::Queued => return,
                Push::Full(command) => (command, Status::Busy),
                Push::Closed(command) => (command, Status::Failure),
            },
            None => (command, Status::Failure),
        };
        log::debug!(
            "command for device {} is not delivered (owner {:?}): answered {status:?}",
            command.key,
            owner.map(|owner| owner.id.to_string())
        );
        fail(command, status).await;
    }

    /// Publishes a node event; a full queue drops it with a log line.
    fn publish(&self, event: BridgeEvent) {
        if let Err(mpsc::error::TrySendError::Full(event)) = self.events_tx.try_send(event) {
            log::warn!("node event dropped, nobody reads the event stream: {event:?}");
        }
    }

    /// Marks every device of the adapter in `slot` unreachable.
    async fn set_unreachable(&self, slot: usize) {
        for key in self.ownership.keys_of(slot) {
            if let Some(device) = self.bridge.device(&key) {
                let _ = device.set_reachable(false).await;
            }
        }
    }

    /// Logs a failure, shows the restart and waits for its delay. `false` when shutdown came
    /// first.
    async fn wait_backoff(
        &self,
        slot: &Slot,
        attempt: u32,
        shutdown: &mut watch::Receiver<bool>,
        failure: &str,
    ) -> bool {
        let next_in = backoff_delay(
            attempt,
            self.options.initial_backoff,
            self.options.max_backoff,
        );
        log::warn!(
            "adapter {} failed: {failure}; restart {attempt} in {next_in:?}",
            slot.id
        );
        slot.set_state(SlotState::Restarting { attempt, next_in });
        tokio::select! {
            () = sleep(next_in) => true,
            _ = wait_shutdown(shutdown) => {
                slot.set_state(SlotState::Stopped);
                false
            }
        }
    }
}

/// Answers a command the adapter will not see: a raw command or write still waits for its
/// answer; a typed command was answered by the bridge already and is dropped.
async fn fail(command: CommandEvent, status: Status) {
    match command.command {
        DeviceCommand::Raw { response, .. } | DeviceCommand::Write { response, .. } => {
            let _ = response.fail(status).await;
        }
        _ => {}
    }
}

/// Waits until shutdown is requested. The watch guard is dropped here, so that the callers'
/// futures stay `Send`.
async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|requested| *requested).await;
}

/// The delay before restart `attempt` (1 for the first): `initial` doubled per attempt, capped
/// at `max`.
fn backoff_delay(attempt: u32, initial: Duration, max: Duration) -> Duration {
    let factor = 2_u32.saturating_pow(attempt.saturating_sub(1));
    initial.saturating_mul(factor).min(max)
}

/// Pumps the bridge's events until the bridge stops.
async fn pump(inner: Arc<Inner>) {
    loop {
        match inner.bridge.next_event().await {
            Ok(BridgeEvent::Command(command)) => inner.route(command).await,
            Ok(event) => {
                let stopped = matches!(event, BridgeEvent::Stopped(_));
                inner.publish(event);
                if stopped {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// How a run of an adapter ended.
enum Outcome {
    Finished(Result<(), Error>),
    Panicked(String),
    Cancelled,
}

impl From<Result<Result<(), Error>, JoinError>> for Outcome {
    fn from(result: Result<Result<(), Error>, JoinError>) -> Self {
        match result {
            Ok(result) => Self::Finished(result),
            Err(error) if error.is_cancelled() => Self::Cancelled,
            Err(error) => Self::Panicked(panic_message(&error.into_panic())),
        }
    }
}

/// The text of a panic payload, for the log.
fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        String::from("panic without message")
    }
}

/// Runs, watches and restarts the adapter of one slot until shutdown.
async fn supervise(inner: Arc<Inner>, slot: Arc<Slot>) {
    let options = &inner.options;
    let mut shutdown = inner.shutdown.subscribe();
    let mut attempt: u32 = 0;
    loop {
        if *shutdown.borrow() {
            slot.set_state(SlotState::Stopped);
            return;
        }
        let adapter = match (slot.factory)() {
            Ok(adapter) => adapter,
            Err(Error::Config(reason)) => {
                log::error!("adapter {} cannot be built: {reason}", slot.id);
                slot.set_state(SlotState::Failed(reason));
                return;
            }
            Err(error) => {
                attempt += 1;
                if !inner
                    .wait_backoff(&slot, attempt, &mut shutdown, &error.to_string())
                    .await
                {
                    return;
                }
                continue;
            }
        };
        let run = RunState::new(options.command_capacity);
        let ctx = AdapterContext::new(
            slot.id.clone(),
            Arc::clone(&run),
            Arc::clone(&inner.bridge),
            Arc::clone(&inner.ownership),
            slot.index,
        );
        slot.set_state(SlotState::Running(Arc::clone(&run)));
        let started = Instant::now();
        log::info!("adapter {} starts", slot.id);
        let mut task = AbortOnDrop(tokio::spawn(adapter.run(ctx)));
        let outcome = tokio::select! {
            result = &mut task => Outcome::from(result),
            _ = wait_shutdown(&mut shutdown) => {
                run.request_shutdown();
                match timeout(options.stop_timeout, &mut task).await {
                    Ok(Ok(Err(error))) => {
                        log::warn!("adapter {} failed while stopping: {error}", slot.id);
                    }
                    Ok(Err(error)) if !error.is_cancelled() => {
                        log::warn!(
                            "adapter {} panicked while stopping: {}",
                            slot.id,
                            panic_message(&error.into_panic())
                        );
                    }
                    Ok(_) => log::info!("adapter {} stopped", slot.id),
                    Err(_) => {
                        task.abort();
                        let _ = (&mut task).await;
                        log::warn!(
                            "adapter {} did not stop within {:?} and was aborted",
                            slot.id,
                            options.stop_timeout
                        );
                    }
                }
                slot.set_state(SlotState::Stopped);
                return;
            }
        };
        let failure = match outcome {
            Outcome::Finished(Ok(())) => {
                log::info!("adapter {} stopped on its own", slot.id);
                inner.set_unreachable(slot.index).await;
                slot.set_state(SlotState::Stopped);
                return;
            }
            Outcome::Finished(Err(Error::Config(reason))) => {
                log::error!(
                    "adapter {} stopped: invalid configuration: {reason}",
                    slot.id
                );
                inner.set_unreachable(slot.index).await;
                slot.set_state(SlotState::Failed(reason));
                return;
            }
            Outcome::Finished(Err(error)) => error.to_string(),
            Outcome::Panicked(message) => std::format!("panicked: {message}"),
            Outcome::Cancelled => {
                slot.set_state(SlotState::Stopped);
                return;
            }
        };
        inner.set_unreachable(slot.index).await;
        if started.elapsed() >= options.healthy_after {
            attempt = 0;
        }
        attempt += 1;
        if !inner
            .wait_backoff(&slot, attempt, &mut shutdown, &failure)
            .await
        {
            return;
        }
    }
}

/// A task handle that aborts its task when dropped, so that a supervising task that ends takes
/// its adapter with it.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> AbortOnDrop<T> {
    fn abort(&self) {
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The node's events that are not commands; see [`Supervisor::events`].
#[derive(Clone)]
pub struct EventStream {
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<BridgeEvent>>>,
}

impl core::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("EventStream")
    }
}

impl EventStream {
    /// Waits for the next event. `None` once the supervisor is gone; the last event of a
    /// running supervisor is [`BridgeEvent::Stopped`].
    pub async fn next(&self) -> Option<BridgeEvent> {
        self.receiver.lock().await.recv().await
    }
}

/// The bridge's response timer on tokio: the product passes it in the bridge's options so that
/// raw commands an adapter does not answer time out on the runtime's clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioTimer;

impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(sleep(duration))
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::boxed::Box;
    use std::string::String;
    use std::time::Duration;

    use super::{SupervisorOptions, backoff_delay, panic_message};

    #[test]
    fn backoff_doubles_from_one_second_to_the_cap() {
        let options = SupervisorOptions::default();
        let delay = |attempt| backoff_delay(attempt, options.initial_backoff, options.max_backoff);
        assert_eq!(delay(0), Duration::from_secs(1));
        assert_eq!(delay(1), Duration::from_secs(1));
        assert_eq!(delay(2), Duration::from_secs(2));
        assert_eq!(delay(3), Duration::from_secs(4));
        assert_eq!(delay(9), Duration::from_secs(256));
        assert_eq!(delay(10), Duration::from_secs(300));
        assert_eq!(delay(40), Duration::from_secs(300));
        assert_eq!(delay(u32::MAX), Duration::from_secs(300));
    }

    #[test]
    fn options_default_to_the_decided_values() {
        let options = SupervisorOptions::default();
        assert_eq!(options.initial_backoff, Duration::from_secs(1));
        assert_eq!(options.max_backoff, Duration::from_secs(300));
        assert_eq!(options.healthy_after, Duration::from_secs(600));
        assert_eq!(options.stop_timeout, Duration::from_secs(10));
        assert_eq!(options.command_capacity, 256);
        assert_eq!(options.event_capacity, 64);
    }

    #[test]
    fn panic_messages_are_extracted_from_str_and_string() {
        fn payload(value: impl Any + Send) -> Box<dyn Any + Send> {
            Box::new(value)
        }
        assert_eq!(panic_message(&payload("boom")), "boom");
        assert_eq!(panic_message(&payload(String::from("bang"))), "bang");
        assert_eq!(panic_message(&payload(42_u8)), "panic without message");
    }
}
