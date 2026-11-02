"""chip-tool from the connectedhomeip certification image, one container per command.

chip-tool keeps its fabric in ``--storage-directory``, which is a named Docker volume here, so
consecutive invocations share the fabric. The certification build is ``ipv6only`` with platform
mDNS, which is why every container gets the host network and the host's D-Bus socket (Avahi).

A subscription outlives a single command only in chip-tool's interactive mode: a plain
``subscribe`` command exits as soon as the subscription is established, while ``interactive
start`` keeps the subscriptions of the commands it ran and prints their reports until it is told
to quit. :class:`ChipToolInteractive` runs that mode in a long-lived container on the same fabric.
"""

from __future__ import annotations

import logging
import re
from pathlib import Path

from harness.config import HarnessConfig
from harness.docker import DBUS_CLIENT_SECURITY_OPTS, run_command
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

CHIP_TOOL = "/root/chip-tool"
PAIRING_SUCCESS = r"Device commissioning completed with success"
MANUAL_CODE_LINE = r"Manual pairing code: \[(\d+)\]"
SUBSCRIPTION_ESTABLISHED = r"Subscription established"
# chip-tool's data model logger prints a list as "<n> entries" followed by one "[i]: value" line
# per entry, and an attribute status as "Response Failure: IM Error ...".
LIST_HEADER = r"{name}:\s*(\d+) entries"
LIST_ENTRY = re.compile(r"\[\d+\]:\s*(\S+)")
RESPONSE_FAILURE = r"Response Failure: (.*)"
UNSUPPORTED_ENDPOINT = r"UNSUPPORTED_ENDPOINT"


def attribute_report(endpoint: int, cluster: int) -> str:
    """The pattern of chip-tool's report header for an attribute of ``cluster`` on ``endpoint``."""
    return rf"Endpoint: {endpoint} Cluster: 0x{cluster >> 16:04X}_{cluster & 0xFFFF:04X} Attribute 0x"


class ChipTool:
    def __init__(
        self,
        config: HarnessConfig,
        redactor: Redactor,
        logger: logging.Logger,
        logs_dir: Path,
        *,
        component: str = "chip-tool",
        discriminator: int | None = None,
    ) -> None:
        """A chip-tool with its own fabric in the volume named after ``component``.

        ``discriminator`` is the long discriminator :meth:`pair` filters on; the default is the
        configured one of the matter.js device.
        """
        self._config = config
        self._redactor = redactor
        self._logger = logger
        self._logs_dir = logs_dir
        self._counter = 0
        self.component = component
        self.volume = config.volume_name(component)
        self.discriminator = config.discriminator if discriminator is None else discriminator

    def run(self, label: str, args: tuple[str, ...], timeout: float, *, expect_success: bool = True) -> ManagedProcess:
        """Run one chip-tool command in a fresh container and return its collected output."""
        self._counter += 1
        name = self._config.container_name(f"{self.component}-{self._counter}")
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
                str(self.discriminator),
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

    def read(
        self,
        cluster: str,
        attribute: str,
        node_id: int,
        endpoint: int,
        *,
        timeout: float = 60.0,
        expect_success: bool = True,
    ) -> ManagedProcess:
        """Read one attribute (chip-tool's cluster and attribute names) and return the output."""
        return self.run(
            f"read-{cluster}-{attribute}",
            (cluster, "read", attribute, str(node_id), str(endpoint)),
            timeout,
            expect_success=expect_success,
        )

    def read_value(self, cluster: str, attribute: str, label: str, node_id: int, endpoint: int) -> str:
        """Read one scalar attribute and return the value chip-tool printed after ``label:``."""
        process = self.read(cluster, attribute, node_id, endpoint)
        return process.expect_pattern(rf"{label}:\s*(\S+)").group(1)

    def read_failure(self, cluster: str, attribute: str, node_id: int, endpoint: int) -> str:
        """Read one attribute that the device must refuse; returns chip-tool's failure text."""
        process = self.read(cluster, attribute, node_id, endpoint, expect_success=False)
        return process.expect_pattern(RESPONSE_FAILURE).group(1)

    def write(
        self,
        cluster: str,
        attribute: str,
        value: str,
        node_id: int,
        endpoint: int,
        *,
        timeout: float = 60.0,
    ) -> ManagedProcess:
        """Write one attribute; the exit code is not checked, the caller reads the output."""
        return self.run(
            f"write-{cluster}-{attribute}",
            (cluster, "write", attribute, value, str(node_id), str(endpoint)),
            timeout,
            expect_success=False,
        )

    def read_list(self, cluster: str, attribute: str, label: str, node_id: int, endpoint: int) -> list[str]:
        """Read one list attribute and return its entries as chip-tool printed them."""
        process = self.read(cluster, attribute, node_id, endpoint)
        header = re.compile(LIST_HEADER.format(name=label))
        lines = process.lines
        start = next((i for i, line in enumerate(lines) if header.search(line)), None)
        if start is None:
            raise HarnessError(
                f"chip-tool: no {label} list in the output.\nLast lines of {process.log_path.name}:\n{process.tail()}"
            )
        count = int(header.search(lines[start]).group(1))
        entries: list[str] = []
        for line in lines[start + 1 :]:
            match = LIST_ENTRY.search(line)
            if not match or len(entries) == count:
                break
            entries.append(match.group(1))
        if len(entries) != count:
            raise HarnessError(f"chip-tool: {label} announces {count} entries, {len(entries)} were printed")
        return entries

    def read_parts_list(self, node_id: int, endpoint: int) -> list[int]:
        """The ``PartsList`` of the Descriptor cluster on ``endpoint``."""
        return [int(entry) for entry in self.read_list("descriptor", "parts-list", "PartsList", node_id, endpoint)]

    def read_configuration_version(self, node_id: int) -> int:
        """The ``ConfigurationVersion`` of Basic Information on the root endpoint."""
        return int(self.read_value("basicinformation", "configuration-version", "ConfigurationVersion", node_id, 0))

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


