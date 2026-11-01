"""chip-tool from the connectedhomeip certification image, one container per command.

chip-tool keeps its fabric in ``--storage-directory``, which is a named Docker volume here, so
consecutive invocations share the fabric. The certification build is ``ipv6only`` with platform
mDNS, which is why every container gets the host network and the host's D-Bus socket (Avahi).
"""

from __future__ import annotations

import logging
from pathlib import Path

from harness.config import HarnessConfig
from harness.docker import DBUS_CLIENT_SECURITY_OPTS, run_command
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

CHIP_TOOL = "/root/chip-tool"
PAIRING_SUCCESS = r"Device commissioning completed with success"
MANUAL_CODE_LINE = r"Manual pairing code: \[(\d+)\]"


class ChipTool:
    def __init__(self, config: HarnessConfig, redactor: Redactor, logger: logging.Logger, logs_dir: Path) -> None:
        self._config = config
        self._redactor = redactor
        self._logger = logger
        self._logs_dir = logs_dir
        self._counter = 0
        self.volume = config.volume_name("chip-tool")

    def run(self, label: str, args: tuple[str, ...], timeout: float, *, expect_success: bool = True) -> ManagedProcess:
        """Run one chip-tool command in a fresh container and return its collected output."""
        self._counter += 1
        name = self._config.container_name(f"chip-tool-{self._counter}")
        command = run_command(
            self._config.chip_image,
            (CHIP_TOOL, *args, "--storage-directory", "/state"),
            name=name,
            mounts=(
                (self._config.dbus_socket, self._config.dbus_socket, False),
                (self.volume, "/state", False),
            ),
            security_opts=DBUS_CLIENT_SECURITY_OPTS,
        )
        process = ManagedProcess(
            f"chip-tool {label}",
            command,
            self._logs_dir / f"chip-tool-{self._counter:02d}-{label}.log",
            self._redactor,
            self._logger,
            container_name=name,
        )
        process.run(timeout, expect_success=expect_success)
        return process

    def pair(self, node_id: int, timeout: float = 150.0) -> None:
        """Commission the device under test over IP, by passcode and long discriminator."""
        process = self.run(
            "pairing",
            (
                "pairing",
                "onnetwork-long",
                str(node_id),
                str(self._config.passcode),
                str(self._config.discriminator),
            ),
            timeout,
        )
        process.expect_pattern(PAIRING_SUCCESS)

    def toggle(self, node_id: int, endpoint: int, timeout: float = 60.0) -> None:
        self.run("toggle", ("onoff", "toggle", str(node_id), str(endpoint)), timeout)

    def read_on_off(self, node_id: int, endpoint: int, timeout: float = 60.0) -> bool:
        process = self.run("read-onoff", ("onoff", "read", "on-off", str(node_id), str(endpoint)), timeout)
        match = process.expect_pattern(r"OnOff:\s*(TRUE|FALSE)")
        return match.group(1) == "TRUE"

    def open_commissioning_window(
        self,
        node_id: int,
        discriminator: int,
        *,
        window_seconds: int = 600,
        timeout: float = 60.0,
    ) -> str:
        """Open an enhanced commissioning window and return its manual pairing code.

        The code is a fresh secret: it is captured from chip-tool's raw output and registered with
        the redactor before the line reaches any log.
        """
        capture = self._redactor.capture(MANUAL_CODE_LINE)
        # A window left open by an earlier step (for example a second administrator that never
        # commissioned) would make the device answer Busy. Revoking first costs one timed
        # command and fails harmlessly with WindowNotOpen when there is nothing to revoke.
        self.run(
            "revoke-window",
            (
                "administratorcommissioning",
                "revoke-commissioning",
                str(node_id),
                "0",
                "--timedInteractionTimeoutMs",
                "1000",
            ),
            timeout,
            expect_success=False,
        )
        process = self.run(
            "open-window",
            (
                "pairing",
                "open-commissioning-window",
                str(node_id),
                "1",
                str(window_seconds),
                "1000",
                str(discriminator),
            ),
            timeout,
        )
        if capture.value is None:
            raise HarnessError(
                "chip-tool open-window: no manual pairing code in its output.\n"
                f"Last lines of {process.log_path.name}:\n{process.tail()}"
            )
        return capture.value
