"""The engine example ``onoff_node`` as a device under test, driven through its standard input.

The example answers every command line with ``ok`` or ``err <reason>`` and prints the engine's
events as ``event <kind> ...`` lines. :class:`EngineDut` sends commands, waits for their answers
and for events, and restarts the process on the same state directory for the restart scenario.
The protocol itself is documented in the example's source (``crates/engine-rsmatter/examples``).
"""

from __future__ import annotations

import logging
import re
from pathlib import Path

from harness.config import DutSpec
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

ANSWER = re.compile(r"^(ok|err .*)$")
DEFAULT_COMMAND_TIMEOUT = 30.0


class EngineDut:
    """A running ``onoff_node`` with its line protocol."""

    def __init__(self, spec: DutSpec, logs_dir: Path, redactor: Redactor, logger: logging.Logger) -> None:
        if not spec.stdin:
            raise ValueError("the engine DUT takes commands on its standard input; the spec must say so")
        self.spec = spec
        self._logs_dir = logs_dir
        self._redactor = redactor
        self._logger = logger
        self.process: ManagedProcess | None = None

    @property
    def endpoint(self) -> int:
        return self.spec.endpoint

    # -- lifecycle -------------------------------------------------------------------------

    def start(self) -> EngineDut:
        """Start the process and wait for its ready line; stop it again when that fails."""
        process = ManagedProcess(
            f"dut {self.spec.name}",
            self.spec.command,
            self._logs_dir / f"{self.spec.name}.log",
            self._redactor,
            self._logger,
            stdin=True,
        ).start()
        try:
            process.wait_for(self.spec.ready_pattern, self.spec.ready_timeout)
        except HarnessError:
            process.stop()
            raise
        self.process = process
        return self

    def stop(self, timeout: float = DEFAULT_COMMAND_TIMEOUT) -> None:
        """Ask the node to quit, wait for its stop event and its exit; kill it if it lingers."""
        process = self.process
        if process is None:
            return
        self.process = None
        try:
            if process.returncode is None:
                since = process.mark()
                process.send("quit")
                process.wait_for(r"^event Stopped", timeout, since)
                process.wait(timeout)
        except (HarnessError, OSError):
            pass
        finally:
            process.stop()

    def restart(self) -> EngineDut:
        """Stop the node and start it again on the same state directory."""
        self.stop()
        return self.start()

    # -- protocol --------------------------------------------------------------------------

    def _running(self) -> ManagedProcess:
        if self.process is None:
            raise HarnessError("the engine DUT is not running")
        return self.process

    def command(self, line: str, timeout: float = DEFAULT_COMMAND_TIMEOUT) -> str:
        """Send one command and return its answer line (``ok`` or ``err <reason>``)."""
        process = self._running()
        since = process.mark()
        process.send(line)
        match = process.wait_for(ANSWER, timeout, since)
        return match.group(1)

    def ok(self, line: str, timeout: float = DEFAULT_COMMAND_TIMEOUT) -> None:
        """Send one command that must be answered ``ok``."""
        answer = self.command(line, timeout)
        if answer != "ok":
            process = self._running()
            raise HarnessError(
                f"engine DUT refused {line!r}: {answer}\nLast lines of {process.log_path.name}:\n{process.tail()}"
            )

    def err(self, line: str, timeout: float = DEFAULT_COMMAND_TIMEOUT) -> str:
        """Send one command that must be refused; returns the reason."""
        answer = self.command(line, timeout)
        if not answer.startswith("err "):
            process = self._running()
            raise HarnessError(
                f"engine DUT accepted {line!r} although it must refuse it\n"
                f"Last lines of {process.log_path.name}:\n{process.tail()}"
            )
        return answer[4:]

    def mark(self) -> int:
        """The current line count of the output; pass it to :meth:`wait_event` as ``since``."""
        return self._running().mark()

    def wait_event(self, pattern: str, timeout: float, since: int = 0) -> re.Match[str]:
        """Wait for an ``event ...`` line matching ``pattern`` (a regular expression)."""
        return self._running().wait_for(pattern, timeout, since)

    def add(self, endpoint: int, device_type: str, parent: int | None = None) -> None:
        line = f"add {endpoint} {device_type}" + (f" {parent}" if parent is not None else "")
        self.ok(line)

    def remove(self, endpoint: int) -> None:
        self.ok(f"remove {endpoint}")

    def set(self, endpoint: int, cluster: int, attribute: int, value: str) -> None:
        self.ok(f"set {endpoint} 0x{cluster:04X} 0x{attribute:04X} {value}")

    def reachable(self, endpoint: int, reachable: bool) -> None:
        self.ok(f"reachable {endpoint} {'true' if reachable else 'false'}")