class ChipToolInteractive:
    """One ``chip-tool interactive start`` session on the fabric of a :class:`ChipTool`.

    Commands are sent as lines; every report of every subscription the session holds is printed
    into one log, which the scenarios watch with :meth:`wait_for`.
    """

    def __init__(
        self,
        tool: ChipTool,
        config: HarnessConfig,
        redactor: Redactor,
        logger: logging.Logger,
        logs_dir: Path,
        *,
        label: str,
    ) -> None:
        self._tool = tool
        self._config = config
        self._redactor = redactor
        self._logger = logger
        self._logs_dir = logs_dir
        self._label = label
        self.process: ManagedProcess | None = None

    def start(self, timeout: float = 60.0) -> ChipToolInteractive:
        name = self._config.container_name(f"{self._tool.component}-interactive-{self._label}")
        command = run_command(
            self._config.chip_image,
            (CHIP_TOOL, "interactive", "start", "--storage-directory", "/state"),
            name=name,
            interactive=True,
            mounts=(
                (self._config.dbus_socket, self._config.dbus_socket, False),
                (self._tool.volume, "/state", False),
            ),
            security_opts=DBUS_CLIENT_SECURITY_OPTS,
        )
        self.process = ManagedProcess(
            f"chip-tool interactive {self._label}",
            command,
            self._logs_dir / f"chip-tool-interactive-{self._label}.log",
            self._redactor,
            self._logger,
            stdin=True,
            container_name=name,
        ).start()
        # The session is usable once the controller has loaded its fabric from the storage.
        self.process.wait_for(r"Fabric index|Loaded fabric|interactive", timeout)
        return self

    def _running(self) -> ManagedProcess:
        if self.process is None:
            raise HarnessError("the interactive chip-tool session is not running")
        return self.process

    def mark(self) -> int:
        return self._running().mark()

    def send(self, line: str) -> int:
        """Send one chip-tool command line; returns the output mark before it was sent."""
        process = self._running()
        since = process.mark()
        process.send(line)
        return since

    def wait_for(self, pattern: str | re.Pattern[str], timeout: float, since: int = 0) -> re.Match[str]:
        return self._running().wait_for(pattern, timeout, since)

    def subscribe(self, args: str, timeout: float = 60.0) -> int:
        """Run a subscribe command and wait until the subscription is established.

        Returns the output mark at the time the subscription was established, so that reports
        after the priming one can be searched from there.
        """
        since = self.send(args)
        self.wait_for(SUBSCRIPTION_ESTABLISHED, timeout, since)
        return self.mark()

    def subscribe_on_off(self, node_id: int, endpoint: int, *, min_interval: int = 0, max_interval: int = 10) -> int:
        return self.subscribe(f"onoff subscribe on-off {min_interval} {max_interval} {node_id} {endpoint}")

    def subscribe_all(self, node_id: int, *, min_interval: int = 0, max_interval: int = 10) -> int:
        """A wildcard subscription to every attribute of every cluster on every endpoint."""
        return self.subscribe(
            f"any subscribe-by-id 0xFFFFFFFF 0xFFFFFFFF {min_interval} {max_interval} {node_id} 0xFFFF"
        )

    def close(self) -> None:
        if self.process is None:
            return
        try:
            if self.process.returncode is None:
                self.process.send("quit")
                self.process.wait(30)
        except (HarnessError, OSError):
            pass
        finally:
            self.process.stop()
            self.process = None
