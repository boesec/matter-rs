//! The stop signals of the service, through tokio's signal handling.
//!
//! On Unix these are `SIGTERM` (what systemd and `docker stop` send) and `SIGINT` (Ctrl-C in a
//! terminal). On Windows they are Ctrl-C, Ctrl-Break, closing the console window, and the log-off
//! and shut-down notifications of the console. The handlers are installed before the node starts,
//! so a signal that arrives during start-up stops the node as soon as it runs instead of killing
//! the process.

use std::io;

/// The installed stop signal handlers.
pub(crate) struct StopSignals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
    #[cfg(windows)]
    ctrl_logoff: tokio::signal::windows::CtrlLogoff,
}

impl StopSignals {
    /// Installs the handlers. Must be called inside the tokio runtime.
    ///
    /// # Errors
    ///
    /// The operating system's error when a handler cannot be installed.
    #[cfg(unix)]
    pub(crate) fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }

    /// Installs the handlers. Must be called inside the tokio runtime.
    ///
    /// # Errors
    ///
    /// The operating system's error when a handler cannot be installed.
    #[cfg(windows)]
    pub(crate) fn install() -> io::Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            ctrl_c: windows::ctrl_c()?,
            ctrl_break: windows::ctrl_break()?,
            ctrl_close: windows::ctrl_close()?,
            ctrl_shutdown: windows::ctrl_shutdown()?,
            ctrl_logoff: windows::ctrl_logoff()?,
        })
    }

    /// Waits for the next stop signal and returns its name.
    #[cfg(unix)]
    pub(crate) async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.interrupt.recv() => "SIGINT",
        }
    }

    /// Waits for the next stop signal and returns its name.
    #[cfg(windows)]
    pub(crate) async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "Ctrl-C",
            _ = self.ctrl_break.recv() => "Ctrl-Break",
            _ = self.ctrl_close.recv() => "console close",
            _ = self.ctrl_shutdown.recv() => "system shutdown",
            _ = self.ctrl_logoff.recv() => "log-off",
        }
    }
}
