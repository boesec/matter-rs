//! A mock adapter driven by the test.
//!
//! [`MockControl`] is the test's side: it builds [`MockAdapter`] instances for the supervisor
//! (as many as the supervisor restarts), tells the running instance what to do (add a device,
//! set a value, report health, fail, panic, hang, stop) and receives the commands the instance
//! was given. Every action is executed by the adapter's own task, so a test that gets an answer
//! has proven that the task is alive and unblocked.
//!
//! This is how the supervisor is tested, and how adapter wiring (a config kind, a service) can be
//! tested before a real adapter exists.

use std::boxed::Box;
use std::collections::VecDeque;
use std::string::String;
use std::sync::{Arc, Mutex, PoisonError};

use matter_rs_bridge::{
    Attribute, CommandEvent, DeviceHandle, DeviceKey, DeviceSpec, InitialValues, PartIndex,
};
use matter_rs_engine::{BoxFuture, Value};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::adapter::Adapter;
use crate::context::AdapterContext;
use crate::error::Error;
use crate::health::Health;

/// The test's handle on a mock adapter; see the [module](self) documentation.
#[derive(Clone)]
pub struct MockControl {
    shared: Arc<Shared>,
}

/// What the test can observe about the runs of a mock adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MockStatus {
    /// How many runs started.
    pub runs: usize,
    /// How many runs ended: returned, failed, panicked or were aborted.
    pub ended: usize,
    /// How many runs saw the shutdown signal and returned because of it.
    pub shutdowns: usize,
}

impl MockStatus {
    /// Whether a run is active.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.runs > self.ended
    }
}

struct Shared {
    actions: Mutex<VecDeque<Action>>,
    wake: Notify,
    status: watch::Sender<MockStatus>,
    commands_tx: mpsc::UnboundedSender<CommandEvent>,
    commands_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<CommandEvent>>,
    context: Mutex<Option<AdapterContext>>,
}

enum Action {
    AddDevice {
        spec: DeviceSpec,
        initial: InitialValues,
        reply: oneshot::Sender<Result<DeviceHandle, Error>>,
    },
    RemoveDevice {
        key: DeviceKey,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Set {
        key: DeviceKey,
        part: PartIndex,
        attribute: Attribute,
        value: Value,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Health(Health),
    Fail(Error),
    Panic(String),
    Hang,
    Stop,
}

impl Default for MockControl {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for MockControl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MockControl")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl MockControl {
    /// A control without a running adapter.
    #[must_use]
    pub fn new() -> Self {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (status, _) = watch::channel(MockStatus::default());
        Self {
            shared: Arc::new(Shared {
                actions: Mutex::new(VecDeque::new()),
                wake: Notify::new(),
                status,
                commands_tx,
                commands_rx: tokio::sync::Mutex::new(commands_rx),
                context: Mutex::new(None),
            }),
        }
    }

    /// A new adapter instance bound to this control.
    #[must_use]
    pub fn adapter(&self) -> Box<dyn Adapter> {
        Box::new(MockAdapter {
            shared: Arc::clone(&self.shared),
        })
    }

    /// A factory for the supervisor that builds [`adapter`](Self::adapter) on every start.
    pub fn factory(&self) -> impl Fn() -> Result<Box<dyn Adapter>, Error> + Send + Sync + 'static {
        let control = self.clone();
        move || Ok(control.adapter())
    }

    /// The runs so far.
    #[must_use]
    pub fn status(&self) -> MockStatus {
        *self.shared.status.borrow()
    }

    /// Waits until the status satisfies `condition` and returns it.
    pub async fn wait_for(&self, mut condition: impl FnMut(&MockStatus) -> bool) -> MockStatus {
        let mut receiver = self.shared.status.subscribe();
        match receiver.wait_for(|status| condition(status)).await {
            Ok(status) => *status,
            // The control holds the sender, so the channel never closes.
            Err(_) => self.status(),
        }
    }

    /// Waits until a run is active.
    pub async fn wait_until_running(&self) {
        self.wait_for(MockStatus::is_running).await;
    }

    /// Waits until `runs` runs have started.
    pub async fn wait_for_runs(&self, runs: usize) {
        self.wait_for(|status| status.runs >= runs).await;
    }

    /// Waits until `ended` runs have ended.
    pub async fn wait_for_ended(&self, ended: usize) {
        self.wait_for(|status| status.ended >= ended).await;
    }

    /// The context of the current or last run.
    #[must_use]
    pub fn context(&self) -> Option<AdapterContext> {
        lock(&self.shared.context).clone()
    }

    /// Makes the running adapter add a device through its context.
    ///
    /// # Errors
    ///
    /// The registrar's error; [`Error::Internal`] when no run is active to execute the action.
    pub async fn add_device(
        &self,
        spec: DeviceSpec,
        initial: InitialValues,
    ) -> Result<DeviceHandle, Error> {
        let (reply, answer) = oneshot::channel();
        self.push(Action::AddDevice {
            spec,
            initial,
            reply,
        });
        answer.await.unwrap_or_else(|_| Err(not_running()))
    }

    /// Makes the running adapter remove a device through its context.
    ///
    /// # Errors
    ///
    /// The registrar's error; [`Error::Internal`] when no run is active to execute the action.
    pub async fn remove_device(&self, key: &DeviceKey) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.push(Action::RemoveDevice {
            key: key.clone(),
            reply,
        });
        answer.await.unwrap_or_else(|_| Err(not_running()))
    }

