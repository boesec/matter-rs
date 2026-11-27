"""The service binary ``matter-rs run`` as a device under test.

Unlike the engine example, the service has no line protocol: it is configured by a file, logs to
standard error and stops on ``SIGTERM``. :class:`ServiceDut` writes a configuration for it (the
harness passcode, its own port and discriminator, a temporary state directory), starts it, waits
for its ready line, and stops it with a signal so that a scenario can check the exit code and
restart it on the same state directory.

The binary comes from ``INTEROP_SERVICE_DUT`` when that names a file; otherwise
:func:`build_service_binary` runs ``cargo build --release -p matter-rs`` in the repository, which
is quick when the workflow's cargo cache already holds the dependencies.
"""

from __future__ import annotations

import logging
import os
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from harness.config import INTEROP_ROOT, DutSpec, HarnessConfig
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

REPOSITORY_ROOT = INTEROP_ROOT.parent.parent
SERVICE_READY_PATTERN = r"node is running"
# The service runs next to the matter.js light (5540/3840) and the engine example (5541/3841).
DEFAULT_SERVICE_PORT = 5542
DEFAULT_SERVICE_DISCRIMINATOR = 3842
# The stop timeout of the systemd unit; the service gives up on its own after 15 seconds.
STOP_TIMEOUT = 20.0
BUILD_TIMEOUT = 1500.0


@dataclass(frozen=True)
class ServiceBuild:
    """The service binary and how long it took to get it (0 when it was given)."""

    binary: Path
    seconds: float
    built: bool


def service_port(env: dict[str, str] | None = None) -> int:
    env = dict(os.environ if env is None else env)
    return int(env.get("INTEROP_SERVICE_DUT_PORT") or str(DEFAULT_SERVICE_PORT), 0)


def service_discriminator(env: dict[str, str] | None = None) -> int:
    env = dict(os.environ if env is None else env)
    return int(env.get("INTEROP_SERVICE_DISCRIMINATOR") or str(DEFAULT_SERVICE_DISCRIMINATOR), 0)


def cargo_target_dir(env: dict[str, str] | None = None) -> Path:
    env = dict(os.environ if env is None else env)
    configured = env.get("CARGO_TARGET_DIR")
    return Path(configured).resolve() if configured else REPOSITORY_ROOT / "target"


def build_service_binary(logger: logging.Logger, env: dict[str, str] | None = None) -> ServiceBuild:
    """The service binary: ``INTEROP_SERVICE_DUT`` if it names a file, else a fresh release build.

    Raises :class:`HarnessError` when neither works; the message says why.
    """
    env = dict(os.environ if env is None else env)
    given = env.get("INTEROP_SERVICE_DUT")
    if given:
        binary = Path(given).resolve()
        if not binary.is_file():
            raise HarnessError(f"INTEROP_SERVICE_DUT names {binary}, which is not a file")
        return ServiceBuild(binary, 0.0, built=False)
    cargo = shutil.which("cargo")
    if cargo is None:
        raise HarnessError("cargo is not on PATH; build matter-rs and name it with INTEROP_SERVICE_DUT")
    command = [cargo, "build", "--release", "-p", "matter-rs"]
    logger.info("service DUT: building with %s", " ".join(command))
    started = time.monotonic()
    result = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        timeout=BUILD_TIMEOUT,
    )
    seconds = time.monotonic() - started
    output = result.stdout.decode("utf-8", errors="replace")
    if result.returncode != 0:
        raise HarnessError(f"cargo build of matter-rs failed after {seconds:.0f} s:\n{output[-4000:]}")
    suffix = ".exe" if os.name == "nt" else ""
    binary = cargo_target_dir(env) / "release" / f"matter-rs{suffix}"
    if not binary.is_file():
        raise HarnessError(f"cargo build succeeded but {binary} does not exist")
    logger.info("service DUT: release build of matter-rs took %.1f s", seconds)
    return ServiceBuild(binary, seconds, built=True)


def service_config_text(config: HarnessConfig, state_dir: Path, port: int, discriminator: int) -> str:
    """The configuration file of the service DUT. It holds the passcode: keep it out of ``logs/``."""
    lines = [
        "[node]",
        f"passcode = {config.passcode}",
        f"discriminator = {discriminator}",
        'node_label = "matter-rs-interop"',
        "",
        "[network]",
    ]
    if config.engine_interface:
        lines.append(f'interface = "{config.engine_interface}"')
    lines += [
        f"port = {port}",
        "",
        "[state]",
        f"dir = '{state_dir}'",
        "",
        "[log]",
        'level = "info"',
        'format = "text"',
        "",
        "[bridge]",
        "",
    ]
    return "\n".join(lines)


class ServiceProcess(ManagedProcess):
    """A managed process that can be sent a signal."""

    def send_signal(self, number: int) -> None:
        if self._process is None:
            raise HarnessError(f"{self.name}: not started")
        self._logger.info("%s: sending signal %s", self.name, signal.Signals(number).name)
        self._process.send_signal(number)


class ServiceDut:
    """``matter-rs run`` on a configuration in ``work_dir`` and a state directory next to it."""

    def __init__(
        self,
        binary: Path,
        config: HarnessConfig,
        work_dir: Path,
        logs_dir: Path,
        redactor: Redactor,
        logger: logging.Logger,
        *,
        port: int = DEFAULT_SERVICE_PORT,
        discriminator: int = DEFAULT_SERVICE_DISCRIMINATOR,
    ) -> None:
        self.state_dir = work_dir / "state"
        self.config_path = work_dir / "config.toml"
        self.port = port
        self.discriminator = discriminator
        self._logs_dir = logs_dir
        self._redactor = redactor
        self._logger = logger
        self._starts = 0
        self.process: ServiceProcess | None = None
        work_dir.mkdir(parents=True, exist_ok=True)
        self.config_path.write_text(service_config_text(config, self.state_dir, port, discriminator), encoding="utf-8")
        if os.name != "nt":
            self.config_path.chmod(0o600)
        self.spec = DutSpec(
            name="service",
            command=(str(binary), "run", "--config", str(self.config_path)),
            ready_pattern=SERVICE_READY_PATTERN,
            ready_timeout=config.ready_timeout,
            endpoint=1,
        )

    def start(self) -> ServiceDut:
        """Start the service and wait for its ready line; stop it again when that fails."""
        self._starts += 1
        process = ServiceProcess(
            f"dut {self.spec.name}",
            self.spec.command,
            self._logs_dir / f"service-{self._starts}.log",
            self._redactor,
            self._logger,
        ).start()
        try:
            process.wait_for(self.spec.ready_pattern, self.spec.ready_timeout)
        except HarnessError:
            process.stop()
            raise
        self.process = process
        return self

    def running(self) -> ServiceProcess:
        if self.process is None:
            raise HarnessError("the service DUT is not running")
        return self.process

    def terminate(self, timeout: float = STOP_TIMEOUT) -> tuple[int, float, list[str]]:
        """Send ``SIGTERM`` and wait for the exit.

        Returns the exit code, the seconds the stop took and the (redacted) output lines.
        """
        process = self.running()
        started = time.monotonic()
        process.send_signal(signal.SIGTERM)
        try:
            code = process.wait(timeout)
            seconds = time.monotonic() - started
        finally:
            process.stop()
            self.process = None
        return code, seconds, process.lines

    def stop(self) -> None:
        """Stop the service if it runs (``SIGTERM``, killed after the grace period)."""
        if self.process is not None:
            self.process.stop(grace=STOP_TIMEOUT)
            self.process = None