    /// Makes the running adapter report a value of one of its devices.
    ///
    /// # Errors
    ///
    /// The device handle's error; `UnknownDevice` when the adapter does not own the device;
    /// [`Error::Internal`] when no run is active to execute the action.
    pub async fn set(
        &self,
        key: &DeviceKey,
        part: PartIndex,
        attribute: Attribute,
        value: impl Into<Value>,
    ) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.push(Action::Set {
            key: key.clone(),
            part,
            attribute,
            value: value.into(),
            reply,
        });
        answer.await.unwrap_or_else(|_| Err(not_running()))
    }

    /// Makes the running adapter report its health.
    pub fn report(&self, health: Health) {
        self.push(Action::Health(health));
    }

    /// Makes the running adapter return `error`.
    pub fn fail(&self, error: Error) {
        self.push(Action::Fail(error));
    }

    /// Makes the running adapter panic with `message`.
    pub fn panic(&self, message: impl Into<String>) {
        self.push(Action::Panic(message.into()));
    }

    /// Makes the running adapter hang: it stops processing anything, including shutdown.
    pub fn hang(&self) {
        self.push(Action::Hang);
    }

    /// Makes the running adapter return `Ok(())`.
    pub fn stop(&self) {
        self.push(Action::Stop);
    }

    /// Waits for the next command the running adapter received.
    pub async fn next_command(&self) -> Option<CommandEvent> {
        self.shared.commands_rx.lock().await.recv().await
    }

    fn push(&self, action: Action) {
        lock(&self.shared.actions).push_back(action);
        self.shared.wake.notify_one();
    }
}

impl Shared {
    /// Takes the next action; the guard is released before the action is executed.
    fn pop_action(&self) -> Option<Action> {
        lock(&self.actions).pop_front()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn not_running() -> Error {
    Error::Internal(String::from("the mock adapter is not running"))
}

/// An adapter instance of a [`MockControl`]; built by [`MockControl::adapter`].
pub struct MockAdapter {
    shared: Arc<Shared>,
}

impl core::fmt::Debug for MockAdapter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MockAdapter")
    }
}

/// Counts the end of a run, however it ends.
struct EndGuard(Arc<Shared>);

impl Drop for EndGuard {
    fn drop(&mut self) {
        self.0.status.send_modify(|status| status.ended += 1);
    }
}

impl Adapter for MockAdapter {
    /// Runs until the control says stop, fail or panic, or until shutdown.
    ///
    /// # Panics
    ///
    /// When the control asked for it with [`MockControl::panic`].
    fn run(self: Box<Self>, ctx: AdapterContext) -> BoxFuture<'static, Result<(), Error>> {
        Box::pin(async move {
            let shared = self.shared;
            *lock(&shared.context) = Some(ctx.clone());
            shared.status.send_modify(|status| status.runs += 1);
            let _guard = EndGuard(Arc::clone(&shared));
            let mut commands = ctx.commands();
            let mut shutdown = ctx.shutdown();
            loop {
                while let Some(action) = shared.pop_action() {
                    match action {
                        Action::AddDevice {
                            spec,
                            initial,
                            reply,
                        } => {
                            let _ = reply.send(ctx.devices().add(spec, initial).await);
                        }
                        Action::RemoveDevice { key, reply } => {
                            let _ = reply.send(ctx.devices().remove(&key).await);
                        }
                        Action::Set {
                            key,
                            part,
                            attribute,
                            value,
                            reply,
                        } => {
                            let result = match ctx.devices().get(&key) {
                                Some(device) => device
                                    .set(part, attribute, value)
                                    .await
                                    .map_err(Error::from),
                                None => {
                                    Err(Error::Bridge(matter_rs_bridge::Error::UnknownDevice(key)))
                                }
                            };
                            let _ = reply.send(result);
                        }
                        Action::Health(health) => ctx.set_health(health),
                        Action::Fail(error) => return Err(error),
                        Action::Panic(message) => panic!("{message}"),
                        Action::Hang => std::future::pending::<()>().await,
                        Action::Stop => return Ok(()),
                    }
                }
                // The shutdown signal and the end of the command stream come together; the
                // signal is checked first, and the stream's end counts as the signal too.
                tokio::select! {
                    biased;
                    () = &mut shutdown => {
                        shared.status.send_modify(|status| status.shutdowns += 1);
                        return Ok(());
                    }
                    command = commands.next() => match command {
                        Some(command) => {
                            let _ = shared.commands_tx.send(command);
                        }
                        None => {
                            shared.status.send_modify(|status| status.shutdowns += 1);
                            return Ok(());
                        }
                    },
                    () = shared.wake.notified() => {}
                }
            }
        })
    }
}
